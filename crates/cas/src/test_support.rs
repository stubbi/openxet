//! Shared fixtures for the crate's unit tests.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use http_body::{Frame, SizeHint};
use http_body_util::channel::{Channel, Sender};

use openxet_auth::{Authenticator, StaticToken, UrlSigner};
use openxet_index::{
    ChunkLayout, ChunkTable, FileRecord, Index, IndexError, NewFile, Registrant, SqliteIndex,
    TermBytes, Visibility,
};
use openxet_protocol::{MerkleHash, VerifyLimits};
use openxet_storage::{Bucket, Key, LocalBucket, StorageError};
use tokio::sync::Semaphore;
use url::Url;

use crate::routes::Routes;
use crate::{AppState, CasConfig};

/// `routes` with `state`, composed as `openxet serve` composes the CAS.
pub(crate) fn serve(routes: Routes, state: AppState) -> Router {
    let body_deadline = state.cfg.body_deadline;
    openxet_http::compose(vec![routes.with_state(state)], body_deadline)
}

/// The CAS's routes with `state`, composed as `openxet serve` composes them.
pub(crate) fn router(state: AppState) -> Router {
    serve(crate::routes(), state)
}

/// State over a fresh index and bucket in `dir`, accepting `tokens`.
pub(crate) async fn state(dir: &Path, tokens: Vec<StaticToken>) -> AppState {
    AppState::new(
        Arc::new(SqliteIndex::open(&dir.join("index.sqlite")).await.unwrap()),
        Arc::new(LocalBucket::open(&dir.join("bucket")).unwrap()),
        Arc::new(
            Authenticator::new(tokens, vec![], Authenticator::DEFAULT_MAX_TOKEN_LIFETIME).unwrap(),
        ),
        Arc::new(UrlSigner::new([1; 32], UrlSigner::DEFAULT_LIFETIME).unwrap()),
        CasConfig {
            public_url: Url::parse("http://127.0.0.1:8080").unwrap(),
            visibility: Visibility::Global,
            verify_limits: VerifyLimits::default(),
            heartbeat_interval: CasConfig::DEFAULT_HEARTBEAT_INTERVAL,
            body_deadline: crate::BodyDeadline::DEFAULT,
            send_deadline: crate::SendDeadline::DEFAULT,
            memory_budget: crate::MemoryBudget::DEFAULT_BYTES,
        },
    )
}

/// An index that records how many permits of a semaphore are free whenever
/// chunk tables are loaded, for tests that the load holds a permit.
pub(crate) struct PermitProbe {
    inner: Arc<dyn Index>,
    validate: Arc<Semaphore>,
    free_at_load: AtomicUsize,
}

impl PermitProbe {
    /// Wraps `state`'s index, probing `permits`.
    pub(crate) fn install(state: &mut AppState, permits: &Arc<Semaphore>) -> Arc<Self> {
        let probe = Arc::new(Self {
            inner: Arc::clone(&state.index),
            validate: Arc::clone(permits),
            free_at_load: AtomicUsize::new(usize::MAX),
        });
        state.index = Arc::clone(&probe) as Arc<dyn Index>;
        probe
    }

    /// The free permits at the last `chunk_tables` call, `None` if none.
    pub(crate) fn free_at_load(&self) -> Option<usize> {
        Some(self.free_at_load.load(Ordering::SeqCst)).filter(|n| *n != usize::MAX)
    }
}

#[async_trait::async_trait]
impl Index for PermitProbe {
    async fn put_xorb(
        &self,
        xorb: &MerkleHash,
        size: u64,
        table: &ChunkTable,
        eligible: &[MerkleHash],
        scope: &str,
    ) -> Result<bool, IndexError> {
        self.inner
            .put_xorb(xorb, size, table, eligible, scope)
            .await
    }

