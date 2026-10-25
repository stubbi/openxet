//! The process-wide memory budget (ruling M1-R40).
//!
//! The permit pools bound the CPU work running at once, not the bytes
//! waiting for it or for a client: an upload body is buffered before it
//! waits for a `validate` permit, and a built reconstruction answer waits
//! for its client to read it. Each of these reserves its bytes here first,
//! so what the process holds for requests is bounded however many clients
//! connect, upload slowly or never read:
//! - an upload body, by its `Content-Length` (by its route's limit when it
//!   has none), before a byte of it is read, until the request ends; a
//!   `/v2/shards` upload until its detached work ends
//! - a reconstruction answer, by the bound its buffers are allocated to
//!   before it is built, then by the bytes it holds once built, until it
//!   has been sent, dropped or overrun its send deadline
//!
//! A reservation that doesn't fit waits for bytes to come free, then fails
//! `503 memory_busy` with `Retry-After: 1`, which stock clients retry with
//! backoff (S10 "Status codes and retries"):
//! - an upload waits up to [`MemoryBudget::UPLOAD_WAIT`], 120 s, its body
//!   unread. A stock client sends up to 64 xorbs at once; past the budget
//!   they queue here, rather than fail into retries the client backs off
//!   from for seconds. A waiting upload holds only its connection, which
//!   `server.max_connections` bounds.
//! - an answer takes free bytes at once, past queued uploads; one that
//!   doesn't fit waits up to [`MemoryBudget::ANSWER_WAIT`], 5 s.
//!
//! One larger than the whole budget can never succeed: it fails at once
//! with a final `413 exceeds_memory_budget`, which clients don't retry. One
//! answer may reserve at most an eighth of the budget, or it fails at once
//! with a final `413 answer_too_large`.
//!
//! Reservations are taken before any permit of the CPU pools, so a request
//! never holds a `validate` or `commit` permit while it waits for memory.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

use crate::CasError;

/// The budget's accounting unit, in bytes.
const UNIT: u64 = 1024;

/// A pool of bytes that requests reserve before buffering. Cloning shares
/// the pool.
///
/// Uploads queue for it in arrival order: a small upload that would fit
/// waits behind a large one that came first, so no upload is starved.
/// Answers don't queue behind uploads: one that fits in the free bytes
/// takes them at once, and only one that doesn't waits, with whatever
/// upload is at the head of the queue, for bytes to come free (review I-2
/// of ruling M1-R40). Otherwise a stock client's queued uploads would make
/// every download on the node wait out its answer wait and fail.
#[derive(Debug, Clone)]
pub struct MemoryBudget {
    pool: Arc<Pool>,
    upload_wait: Duration,
    answer_wait: Duration,
}

#[derive(Debug)]
struct Pool {
    /// The whole budget, in units.
    total: u64,
    /// The units reserved now.
    used: Mutex<u64>,
    /// Woken whenever units are returned.
    freed: Notify,
    /// Held by the upload at the head of the queue while it waits for
    /// room; tokio's mutex is fair, so uploads take turns in arrival order.
    upload_turn: tokio::sync::Mutex<()>,
}

impl Pool {
    /// Takes `units` if they fit now.
    fn try_take(&self, units: u64) -> bool {
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        let fits = *used + units <= self.total;
        if fits {
            *used += units;
        }
        fits
    }

    /// Takes `units` once they fit.
    async fn take(&self, units: u64) {
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            // Registered before the check, so a release in between wakes it.
            freed.as_mut().enable();
            if self.try_take(units) {
                return;
            }
            freed.await;
        }
    }

    fn give_back(&self, units: u64) {
        if units == 0 {
            return;
        }
        *self.used.lock().unwrap_or_else(PoisonError::into_inner) -= units;
        self.freed.notify_waiters();
    }
}

impl MemoryBudget {
    /// The budget where the host's memory can't be read, 1 GiB: room for
    /// 15 xorb uploads of the largest size in flight at once, or about 25
    /// full answers for 100k-term files.
    pub const DEFAULT_BYTES: u64 = 1 << 30;

    /// The smallest budget a server may be configured with: room for three
    /// of the largest uploads and a few answers for 100k-term files.
    pub const MIN_BYTES: u64 = 256 << 20;

    /// The largest budget a server may be configured with.
    pub const MAX_BYTES: u64 = 1 << 40;

    /// How long an upload waits for bytes to come free before it is
    /// refused with `503 memory_busy`: long enough to queue a client's
    /// whole burst of uploads, well inside its 300 s read timeout (S10 §4).
    pub const UPLOAD_WAIT: Duration = Duration::from_mins(2);

