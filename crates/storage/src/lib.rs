//! openxet's bucket: the durable, content-addressed home of xorbs and shards.
//!
//! The bucket is the source of truth (design §5.3); the index is rebuildable
//! from it. A successful [`Bucket::put`] means the bytes survive a crash or
//! power loss. Backends beyond local disk (S3, GCS, Azure via `object_store`)
//! arrive in M3 behind the same [`Bucket`] trait.

#![forbid(unsafe_code)]

mod error;
mod local;

use std::fmt;

use bytes::Bytes;
use openxet_protocol::{MerkleHash, hash_to_hex};

pub use error::StorageError;
pub use local::LocalBucket;

/// The logical name of a stored object. Backends map it to their own layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A xorb, stored at `xorbs/default/{hex}`.
    Xorb(MerkleHash),
    /// A shard, stored at `shards/{hex}`.
    Shard(MerkleHash),
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Xorb(h) => write!(f, "xorbs/default/{}", hash_to_hex(h)),
            Key::Shard(h) => write!(f, "shards/{}", hash_to_hex(h)),
        }
    }
}

/// Durable storage for content-addressed objects.
#[async_trait::async_trait]
pub trait Bucket: Send + Sync + 'static {
    /// Stores `bytes` under `key`. On `Ok` the bytes are durable.
    ///
    /// Keys are content-addressed, so putting a key that already exists with
    /// the same size is a no-op.
    ///
    /// # Errors
    ///
    /// [`StorageError::SizeConflict`] if the key exists with a different size
    /// (corruption or a bug: the object is never overwritten), and
    /// [`StorageError::Io`] if the write fails. Nothing is stored on error.
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError>;

    /// Reads bytes `start..=end_inclusive` of the object, matching the
    /// inclusive ranges of a reconstruction's `bytes` field. Only the
    /// requested range is read.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the key is absent,
    /// [`StorageError::RangeOutOfBounds`] if the range is empty or extends
    /// past the end, and [`StorageError::Io`] if the read fails.
    async fn get_range(
        &self,
        key: &Key,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Bytes, StorageError>;

    /// The size of the object, or `None` if it does not exist.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the lookup fails.
    async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError>;
}
