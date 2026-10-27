//! The process-wide pool of helper threads that read a large `tree` page's
//! blobs beside the request's own blocking thread. Each request takes what
//! is free without waiting and reads the rest itself, so under load pages
//! get slower, never more threads: at most [`HelperPermits::limit`] helpers
//! run at once in the process, whatever the number of requests.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Helper-thread permits shared by every request of one engine.
#[derive(Debug)]
pub(crate) struct HelperPermits {
    limit: usize,
    in_use: AtomicUsize,
    /// The most ever in use at once, for the test that bounds it.
    #[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
    peak: AtomicUsize,
}

/// Permits taken from a [`HelperPermits`], returned on drop.
#[derive(Debug)]
pub(crate) struct Taken<'a> {
    pool: &'a HelperPermits,
    count: usize,
}

impl Taken<'_> {
    /// How many permits were taken.
    pub(crate) fn count(&self) -> usize {
        self.count
    }
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        self.pool.in_use.fetch_sub(self.count, Ordering::AcqRel);
    }
}

impl HelperPermits {
    /// A pool of one permit per CPU this process may use, at least one.
    pub(crate) fn per_cpu() -> Self {
        Self::new(std::thread::available_parallelism().map_or(1, usize::from))
    }

    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            in_use: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    /// Takes up to `wanted` permits without waiting: as many as are free.
    pub(crate) fn take(&self, wanted: usize) -> Taken<'_> {
        let mut current = self.in_use.load(Ordering::Acquire);
        loop {
            let count = wanted.min(self.limit.saturating_sub(current));
            if count == 0 {
                return Taken {
                    pool: self,
                    count: 0,
                };
            }
            match self.in_use.compare_exchange_weak(
                current,
                current + count,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.peak.fetch_max(current + count, Ordering::AcqRel);
                    return Taken { pool: self, count };
                }
                Err(now) => current = now,
            }
        }
    }

    /// The pool's size.
    #[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    /// The most permits ever in use at once.
    #[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
    pub(crate) fn peak(&self) -> usize {
        self.peak.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::HelperPermits;

    #[test]
    fn concurrent_takes_never_exceed_the_limit() {
        let pool = Arc::new(HelperPermits::new(3));
        let barrier = Arc::new(Barrier::new(32));
        let threads: Vec<_> = (0..32)
            .map(|_| {
                let (pool, barrier) = (Arc::clone(&pool), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..1_000 {
                        let taken = pool.take(3);
                        assert!(taken.count() <= 3);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        }
        assert!(pool.peak() <= 3, "peak {}", pool.peak());
        assert_eq!(pool.take(3).count(), 3, "every permit came back");
    }

    #[test]
    fn a_full_pool_gives_none_and_refills_on_drop() {
        let pool = HelperPermits::new(2);
        let all = pool.take(5);
        assert_eq!(all.count(), 2);
        assert_eq!(pool.take(1).count(), 0);
        drop(all);
        assert_eq!(pool.take(1).count(), 1);
    }
}
