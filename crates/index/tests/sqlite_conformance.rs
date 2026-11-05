//! The SQLite index against the backend conformance suite: every case of
//! `openxet_index::conformance`, each on a fresh database file, with a
//! second `SqliteIndex::open` on the same file as the second node.

use std::sync::atomic::{AtomicUsize, Ordering};

use openxet_index::SqliteIndex;
use openxet_index::conformance::Fixture;
use tempfile::TempDir;

struct Sqlite {
    dir: TempDir,
    next: AtomicUsize,
}

fn sqlite() -> Sqlite {
    match TempDir::new() {
        Ok(dir) => Sqlite {
            dir,
            next: AtomicUsize::new(0),
        },
        Err(e) => panic!("tempdir: {e}"),
    }
}

async fn open(path: &std::path::Path) -> SqliteIndex {
    match SqliteIndex::open(path).await {
        Ok(index) => index,
        Err(e) => panic!("open index at {}: {e}", path.display()),
    }
}

#[async_trait::async_trait]
impl Fixture for Sqlite {
    type Index = SqliteIndex;

    async fn fresh(&self) -> SqliteIndex {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        open(&self.dir.path().join(format!("index-{n}.sqlite"))).await
    }

    async fn second_handle(&self, of: &SqliteIndex) -> SqliteIndex {
        open(&of.path()).await
    }
}

openxet_index::index_conformance!(sqlite: cas, repos, git);
