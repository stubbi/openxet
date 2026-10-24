//! Several processes opening one fresh index at once, as several `openxet
//! serve` nodes starting on one data directory do.

#![allow(clippy::unwrap_used)]

use openxet_index::SqliteIndex;

#[test]
fn concurrent_first_opens_all_succeed() {
    for _ in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        // Separate runtimes and pools, like separate processes.
        let results: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(SqliteIndex::open(&path))
                            .map(drop)
                            .map_err(|e| format!("{e:?}"))
                            .err()
                            .unwrap_or_default()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let failures: Vec<&String> = results.iter().filter(|r| !r.is_empty()).collect();
        assert!(failures.is_empty(), "{failures:?}");
    }
}