    async fn xorb_size(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<u64>, IndexError> {
        self.inner.xorb_size(xorb, scope, vis).await
    }

    async fn chunk_tables(
        &self,
        xorbs: &[MerkleHash],
        scope: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError> {
        self.free_at_load
            .store(self.validate.available_permits(), Ordering::SeqCst);
        self.inner.chunk_tables(xorbs, scope, vis).await
    }

    async fn chunk_layouts(
        &self,
        xorbs: &[MerkleHash],
        scope: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkLayout>, IndexError> {
        self.inner.chunk_layouts(xorbs, scope, vis).await
    }

    async fn next_registration_seq(&self) -> Result<u64, IndexError> {
        self.inner.next_registration_seq().await
    }

    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        by: Registrant<'_>,
        vis: Visibility,
    ) -> Result<bool, IndexError> {
        self.inner.register_shard(shard, files, by, vis).await
    }

    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError> {
        self.inner.file(file, scope).await
    }

    async fn files(
        &self,
        files: &[MerkleHash],
        scope: &str,
    ) -> Result<HashMap<MerkleHash, FileRecord>, IndexError> {
        self.inner.files(files, scope).await
    }

    async fn term_count(&self, files: &[MerkleHash], scope: &str) -> Result<u64, IndexError> {
        self.inner.term_count(files, scope).await
    }

    async fn file_with_bytes(
        &self,
        file: &MerkleHash,
        scope: &str,
    ) -> Result<Option<(FileRecord, Option<TermBytes>)>, IndexError> {
        self.inner.file_with_bytes(file, scope).await
    }

    async fn dedup_xorb(
        &self,
        chunk: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<MerkleHash>, IndexError> {
        self.inner.dedup_xorb(chunk, scope, vis).await
    }

    async fn dedup_shard_xorbs(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<Vec<MerkleHash>>, IndexError> {
        self.inner.dedup_shard_xorbs(xorb, scope, vis).await
    }
}

/// A request body declaring `len` bytes, as hyper's body for a
/// `Content-Length` does, whose bytes the test sends by hand.
pub(crate) fn declared_body(len: u64) -> (Sender<Bytes>, Body) {
    let (tx, body) = Channel::<Bytes>::new(16);
    (tx, Body::new(Declared { inner: body, len }))
}

struct Declared {
    inner: Channel<Bytes>,
    len: u64,
}

impl http_body::Body for Declared {
    type Data = Bytes;
    type Error = <Channel<Bytes> as http_body::Body>::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len)
    }
}

/// A bucket whose puts block a thread for `delay` first, as a slow disk
/// does under `LocalBucket`'s `spawn_blocking`: a dropped caller can't
/// cancel them. Counts the puts finished.
pub(crate) struct BlockingBucket {
    inner: Arc<dyn Bucket>,
    delay: std::time::Duration,
    pub(crate) started: AtomicUsize,
    pub(crate) finished: Arc<AtomicUsize>,
}

impl BlockingBucket {
    /// Wraps `state`'s bucket.
    pub(crate) fn install(state: &mut AppState, delay: std::time::Duration) -> Arc<Self> {
        let bucket = Arc::new(Self {
            inner: Arc::clone(&state.bucket),
            delay,
            started: AtomicUsize::new(0),
            finished: Arc::new(AtomicUsize::new(0)),
        });
        state.bucket = Arc::clone(&bucket) as Arc<dyn Bucket>;
        bucket
    }
}

#[async_trait::async_trait]
impl Bucket for BlockingBucket {
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let (delay, finished) = (self.delay, Arc::clone(&self.finished));
        let inner = Arc::clone(&self.inner);
        let key = key.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(delay);
            let out = handle.block_on(inner.put(&key, bytes));
            finished.fetch_add(1, Ordering::SeqCst);
            out
        })
        .await
        .unwrap()
    }

    async fn create(&self, key: &Key, bytes: Bytes) -> Result<bool, StorageError> {
        self.inner.create(key, bytes).await
    }

    async fn list(&self, kind: openxet_storage::KeyKind) -> Result<Vec<Key>, StorageError> {
        self.inner.list(kind).await
    }

    async fn get_range(&self, key: &Key, start: u64, end: u64) -> Result<Bytes, StorageError> {
        self.inner.get_range(key, start, end).await
    }

    async fn get_range_stream(
        &self,
        key: &Key,
        start: u64,
        end: u64,
    ) -> Result<openxet_storage::RangeStream, StorageError> {
        self.inner.get_range_stream(key, start, end).await
    }

    async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError> {
        self.inner.size(key).await
    }

    async fn put_replace(&self, key: Key, bytes: Bytes) -> Result<(), StorageError> {
        self.inner.put_replace(key, bytes).await
    }

    async fn put_stream(
        &self,
        key: Key,
        body: openxet_storage::BodyStream,
        len: u64,
    ) -> Result<(), StorageError> {
        self.inner.put_stream(key, body, len).await
    }

    async fn get_to_file(&self, key: Key, path: &std::path::Path) -> Result<u64, StorageError> {
        self.inner.get_to_file(key, path).await
    }

    async fn last_modified(&self, key: Key) -> Result<Option<u64>, StorageError> {
        self.inner.last_modified(key).await
    }

    async fn list_under(
        &self,
        repo: openxet_storage::RepoIdHex,
        kind: openxet_storage::KeyKind,
    ) -> Result<Vec<Key>, StorageError> {
        self.inner.list_under(repo, kind).await
    }

    async fn delete(&self, key: &Key) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// Polls `done` every 5 ms for up to 30 s.
pub(crate) async fn eventually(what: &str, done: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !done() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}
