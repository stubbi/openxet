//! The index and repository the Hub latency gate (`tests/hub_latency_gate.rs`)
//! and benchmark (`benches/metadata.rs`) measure (design §5.8): an index of
//! [`REPOS`] repositories, one of which, [`BIG`], holds 10,000 files.
//!
//! The shape is pinned so a gate can't pass on an easy repository:
//! - [`XET_FILES`] Xet files in the root, `model-00000.safetensors` and on,
//!   each an LFS pointer whose SHA-256 is registered to the repository's
//!   scope, so every one `tree` lists costs an index lookup for its
//!   `xetHash`
//! - [`REGULAR_FILES`] small regular files under `configs/`, which `resolve`
//!   serves inline from their Git blob
//!
//! So the root's first `tree` page lists the initial commit's
//! `.gitattributes`, `configs` and 998 Xet files. The
//! other repositories are index rows only: the token route reads a
//! repository's row and nothing of its Git data.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use openxet_auth::Action;
use openxet_hub::test_support::{BEARER, TestHub, rule, user_token};
use openxet_index::{ChunkTable, FileRecord, NewFile, Registrant, Repo, RepoId, Term, Visibility};
use openxet_protocol::MerkleHash;
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// Repositories in the index, [`BIG`] included.
pub const REPOS: u32 = 10_000;
/// [`BIG`]'s Xet files.
pub const XET_FILES: u32 = 9_000;
/// [`BIG`]'s regular files.
pub const REGULAR_FILES: u32 = 1_000;
/// The repository holding the files.
pub const BIG: &str = "bench/big";
/// The credential every request carries.
pub const TOKEN: &str = "oxt_bench_metadata";

/// Each Xet file's size: one chunk.
const XET_FILE_BYTES: u32 = 64 * 1024;

/// A Hub over [`REPOS`] repositories, [`BIG`] holding the files.
pub struct Fixture {
    /// The Hub, over its own index and bucket on disk.
    pub hub: TestHub,
    /// Every repository's `{namespace}/{name}`, [`BIG`] first.
    pub repos: Vec<String>,
}

/// The `i`th Xet file's path.
pub fn xet_path(i: u32) -> String {
    format!("model-{i:05}.safetensors")
}

/// The `i`th regular file's path.
pub fn regular_path(i: u32) -> String {
    format!("configs/config-{i:04}.json")
}

/// A distinct hash per `(kind, i)`, without hashing.
fn synthetic(kind: u8, i: u32) -> [u8; 32] {
    let mut raw = [0u8; 32];
    raw[0] = kind;
    raw[1..5].copy_from_slice(&i.to_le_bytes());
    raw[31] = 0x6d;
    raw
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Builds the fixture.
pub async fn fixture() -> Fixture {
    let hub = TestHub::start(
        vec![user_token(TOKEN, "bench", &[])],
        vec![rule(
            &["bench"],
            &["*"],
            &[Action::Read, Action::Write, Action::Create],
        )],
    )
    .await;
    hub.create(TOKEN, "bench", "big", None).await;
    let mut repos = vec![BIG.to_owned()];
    for i in 1..REPOS {
        let repo = Repo {
            id: RepoId::generate(),
            namespace: format!("org{:03}", i % 100),
            name: format!("repo-{i:05}"),
            private: false,
            labels: vec!["model".to_owned()],
            created_at: 1_761_400_000,
            full_repack_gen: 0,
        };
        hub.state.index.create_repo(&repo).await.unwrap();
        repos.push(format!("{}/{}", repo.namespace, repo.name));
    }
    let id = hub
        .state
        .index
        .repo_by_name("bench", "big")
        .await
        .unwrap()
        .unwrap()
        .id;
    register_xet_files(&hub, &format!("repo:{id}")).await;
    commit_files(&hub).await;
    Fixture { hub, repos }
}

/// Registers [`XET_FILES`] files of one chunk each to `scope`, as a shard
/// upload would, so the commit may name their SHA-256s.
async fn register_xet_files(hub: &TestHub, scope: &str) {
    let xorb = MerkleHash::from(synthetic(1, 0));
    let table = ChunkTable {
        hashes: vec![(MerkleHash::from(synthetic(2, 0)), u64::from(XET_FILE_BYTES))].into(),
        offsets: vec![0, XET_FILE_BYTES + 8].into(),
    };
    let index = &hub.state.index;
    index
        .put_xorb(
            &xorb,
            u64::from(XET_FILE_BYTES) + 1024,
            &table,
            &[],
            "bench",
        )
        .await
        .unwrap();
    let files: Vec<NewFile> = (0..XET_FILES)
        .map(|i| NewFile {
            record: FileRecord {
                hash: MerkleHash::from(synthetic(3, i)),
                size: u64::from(XET_FILE_BYTES),
                terms: vec![Term {
                    xorb,
                    chunk_start: 0,
                    chunk_end: 1,
                    unpacked_bytes: XET_FILE_BYTES,
                }],
            },
            sha256: Some(synthetic(4, i)),
            first_chunk: None,
        })
        .collect();
    let seq = index.next_registration_seq().await.unwrap();
    index
        .register_shard(
            &MerkleHash::from(synthetic(5, 0)),
            &files,
            Registrant {
                scope,
                tenant: "bench",
                seq,
            },
            Visibility::Global,
        )
        .await
        .unwrap();
}

/// Commits every file to [`BIG`]'s `main` in one Hub commit.
async fn commit_files(hub: &TestHub) {
    let mut lines =
        vec![json!({"key": "header", "value": {"summary": "files", "description": ""}})];
    lines.extend((0..XET_FILES).map(|i| {
        json!({"key": "lfsFile", "value": {
            "path": xet_path(i),
            "algo": "sha256",
            "oid": hex(&synthetic(4, i)),
            "size": XET_FILE_BYTES,
        }})
    }));
    lines.extend((0..REGULAR_FILES).map(|i| {
        let content = format!("{{\"index\": {i}}}\n");
        json!({"key": "file", "value": {
            "path": regular_path(i),
            "content": STANDARD.encode(content),
            "encoding": "base64",
        }})
    }));
    let reply = hub
        .commit(TOKEN, &format!("/api/models/{BIG}"), "main", &lines)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "commit: {}", reply.text());
}

/// `method path` with [`TOKEN`], timed from the request to the body's last
/// byte; panics unless it answers `expected`. Returns the time and the
/// body.
pub async fn timed(
    hub: &TestHub,
    method: Method,
    path: &str,
    expected: StatusCode,
) -> (Duration, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(AUTHORIZATION, format!("{BEARER} {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let started = Instant::now();
    let response = hub.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let took = started.elapsed();
    assert_eq!(
        status,
        expected,
        "{path}: {}",
        String::from_utf8_lossy(&body)
    );
    (took, body.to_vec())
}

/// The entries of a `tree` answer.
pub fn entries(body: &[u8]) -> Vec<Value> {
    serde_json::from_slice::<Value>(body)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}
