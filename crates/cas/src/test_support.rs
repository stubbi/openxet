//! Shared fixtures for the crate's unit tests.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use openxet_auth::{Authenticator, StaticToken, UrlSigner};
use openxet_index::{ChunkTable, FileRecord, Index, IndexError, NewFile, SqliteIndex, Visibility};
use openxet_protocol::{MerkleHash, VerifyLimits};
use openxet_storage::LocalBucket;
use tokio::sync::Semaphore;
use url::Url;

use crate::{AppState, CasConfig};

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
            dedup_send: crate::SendDeadline::DEFAULT,
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

    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        scope: &str,
        vis: Visibility,
    ) -> Result<bool, IndexError> {
        self.inner.register_shard(shard, files, scope, vis).await
    }

    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError> {
        self.inner.file(file, scope).await
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
