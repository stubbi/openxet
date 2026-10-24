//! CPU-heavy request work on tokio's blocking threads, bounded by the
//! `validate` permits (rulings M1-R8, M1-R25).

use tokio::sync::OwnedSemaphorePermit;

use crate::CasError;

/// Runs `work` on a blocking thread holding `permit`, and hands the permit
/// back with the result. The blocking task holds it while the work runs: a
/// blocking task can't be cancelled, so a permit held by the caller would be
/// returned early if the caller were dropped while the work ran on (ruling
/// M1-R8). A caller dropped meanwhile drops the permit as the work ends.
pub(crate) async fn blocking<T: Send + 'static>(
    permit: OwnedSemaphorePermit,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<(OwnedSemaphorePermit, T), CasError> {
    tokio::task::spawn_blocking(move || {
        let out = work();
        (permit, out)
    })
    .await
    .map_err(CasError::internal)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tokio::sync::Semaphore;

    use super::*;

    #[tokio::test]
    async fn the_permit_lives_as_long_as_the_blocking_work() {
        // A dropped caller (a v1 client gone mid-verification) mustn't
        // return the permit while the blocking work runs on (ruling M1-R8).
        let validate = Arc::new(Semaphore::new(1));
        let done = Arc::new(AtomicBool::new(false));

        let caller = {
            let (validate, done) = (Arc::clone(&validate), Arc::clone(&done));
            tokio::spawn(async move {
                let permit = validate.acquire_owned().await.unwrap();
                blocking(permit, move || {
                    std::thread::sleep(Duration::from_millis(200));
                    done.store(true, Ordering::SeqCst);
                })
                .await
                .map(|_| ())
            })
        };
        while validate.available_permits() == 1 {
            tokio::task::yield_now().await;
        }
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());

        assert_eq!(validate.available_permits(), 0);
        let released = tokio::time::timeout(Duration::from_secs(30), async {
            while validate.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        released.await.unwrap();
        assert!(done.load(Ordering::SeqCst), "the permit came back first");
    }
}