    /// How long a reconstruction answer that doesn't fit now waits for
    /// bytes to come free before it is refused with `503 memory_busy`.
    pub const ANSWER_WAIT: Duration = Duration::from_secs(5);

    /// The share of the budget one answer may reserve: an eighth, so one
    /// reader can't hold most of it with a few unread answers (review I-5
    /// of ruling M1-R40).
    const ANSWER_SHARE: u64 = 8;

    /// A budget of `bytes`, rounded up to a whole KiB, with the default
    /// waits.
    #[must_use]
    pub fn new(bytes: u64) -> Self {
        Self {
            pool: Arc::new(Pool {
                total: bytes.div_ceil(UNIT),
                used: Mutex::new(0),
                freed: Notify::new(),
                upload_turn: tokio::sync::Mutex::new(()),
            }),
            upload_wait: Self::UPLOAD_WAIT,
            answer_wait: Self::ANSWER_WAIT,
        }
    }

    /// The same budget, with uploads waiting at most `upload` and answers
    /// at most `answer`.
    #[must_use]
    pub fn with_waits(mut self, upload: Duration, answer: Duration) -> Self {
        self.upload_wait = upload;
        self.answer_wait = answer;
        self
    }

    /// The whole budget, in bytes.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.pool.total * UNIT
    }

    /// The bytes not reserved right now, whether or not requests wait.
    #[must_use]
    pub fn available(&self) -> u64 {
        let used = *self
            .pool
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (self.pool.total - used) * UNIT
    }

    /// The most one answer may reserve: an eighth of the budget.
    #[must_use]
    pub fn answer_limit(&self) -> u64 {
        self.bytes() / Self::ANSWER_SHARE
    }

    /// Reserves `bytes` for an upload body, in arrival order among uploads,
    /// waiting at most the upload wait:
    /// - [`CasError::ExceedsMemoryBudget`] at once if `bytes` exceeds the
    ///   whole budget
    /// - [`CasError::MemoryBusy`] if the wait passes first
    pub(crate) async fn reserve_upload(&self, bytes: u64) -> Result<Reservation, CasError> {
        let units = self.units_within(bytes)?;
        let pool = &self.pool;
        tokio::time::timeout(self.upload_wait, async {
            let _turn = pool.upload_turn.lock().await;
            pool.take(units).await;
        })
        .await
        .map_err(|_| CasError::MemoryBusy)?;
        Ok(self.reservation(units))
    }

    /// Reserves `bytes` for a read request's answer (or its body): at once
    /// if they fit now, past any queued upload; otherwise waiting at most
    /// the answer wait:
    /// - [`CasError::AnswerTooLarge`] at once if `bytes` exceeds
    ///   [`MemoryBudget::answer_limit`]
    /// - [`CasError::MemoryBusy`] if the wait passes first
    ///
    /// The Hub's `resolve` and file gateway reserve their reads here too,
    /// so they share the CAS's pool.
    ///
    /// # Errors
    ///
    /// As listed.
    pub async fn reserve_answer(&self, bytes: u64) -> Result<Reservation, CasError> {
        self.check_answer(bytes)?;
        let units = bytes.div_ceil(UNIT);
        if !self.pool.try_take(units) {
            tokio::time::timeout(self.answer_wait, self.pool.take(units))
                .await
                .map_err(|_| CasError::MemoryBusy)?;
        }
        Ok(self.reservation(units))
    }

    /// [`CasError::AnswerTooLarge`] if one answer of `bytes` would exceed
    /// [`MemoryBudget::answer_limit`].
    ///
    /// # Errors
    ///
    /// As described.
    pub fn check_answer(&self, bytes: u64) -> Result<(), CasError> {
        let limit = self.answer_limit();
        if bytes > limit {
            return Err(CasError::AnswerTooLarge {
                needed: bytes,
                limit,
            });
        }
        Ok(())
    }

    fn units_within(&self, bytes: u64) -> Result<u64, CasError> {
        let units = bytes.div_ceil(UNIT);
        if units > self.pool.total {
            return Err(CasError::ExceedsMemoryBudget {
                needed: bytes,
                budget: self.bytes(),
            });
        }
        Ok(units)
    }

    fn reservation(&self, units: u64) -> Reservation {
        Reservation {
            pool: Arc::clone(&self.pool),
            units,
        }
    }
}

/// Bytes reserved from a [`MemoryBudget`], returned to it when dropped.
#[derive(Debug)]
pub struct Reservation {
    pool: Arc<Pool>,
    units: u64,
}

impl Reservation {
    /// Returns what is reserved beyond `bytes` to the budget. Never grows
    /// the reservation.
    pub fn shrink_to(&mut self, bytes: u64) {
        let keep = bytes.div_ceil(UNIT);
        if keep < self.units {
            self.pool.give_back(self.units - keep);
            self.units = keep;
        }
    }

