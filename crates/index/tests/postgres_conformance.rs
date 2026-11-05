//! The Postgres index against the backend conformance suite, each case on
//! a database of its own, created on the server `OPENXET_TEST_POSTGRES_URL`
//! names and dropped afterwards, with a second pool on the same database
//! as the second node; and the retries only Postgres has.
//!
//! Without `OPENXET_TEST_POSTGRES_URL` every test here prints why and
//! skips, unless `OPENXET_REQUIRE_SERVICES=1` (as in CI) makes that a
//! failure.

use std::sync::Mutex;

use openxet_index::conformance::{Fixture, block_on, service_url};
use openxet_index::{
    ChunkTable, FileRecord, Index, IndexError, Migrate, NewFile, PostgresIndex, RETRIES,
    Registrant, Term, Visibility,
};
use openxet_protocol::{MerkleHash, chunk_hash};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

const URL_VAR: &str = "OPENXET_TEST_POSTGRES_URL";

/// A server and the databases this fixture created on it.
struct Postgres {
    server: url::Url,
    created: Mutex<Vec<String>>,
}

fn postgres() -> Option<Postgres> {
    let url = service_url(URL_VAR, "postgres://postgres@127.0.0.1:5432/postgres")?;
    match url::Url::parse(&url) {
        Ok(server) => Some(Postgres {
            server,
            created: Mutex::new(Vec::new()),
        }),
        Err(e) => panic!("{URL_VAR} is not a URL: {e}"),
    }
}

impl Postgres {
    fn url_of(&self, database: &str) -> String {
        let mut url = self.server.clone();
        url.set_path(database);
        url.into()
    }

    async fn admin(&self) -> PgConnection {
        match PgConnection::connect(self.server.as_str()).await {
            Ok(conn) => conn,
            Err(e) => panic!("connect to {URL_VAR}: {e}"),
        }
    }

    async fn open(&self, database: &str) -> PostgresIndex {
        match PostgresIndex::connect(&self.url_of(database), 8, Migrate::Auto).await {
            Ok(index) => index,
            Err(e) => panic!("open the index in database {database}: {e}"),
        }
    }
}

#[async_trait::async_trait]
impl Fixture for Postgres {
    type Index = PostgresIndex;

    async fn fresh(&self) -> PostgresIndex {
        let name = format!("openxet_test_{}", uuid::Uuid::now_v7().simple());
        // `name` is ASCII letters, digits and underscores.
        let create = AssertSqlSafe(format!("CREATE DATABASE {name}"));
        if let Err(e) = self.admin().await.execute(create).await {
            panic!("create database {name}: {e}");
        }
        self.created
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(name.clone());
        self.open(&name).await
    }

    async fn second_handle(&self, of: &PostgresIndex) -> PostgresIndex {
        match of.database() {
            Some(name) => self.open(&name).await,
            None => panic!("the index names no database"),
        }
    }
}

impl Drop for Postgres {
    /// Drops the databases this fixture created, on a thread of its own:
    /// the fixture is dropped inside the test's runtime, which can't block
    /// on another.
    fn drop(&mut self) {
        let names = std::mem::take(
            self.created
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let server = self.server.to_string();
        let dropped = std::thread::spawn(move || {
            block_on(async move {
                let mut conn = match PgConnection::connect(&server).await {
                    Ok(conn) => conn,
                    Err(e) => panic!("connect to drop test databases: {e}"),
                };
                for name in names {
                    let drop =
                        AssertSqlSafe(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
                    if let Err(e) = conn.execute(drop).await {
                        panic!("drop database {name}: {e}");
                    }
                }
            });
        })
        .join();
        assert!(
            dropped.is_ok() || std::thread::panicking(),
            "dropping the test databases failed"
        );
    }
}

openxet_index::index_conformance!(postgres: cas);

const SCOPE: &str = "model/org/repo";

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

fn table() -> ChunkTable {
    ChunkTable {
        hashes: vec![(h("chunk"), 1000)].into(),
        offsets: vec![0, 600].into(),
    }
}

fn file(xorb: MerkleHash) -> NewFile {
    NewFile {
        record: FileRecord {
            hash: h("file"),
            size: 1000,
            terms: vec![Term {
                xorb,
                chunk_start: 0,
                chunk_end: 1,
                unpacked_bytes: 1000,
            }],
        },
        sha256: None,
        first_chunk: Some(h("chunk")),
    }
}

/// Ruling M3-R2: a write Postgres aborts with a serialization failure is
/// tried again up to `RETRIES` times, and one that fails every time is
/// `IndexError::Busy`, with nothing recorded.
#[test]
fn serialization_failures_retry_then_busy() {
    block_on(async {
        let Some(f) = postgres() else { return };
        let index = f.fresh().await;
        let reg = |seq| Registrant {
            scope: SCOPE,
            tenant: SCOPE,
            seq,
        };

        index.fail_next_commits(RETRIES);
        assert!(matches!(
            index.put_xorb(&h("xorb"), 700, &table(), &[], SCOPE).await,
            Ok(true)
        ));
        index.fail_next_commits(RETRIES);
        assert!(matches!(
            index
                .register_shard(&h("shard"), &[file(h("xorb"))], reg(1), Visibility::Global)
                .await,
            Ok(true)
        ));

        index.fail_next_commits(RETRIES + 1);
        let busy = index
            .put_xorb(&h("busy-xorb"), 700, &table(), &[], SCOPE)
            .await;
        assert!(matches!(busy, Err(IndexError::Busy(_))), "{busy:?}");
        assert!(matches!(
            index
                .xorb_size(&h("busy-xorb"), SCOPE, Visibility::Global)
                .await,
            Ok(None)
        ));

        index.fail_next_commits(RETRIES + 1);
        let busy = index
            .register_shard(
                &h("busy-shard"),
                &[file(h("xorb"))],
                reg(2),
                Visibility::Global,
            )
            .await;
        assert!(matches!(busy, Err(IndexError::Busy(_))), "{busy:?}");
        assert!(
            matches!(
                index
                    .register_shard(&h("busy-shard"), &[], reg(2), Visibility::Global)
                    .await,
                Ok(true)
            ),
            "the busy registration recorded nothing"
        );
    });
}
