//! openxet's bucket: the durable home of xorbs, shards and the records that
//! say which scope registered what.
//!
//! The bucket is the source of truth (design §5.3); the index is rebuildable
//! from it. A successful [`Bucket::put`] means the bytes survive a crash or
//! power loss. Backends beyond local disk (S3, GCS, Azure via `object_store`)
//! arrive in M3 behind the same [`Bucket`] trait.
//!
//! # Objects
//!
//! Every key's logical form is its [`Key`]'s `Display`; backends map it to
//! their own layout. `hex` is a Xet hash's string form ([`hash_to_hex`]),
//! and `{scope}` and `{tenant}` are a name's [`NameHash`]: 64 lowercase hex
//! digits of the SHA-256 of its UTF-8 bytes. Scopes and tenants are
//! arbitrary strings of up to 1024 bytes, so a key never holds one raw; the
//! record body holds the name itself.
//!
//! | Key | Body |
//! |---|---|
//! | `xorbs/default/{hex}` | a stored xorb |
//! | `shards/{hex}` | a stored shard, in the upload form |
//! | `registrations/{scope}/{tenant}/{shard hex}` | a [`Registration`] |
//! | `xorb-owners/{tenant}/{xorb hex}` | a [`XorbOwner`] |
//!
//! Together with the stored xorbs and shards, the two record kinds hold
//! everything the index's scope tables say (ruling M1-R37): a rebuild
//! needs nothing else.

#![forbid(unsafe_code)]

mod dir;
mod error;
mod local;
mod records;

use std::fmt;

use bytes::Bytes;
use futures_core::stream::BoxStream;
use openxet_protocol::{MerkleHash, hash_to_hex};
use sha2::{Digest as _, Sha256};

pub use dir::create_private_dir_all;
pub use error::StorageError;
pub use local::LocalBucket;
pub use records::{Registration, XorbOwner};

/// The SHA-256 of a scope or tenant name, as keys carry it. Its `Display`
/// is 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameHash([u8; 32]);

impl NameHash {
    /// The hash of `name`'s UTF-8 bytes.
    #[must_use]
    pub fn of(name: &str) -> Self {
        Self(Sha256::digest(name.as_bytes()).into())
    }

    /// Parses the 64 lowercase hex digits `Display` writes.
    fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Display for NameHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// The logical name of a stored object. Backends map it to their own layout;
/// the [crate docs](crate#objects) list every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A xorb, stored at `xorbs/default/{hex}`.
    Xorb(MerkleHash),
    /// A shard, stored at `shards/{hex}`.
    Shard(MerkleHash),
    /// A [`Registration`] of `shard` to a scope for a tenant, stored at
    /// `registrations/{scope}/{tenant}/{shard hex}`.
    Registration {
        /// The scope's name hash.
        scope: NameHash,
        /// The tenant's name hash.
        tenant: NameHash,
        /// The registered shard.
        shard: MerkleHash,
    },
    /// A [`XorbOwner`] record, stored at `xorb-owners/{tenant}/{xorb hex}`.
    XorbOwner {
        /// The tenant's name hash.
        tenant: NameHash,
        /// The owned xorb.
        xorb: MerkleHash,
    },
}

impl Key {
    /// The key of the registration of `shard` to `scope` for `tenant`.
    #[must_use]
    pub fn registration(scope: &str, tenant: &str, shard: MerkleHash) -> Self {
        Self::Registration {
            scope: NameHash::of(scope),
            tenant: NameHash::of(tenant),
            shard,
        }
    }

    /// The key of `tenant`'s ownership of `xorb`.
    #[must_use]
    pub fn xorb_owner(tenant: &str, xorb: MerkleHash) -> Self {
        Self::XorbOwner {
            tenant: NameHash::of(tenant),
            xorb,
        }
    }

