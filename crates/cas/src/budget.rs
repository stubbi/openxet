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
//! A reservation waits at most the budget's wait, then fails `503
//! memory_busy` with `Retry-After: 1`, which stock clients retry with
//! backoff (S10 "Status codes and retries"). One larger than the whole
//! budget can never succeed and fails at once.
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
    wait: Duration,
}

impl MemoryBudget {
    /// The default budget, 1 GiB: room for 15 xorb uploads of the largest
    /// size in flight at once, or about 16 full answers for 100k-term
    /// files, while staying well inside the memory of a small server.
    pub const DEFAULT_BYTES: u64 = 1 << 30;

    /// The smallest budget a server may be configured with: room for three
    /// of the largest uploads and a few answers for 100k-term files. Below
    /// about 600 MB, a batch reconstruction of [`crate::MAX_BATCH_TERMS`]
    /// terms is refused as over the whole budget.
    pub const MIN_BYTES: u64 = 256 << 20;

    /// The largest budget a server may be configured with.
    pub const MAX_BYTES: u64 = 1 << 40;

    /// How long a reservation waits for bytes to come free before the
    /// request is refused with `503 memory_busy`: long enough to ride out a
    /// burst of uploads, short against a client's 300 s read timeout.
    pub const DEFAULT_WAIT: Duration = Duration::from_secs(5);

    /// A budget of `bytes` (rounded up to a whole KiB, at most 4 TiB)
    /// whose reservations wait at most `wait`.
    #[must_use]
    pub fn new(bytes: u64, wait: Duration) -> Self {
        let units = bytes.div_ceil(UNIT).min(u64::from(u32::MAX));
        Self {
            units: Arc::new(Semaphore::new(to_usize(units))),
            bytes: units * UNIT,
            wait,
        }
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

    /// Reserves `bytes`, waiting at most the budget's wait:
    /// - [`CasError::OverMemoryBudget`] at once if `bytes` exceeds the
    ///   whole budget
    /// - [`CasError::MemoryBusy`] if the wait passes first
    pub(crate) async fn reserve(&self, bytes: u64) -> Result<Reservation, CasError> {
        let units = bytes.div_ceil(UNIT);
        let Some(units) = u32::try_from(units)
            .ok()
            .filter(|n| u64::from(*n) * UNIT <= self.bytes)
        else {
            return Err(CasError::OverMemoryBudget {
                needed: bytes,
                budget: self.bytes,
            });
        };
        let permit =
            tokio::time::timeout(self.wait, Arc::clone(&self.units).acquire_many_owned(units))
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
        let budget = MemoryBudget::new(4 * MIB, Duration::from_secs(5));
        let first = budget.reserve(3 * MIB).await.unwrap();
        assert_eq!(budget.available(), MIB);
        drop(first);
        assert_eq!(budget.available(), 4 * MIB);
    }

    #[tokio::test]
    async fn a_reservation_that_does_not_fit_in_time_is_memory_busy() {
        let wait = Duration::from_millis(200);
        let budget = MemoryBudget::new(4 * MIB, wait);
        let _held = budget.reserve(3 * MIB).await.unwrap();

        let started = std::time::Instant::now();
        let refused = budget.reserve(2 * MIB).await.unwrap_err();
        assert!(matches!(refused, CasError::MemoryBusy), "{refused:?}");
        assert!(started.elapsed() >= wait);
    }

    #[tokio::test]
    async fn a_reservation_waits_for_bytes_to_come_free() {
        let budget = MemoryBudget::new(4 * MIB, Duration::from_secs(5));
        let held = budget.reserve(3 * MIB).await.unwrap();
        let waiting = tokio::spawn({
            let budget = budget.clone();
            async move { budget.reserve(2 * MIB).await.map(|_| ()) }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(held);
        waiting.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_reservation_over_the_whole_budget_fails_at_once() {
        let budget = MemoryBudget::new(4 * MIB, Duration::from_mins(1));
        let started = std::time::Instant::now();
        let refused = budget.reserve(4 * MIB + 1).await.unwrap_err();
        assert!(
            matches!(refused, CasError::OverMemoryBudget { needed, budget: whole }
                if needed == 4 * MIB + 1 && whole == 4 * MIB),
            "{refused:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(budget.reserve(u64::MAX).await.is_err());
    }

    #[tokio::test]
    async fn shrinking_returns_the_excess_and_never_grows() {
        let budget = MemoryBudget::new(4 * MIB, Duration::from_secs(5));
        let mut held = budget.reserve(3 * MIB).await.unwrap();
        held.shrink_to(MIB + 1);
        assert_eq!(budget.available(), 4 * MIB - (MIB + UNIT));
        held.shrink_to(3 * MIB);
        assert_eq!(budget.available(), 4 * MIB - (MIB + UNIT));
        drop(held);
        assert_eq!(budget.available(), 4 * MIB);
    }
}
