//! Doubles the Hub's integration tests share: a bucket and an authorizer
//! that run a hook at the moment a test needs to interleave a concurrent
//! change (a lost name race, a stale label list).

#![allow(dead_code, clippy::unwrap_used)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use openxet_auth::{Action, Authorizer, AuthzError, RulesAuthorizer, Target, UserPrincipal};
use openxet_storage::{
    BodyStream, Bucket, Key, KeyKind, LocalBucket, RangeStream, RepoIdHex, RepoMeta, StorageError,
};

/// A boxed future of a hook.
pub type HookFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// What a [`HookBucket`] runs on each metadata write, with the record.
pub type MetaHook = Arc<dyn Fn(RepoMeta) -> HookFuture + Send + Sync>;

/// A local bucket that records every repository metadata record written
/// and runs a hook right after writing it, before the caller goes on: the
/// moment another writer would interleave.
pub struct HookBucket {
    inner: LocalBucket,
    hook: Mutex<Option<MetaHook>>,
    /// Every metadata record written, in order.
    pub replaced: Mutex<Vec<RepoMeta>>,
}

impl HookBucket {
    /// A bucket in `dir`, without a hook.
    pub fn open(dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            inner: LocalBucket::open(dir).unwrap(),
            hook: Mutex::new(None),
            replaced: Mutex::new(Vec::new()),
        })
    }

    /// Runs `hook` after each later metadata write.
    pub fn set_hook(&self, hook: MetaHook) {
        *self.hook.lock().unwrap() = Some(hook);
    }

    /// The records written so far.
    pub fn replaced(&self) -> Vec<RepoMeta> {
        self.replaced.lock().unwrap().clone()
    }

    /// The metadata record stored for `id`.
    pub async fn meta(&self, id: &str) -> RepoMeta {
        let key = Key::RepoMeta {
            repo: RepoIdHex::parse(id).unwrap(),
        };
        RepoMeta::from_bytes(&key, &self.inner.get(&key).await.unwrap().unwrap()).unwrap()
    }
}

#[async_trait::async_trait]
impl Bucket for HookBucket {
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }
    async fn create(&self, key: &Key, bytes: Bytes) -> Result<bool, StorageError> {
        self.inner.create(key, bytes).await
    }
    async fn list(&self, kind: KeyKind) -> Result<Vec<Key>, StorageError> {
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
    ) -> Result<RangeStream, StorageError> {
        self.inner.get_range_stream(key, start, end).await
    }
    async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError> {
        self.inner.size(key).await
    }
    async fn put_replace(&self, key: Key, bytes: Bytes) -> Result<(), StorageError> {
        let meta = RepoMeta::from_bytes(&key, &bytes).unwrap();
        self.replaced.lock().unwrap().push(meta.clone());
        self.inner.put_replace(key, bytes).await?;
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(meta).await;
        }
        Ok(())
    }
    async fn put_stream(&self, key: Key, body: BodyStream, len: u64) -> Result<(), StorageError> {
        self.inner.put_stream(key, body, len).await
    }
    async fn get_to_file(&self, key: Key, path: &Path) -> Result<u64, StorageError> {
        self.inner.get_to_file(key, path).await
    }
    async fn last_modified(&self, key: Key) -> Result<Option<u64>, StorageError> {
        self.inner.last_modified(key).await
    }
    async fn list_under(&self, repo: RepoIdHex, kind: KeyKind) -> Result<Vec<Key>, StorageError> {
        self.inner.list_under(repo, kind).await
    }
    async fn delete(&self, key: &Key) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// What a [`HookAuthz`] runs on each `write` decision.
pub type WriteHook = Arc<dyn Fn() -> HookFuture + Send + Sync>;

/// The rules authorizer, running a hook whenever it decides a `write`.
pub struct HookAuthz {
    inner: RulesAuthorizer,
    on_write: Mutex<Option<WriteHook>>,
}

impl HookAuthz {
    /// `inner`, without a hook.
    pub fn new(inner: RulesAuthorizer) -> Arc<Self> {
        Arc::new(Self {
            inner,
            on_write: Mutex::new(None),
        })
    }

    /// Runs `hook` on each later `write` decision.
    pub fn set_hook(&self, hook: WriteHook) {
        *self.on_write.lock().unwrap() = Some(hook);
    }
}

#[async_trait::async_trait]
impl Authorizer for HookAuthz {
    async fn allows(
        &self,
        who: &UserPrincipal,
        action: Action,
        target: Target<'_>,
    ) -> Result<bool, AuthzError> {
        if action == Action::Write {
            let hook = self.on_write.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook().await;
            }
        }
        self.inner.allows(who, action, target).await
    }
}