    /// The key's kind.
    #[must_use]
    pub fn kind(&self) -> KeyKind {
        match self {
            Self::Xorb(_) => KeyKind::Xorb,
            Self::Shard(_) => KeyKind::Shard,
            Self::Registration { .. } => KeyKind::Registration,
            Self::XorbOwner { .. } => KeyKind::XorbOwner,
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Xorb(h) => write!(f, "xorbs/default/{}", hash_to_hex(h)),
            Key::Shard(h) => write!(f, "shards/{}", hash_to_hex(h)),
            Key::Registration {
                scope,
                tenant,
                shard,
            } => write!(f, "registrations/{scope}/{tenant}/{}", hash_to_hex(shard)),
            Key::XorbOwner { tenant, xorb } => {
                write!(f, "xorb-owners/{tenant}/{}", hash_to_hex(xorb))
            }
        }
    }
}

/// A kind of object, for [`Bucket::list`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyKind {
    /// [`Key::Xorb`].
    Xorb,
    /// [`Key::Shard`].
    Shard,
    /// [`Key::Registration`].
    Registration,
    /// [`Key::XorbOwner`].
    XorbOwner,
}

/// The size of the blocks [`Bucket::get_range_stream`] yields: 1 MiB. A
/// streamed read holds about one block at a time (ruling M1-R27).
pub const STREAM_BLOCK_BYTES: usize = 1024 * 1024;

/// A range being read block by block ([`Bucket::get_range_stream`]).
pub struct RangeStream {
    /// The whole object's size, as found when the read started.
    pub size: u64,
    /// The range's bytes, in order, in blocks of at most
    /// [`STREAM_BLOCK_BYTES`]. A failure partway ends the stream with the
    /// error.
    pub blocks: BoxStream<'static, Result<Bytes, StorageError>>,
}

impl fmt::Debug for RangeStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RangeStream")
            .field("size", &self.size)
            .finish_non_exhaustive()
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
    /// [`StorageError::SizeConflict`] if the key already exists with a
    /// different size. Keys are verified content hashes, so this means a bug
    /// or a hash collision. It is detected when puts are sequential and is
    /// best-effort under concurrent puts of the same key, so callers must
    /// verify content before putting, not rely on this check. And
    /// [`StorageError::Io`] if the write fails. Nothing is stored on error.
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError>;

    /// Stores `bytes` under `key` unless the key exists, for objects whose
    /// bytes the key doesn't determine: a [`Registration`] carries a
    /// sequence number its writer chose. Returns whether this call stored
    /// them. Either way, on `Ok` the object under `key` is durable, and
    /// every writer then reads the same one, the first stored.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the write fails. Nothing is stored on error.
    async fn create(&self, key: &Key, bytes: Bytes) -> Result<bool, StorageError>;

    /// The whole object, or `None` if it does not exist. For small objects
    /// such as records; read xorbs by range.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the read fails.
    async fn get(&self, key: &Key) -> Result<Option<Bytes>, StorageError> {
        match self.size(key).await? {
            None => Ok(None),
            Some(0) => Ok(Some(Bytes::new())),
            Some(size) => match self.get_range(key, 0, size - 1).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(StorageError::NotFound { .. }) => Ok(None),
                Err(e) => Err(e),
            },
        }
    }

    /// Every stored object of `kind`, in no particular order: what
    /// `reindex` walks (issue #26). Entries whose names are not a key of
    /// that kind are skipped.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if listing fails.
    async fn list(&self, kind: KeyKind) -> Result<Vec<Key>, StorageError>;

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

    /// Reads bytes `start..=end_inclusive` of the object as a stream of
    /// blocks of at most [`STREAM_BLOCK_BYTES`], so a reader holds one block
    /// at a time, never the range (ruling M1-R27). [`Bucket::get_range`]
    /// stays for small reads.
    ///
    /// The key and the range are checked before this returns, so a caller
    /// learns of a bad range before it sends anything.
    ///
    /// # Errors
    ///
    /// As [`Bucket::get_range`], before the stream starts; a read failing
    /// partway ends the stream with [`StorageError::Io`].
    async fn get_range_stream(
        &self,
        key: &Key,
        start: u64,
        end_inclusive: u64,
    ) -> Result<RangeStream, StorageError>;

    /// The size of the object, or `None` if it does not exist.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the lookup fails.
    async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError>;
}