    /// Adds `other`, from the same budget, to this reservation.
    pub(crate) fn merge(&mut self, mut other: Reservation) {
        debug_assert!(Arc::ptr_eq(&self.pool, &other.pool));
        self.units += std::mem::take(&mut other.units);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.pool.give_back(self.units);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const MIB: u64 = 1 << 20;

    #[tokio::test]
    async fn a_reservation_holds_its_bytes_until_dropped() {
        let budget = MemoryBudget::new(4 * MIB);
        let first = budget.reserve_upload(3 * MIB).await.unwrap();
        assert_eq!(budget.available(), MIB);
        drop(first);
        assert_eq!(budget.available(), 4 * MIB);
    }

    #[tokio::test]
    async fn a_reservation_that_does_not_fit_in_time_is_memory_busy() {
        let wait = Duration::from_millis(200);
        let budget = MemoryBudget::new(4 * MIB).with_waits(wait, wait);
        let _held = budget.reserve_upload(3 * MIB).await.unwrap();

        let started = std::time::Instant::now();
        let refused = budget.reserve_upload(2 * MIB).await.unwrap_err();
        assert!(matches!(refused, CasError::MemoryBusy), "{refused:?}");
        assert!(started.elapsed() >= wait);
    }

    #[tokio::test]
    async fn a_reservation_waits_for_bytes_to_come_free() {
        let budget = MemoryBudget::new(4 * MIB);
        let held = budget.reserve_upload(3 * MIB).await.unwrap();
        let waiting = tokio::spawn({
            let budget = budget.clone();
            async move { budget.reserve_upload(2 * MIB).await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(held);
        waiting.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_reservation_over_the_whole_budget_fails_at_once() {
        let budget = MemoryBudget::new(4 * MIB);
        let started = std::time::Instant::now();
        let refused = budget.reserve_upload(4 * MIB + 1).await.unwrap_err();
        assert!(
            matches!(refused, CasError::ExceedsMemoryBudget { needed, budget: whole }
                if needed == 4 * MIB + 1 && whole == 4 * MIB),
            "{refused:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(budget.reserve_upload(u64::MAX).await.is_err());
    }

    #[tokio::test]
    async fn shrinking_returns_the_excess_and_never_grows() {
        let budget = MemoryBudget::new(4 * MIB);
        let mut held = budget.reserve_upload(3 * MIB).await.unwrap();
        held.shrink_to(MIB + 1);
        assert_eq!(budget.available(), 4 * MIB - (MIB + UNIT));
        held.shrink_to(3 * MIB);
        assert_eq!(budget.available(), 4 * MIB - (MIB + UNIT));
        drop(held);
        assert_eq!(budget.available(), 4 * MIB);
    }

    #[tokio::test]
    async fn an_answer_that_fits_skips_queued_uploads() {
        // Review I-2 of M1-R40: queued uploads must not make reads wait. An
        // answer that fits in the free bytes now takes them at once, and
        // `available` counts bytes free though an upload waits (M-5).
        let budget =
            MemoryBudget::new(4 * MIB).with_waits(Duration::from_mins(1), Duration::from_mins(1));
        let _held = budget.reserve_upload(3 * MIB).await.unwrap();
        let queued = tokio::spawn({
            let budget = budget.clone();
            async move { budget.reserve_upload(2 * MIB).await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!queued.is_finished());
        assert_eq!(budget.available(), MIB);

        let answer = tokio::time::timeout(Duration::from_secs(1), budget.reserve_answer(MIB / 2))
            .await
            .expect("the answer waited behind the queued upload");
        assert!(answer.is_ok());
        assert_eq!(budget.available(), MIB / 2);
        queued.abort();
    }

    #[tokio::test]
    async fn uploads_are_granted_in_arrival_order() {
        // Among uploads the queue is fair: a small one that would fit waits
        // behind a large one that came first, so large uploads are never
        // starved.
        let budget = MemoryBudget::new(4 * MIB);
        let held = budget.reserve_upload(3 * MIB).await.unwrap();
        let large = tokio::spawn({
            let budget = budget.clone();
            async move { budget.reserve_upload(2 * MIB).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let small = tokio::spawn({
            let budget = budget.clone();
            async move { budget.reserve_upload(MIB).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!small.is_finished(), "the small upload jumped the queue");

        drop(held);
        let large = large.await.unwrap();
        let small = small.await.unwrap();
        assert_eq!(budget.available(), MIB);
        drop((large, small));
        assert_eq!(budget.available(), 4 * MIB);
    }
}
