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
//! - an answer waits up to [`MemoryBudget::ANSWER_WAIT`], 5 s, since it
//!   holds the file's decoded record meanwhile.
//!
//! One larger than the whole budget can never succeed: it fails at once
//! with a final `413 exceeds_memory_budget`, which clients don't retry.
//!
//! Reservations are taken before any permit of the CPU pools, so a request
//! never holds a `validate` or `commit` permit while it waits for memory.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::CasError;

/// The budget's accounting unit, in bytes. A semaphore takes at most
/// `u32::MAX` permits at once, so counting in KiB lets one reservation be
/// up to 4 TiB.
const UNIT: u64 = 1024;

/// A pool of bytes that requests reserve before buffering. Cloning shares
/// the pool.
#[derive(Debug, Clone)]
pub struct MemoryBudget {
    units: Arc<Semaphore>,
    bytes: u64,
    upload_wait: Duration,
    answer_wait: Duration,
}

impl MemoryBudget {
    /// The least default budget, 1 GiB: room for 15 xorb uploads of the
    /// largest size in flight at once, or about 25 full answers for
    /// 100k-term files. The server's default is a quarter of the host's
    /// memory, but never less than this.
    pub const DEFAULT_BYTES: u64 = 1 << 30;

    /// The smallest budget a server may be configured with: room for three
    /// of the largest uploads and a few answers for 100k-term files. Below
    /// about 600 MB, a batch reconstruction of [`crate::MAX_BATCH_TERMS`]
    /// terms is refused as over the whole budget.
    pub const MIN_BYTES: u64 = 256 << 20;

    /// The largest budget a server may be configured with.
    pub const MAX_BYTES: u64 = 1 << 40;

    /// How long an upload waits for bytes to come free before it is
    /// refused with `503 memory_busy`: long enough to queue a client's
    /// whole burst of uploads, well inside its 300 s read timeout (S10 §4).
    pub const UPLOAD_WAIT: Duration = Duration::from_mins(2);

    /// How long a reconstruction answer waits for bytes to come free
    /// before it is refused with `503 memory_busy`.
    pub const ANSWER_WAIT: Duration = Duration::from_secs(5);

    /// A budget of `bytes` (rounded up to a whole KiB, at most 4 TiB) with
    /// the default waits.
    #[must_use]
    pub fn new(bytes: u64) -> Self {
        let units = bytes.div_ceil(UNIT).min(u64::from(u32::MAX));
        Self {
            units: Arc::new(Semaphore::new(to_usize(units))),
            bytes: units * UNIT,
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
        self.bytes
    }

    /// The bytes not reserved right now.
    #[must_use]
    pub fn available(&self) -> u64 {
        self.units.available_permits() as u64 * UNIT
    }

    /// Reserves `bytes` for an upload body, waiting at most the upload
    /// wait ([`MemoryBudget::reserve`]).
    pub(crate) async fn reserve_upload(&self, bytes: u64) -> Result<Reservation, CasError> {
        self.reserve(bytes, self.upload_wait).await
    }

    /// Reserves `bytes` for a reconstruction answer, waiting at most the
    /// answer wait ([`MemoryBudget::reserve`]).
    pub(crate) async fn reserve_answer(&self, bytes: u64) -> Result<Reservation, CasError> {
        self.reserve(bytes, self.answer_wait).await
    }

    /// Reserves `bytes`, waiting at most `wait`; reservations are granted
    /// in the order they were asked for:
    /// - [`CasError::ExceedsMemoryBudget`] at once if `bytes` exceeds the
    ///   whole budget
    /// - [`CasError::MemoryBusy`] if the wait passes first
    async fn reserve(&self, bytes: u64, wait: Duration) -> Result<Reservation, CasError> {
        let units = bytes.div_ceil(UNIT);
        let Some(units) = u32::try_from(units)
            .ok()
            .filter(|n| u64::from(*n) * UNIT <= self.bytes)
        else {
            return Err(CasError::ExceedsMemoryBudget {
                needed: bytes,
                budget: self.bytes,
            });
        };
        let permit = tokio::time::timeout(wait, Arc::clone(&self.units).acquire_many_owned(units))
            .await
            .map_err(|_| CasError::MemoryBusy)?
            .map_err(CasError::internal)?;
        Ok(Reservation(permit))
    }
}

/// Bytes reserved from a [`MemoryBudget`], returned to it when dropped.
#[derive(Debug)]
pub(crate) struct Reservation(OwnedSemaphorePermit);

impl Reservation {
    /// Returns what is reserved beyond `bytes` to the budget. Never grows
    /// the reservation.
    pub(crate) fn shrink_to(&mut self, bytes: u64) {
        let keep = to_usize(bytes.div_ceil(UNIT));
        let held = self.0.num_permits();
        if keep < held {
            drop(self.0.split(held - keep));
        }
    }
}

/// `n` units, which [`MemoryBudget::new`] keeps within `u32`, as a count.
fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
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
}
