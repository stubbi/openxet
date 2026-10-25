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
//! | `repos/{id}/meta` | a [`RepoMeta`] |
//! | `repos/{id}/packs/{name}.{pack,idx,rev,bitmap,mtimes}` | a Git pack and its side files ([`PackExt`]) |
//! | `repos/{id}/midx/{generation}.{multi-pack-index,bitmap,rev}` | a multi-pack index generation and its side files ([`MidxExt`]) |
//! | `repos/{id}/reflog/{seq}` | a [`RefLogRecord`] |
//! | `purges/{hex}` | a [`PurgeRecord`] |
//!
//! Together with the stored xorbs and shards, the two record kinds hold
//! everything the index's scope tables say (ruling M1-R37): a rebuild
//! needs nothing else.
//!
//! A repository's keys carry its immutable id ([`RepoIdHex`], ruling
//! M2-R2), never its namespace or name, so a rename rewrites its one
//! metadata record and orphans nothing. `{name}` is the pack's name as git
//! writes it, `pack-` and its 40 (SHA-1) or 64 (SHA-256) lowercase hex
//! digit checksum; `{generation}` is a decimal number, and `{seq}` the ref
//! log's sequence number as 20 decimal digits, so names sort in log order.
//! The side files are those a node hydrates (ADR 0004 item 4).
//!
//! How each kind is written:
//! - xorbs, shards and [`XorbOwner`]s with [`Bucket::put`]: their bytes
//!   follow from their key;
//! - [`Registration`]s, [`RefLogRecord`]s and [`PurgeRecord`]s with
//!   [`Bucket::create`]: written once, never changed (ruling M1-R37);
//! - packs and multi-pack indexes with [`Bucket::put_stream`], once;
//! - a [`RepoMeta`] with [`Bucket::put_replace`], the one object the
//!   bucket ever overwrites, whole and atomically.
//!
//! Only packs and multi-pack indexes are ever deleted, by the pack sweep
//! with [`Bucket::delete`] (ADR 0004 item 5).

#![forbid(unsafe_code)]

mod dir;
mod error;
mod local;
mod records;

use std::fmt;
use std::path::Path;

use bytes::Bytes;
use futures_core::stream::BoxStream;
use openxet_protocol::{MerkleHash, hash_to_hex};
use sha2::{Digest as _, Sha256};

pub use dir::create_private_dir_all;
pub use error::StorageError;
pub use local::LocalBucket;
pub use records::{PurgeRecord, RefLogEntry, RefLogRecord, Registration, RepoMeta, XorbOwner};

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

/// A repository's immutable id as bucket keys carry it: 32 lowercase hex
/// digits (ruling M2-R2). The index owns the UUID it renders; this type
/// only guarantees the form, so no key ever holds a path of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoIdHex(u128);

impl RepoIdHex {
    /// Parses 32 lowercase hex digits, as a UUID's simple form writes them.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for anything else.
    pub fn parse(hex: &str) -> Result<Self, StorageError> {
        // `from_radix` alone would also take a sign and upper case.
        if hex.len() == 32
            && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            && let Ok(id) = u128::from_str_radix(hex, 16)
        {
            return Ok(Self(id));
        }
        Err(StorageError::InvalidKey {
            key: hex.escape_debug().to_string(),
            reason: "a repository id is 32 lowercase hex digits, the simple form of its UUID",
        })
    }
}

impl std::str::FromStr for RepoIdHex {
    type Err = StorageError;

    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        Self::parse(hex)
    }
}

impl fmt::Display for RepoIdHex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// A Git pack's files in the bucket (ADR 0004 item 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackExt {
    /// `.pack`: the objects.
    Pack,
    /// `.idx`: the pack index.
    Idx,
    /// `.rev`: the reverse index.
    Rev,
    /// `.bitmap`: the reachability bitmap.
    Bitmap,
    /// `.mtimes`: a cruft pack's per-object times.
    Mtimes,
}

impl PackExt {
    const ALL: [Self; 5] = [Self::Pack, Self::Idx, Self::Rev, Self::Bitmap, Self::Mtimes];

    /// The file extension, without the dot.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::Idx => "idx",
            Self::Rev => "rev",
            Self::Bitmap => "bitmap",
            Self::Mtimes => "mtimes",
        }
    }

    pub(crate) fn parse(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == ext)
    }
}

/// A multi-pack index generation's files in the bucket (ADR 0004 item 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidxExt {
    /// `.multi-pack-index`: the index itself.
    MultiPackIndex,
    /// `.bitmap`: its reachability bitmap.
    Bitmap,
    /// `.rev`: its reverse index.
    Rev,
}

impl MidxExt {
    const ALL: [Self; 3] = [Self::MultiPackIndex, Self::Bitmap, Self::Rev];

    /// The file extension, without the dot.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MultiPackIndex => "multi-pack-index",
            Self::Bitmap => "bitmap",
            Self::Rev => "rev",
        }
    }

    pub(crate) fn parse(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == ext)
    }
}

/// A Git pack's name as git writes it: `pack-` and its 40 (SHA-1) or 64
/// (SHA-256) lowercase hex digit checksum. Checked at construction, so a
/// pack key can't name a path outside its repository's directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackName(String);

impl PackName {
    /// Parses a pack name, such as the stem git's `index-pack` writes.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for anything else.
    pub fn parse(name: &str) -> Result<Self, StorageError> {
        let valid = name.strip_prefix("pack-").is_some_and(|sum| {
            matches!(sum.len(), 40 | 64)
                && sum.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if valid {
            Ok(Self(name.to_owned()))
        } else {
            Err(StorageError::InvalidKey {
                key: name.escape_debug().to_string(),
                reason: "a pack's name is `pack-` and its 40 or 64 lowercase hex digit checksum, as git writes it",
            })
        }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for PackName {
    type Err = StorageError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name)
    }
}

impl fmt::Display for PackName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The logical name of a stored object. Backends map it to their own layout;
/// the [crate docs](crate#objects) list every kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// A repository's [`RepoMeta`], stored at `repos/{id}/meta`.
    RepoMeta {
        /// The repository.
        repo: RepoIdHex,
    },
    /// One file of a Git pack, stored at `repos/{id}/packs/{name}.{ext}`.
    Pack {
        /// The repository.
        repo: RepoIdHex,
        /// The pack's name.
        name: PackName,
        /// Which of the pack's files.
        ext: PackExt,
    },
    /// One file of a multi-pack index generation, stored at
    /// `repos/{id}/midx/{generation}.{ext}`.
    Midx {
        /// The repository.
        repo: RepoIdHex,
        /// The generation.
        generation: u64,
        /// Which of the generation's files.
        ext: MidxExt,
    },
    /// A [`RefLogRecord`], stored at `repos/{id}/reflog/{seq:020}`.
    RefLog {
        /// The repository.
        repo: RepoIdHex,
        /// The highest sequence number among the record's entries.
        seq: u64,
    },
    /// A [`PurgeRecord`] of a file, stored at `purges/{hex}`.
    Purge(MerkleHash),
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
            Self::RepoMeta { .. } => KeyKind::RepoMeta,
            Self::Pack { .. } => KeyKind::Pack,
            Self::Midx { .. } => KeyKind::Midx,
            Self::RefLog { .. } => KeyKind::RefLog,
            Self::Purge(_) => KeyKind::Purge,
        }
    }

    /// The repository whose key this is, if any.
    #[must_use]
    pub fn repo(&self) -> Option<RepoIdHex> {
        match self {
            Self::RepoMeta { repo }
            | Self::Pack { repo, .. }
            | Self::Midx { repo, .. }
            | Self::RefLog { repo, .. } => Some(*repo),
            Self::Xorb(_)
            | Self::Shard(_)
            | Self::Registration { .. }
            | Self::XorbOwner { .. }
            | Self::Purge(_) => None,
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
            Key::RepoMeta { repo } => write!(f, "repos/{repo}/meta"),
            Key::Pack { repo, name, ext } => {
                write!(f, "repos/{repo}/packs/{name}.{}", ext.as_str())
            }
            Key::Midx {
                repo,
                generation,
                ext,
            } => write!(f, "repos/{repo}/midx/{generation}.{}", ext.as_str()),
            Key::RefLog { repo, seq } => write!(f, "repos/{repo}/reflog/{seq:020}"),
            Key::Purge(h) => write!(f, "purges/{}", hash_to_hex(h)),
        }
    }
}

/// A kind of object, for [`Bucket::list`] and [`Bucket::list_under`].
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
    /// [`Key::RepoMeta`].
    RepoMeta,
    /// [`Key::Pack`].
    Pack,
    /// [`Key::Midx`].
    Midx,
    /// [`Key::RefLog`].
    RefLog,
    /// [`Key::Purge`].
    Purge,
}

impl KeyKind {
    /// Whether keys of this kind belong to one repository.
    #[must_use]
    pub fn is_per_repo(self) -> bool {
        matches!(
            self,
            Self::RepoMeta | Self::Pack | Self::Midx | Self::RefLog
        )
    }
}

/// How an object is written: each kind has one way (the [crate
/// docs](crate#objects) list which).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteMode {
    /// [`Bucket::put`] or [`Bucket::put_stream`]: bytes that follow from
    /// the key, so a second write of the same size changes nothing.
    Put,
    /// [`Bucket::create`]: written once, the first writer's bytes kept.
    Create,
    /// [`Bucket::put_replace`]: overwritten whole.
    Replace,
}

impl Key {
    /// Refuses a write of `self` the way `how` writes, unless that is the
    /// kind's way. Another way could keep an old object of the same size
    /// and answer `Ok`, a silent no-op (`put` of a changed [`RepoMeta`]),
    /// or replace an object that is written once.
    pub(crate) fn check_write(&self, how: WriteMode) -> Result<(), StorageError> {
        let (expected, reason) = match self.kind() {
            KeyKind::RepoMeta => (
                WriteMode::Replace,
                "a repository's metadata record is written whole with put_replace",
            ),
            KeyKind::Registration | KeyKind::RefLog | KeyKind::Purge => (
                WriteMode::Create,
                "this record is written once with create, which keeps the first and says whether this call stored it",
            ),
            KeyKind::Xorb | KeyKind::Shard | KeyKind::XorbOwner | KeyKind::Pack | KeyKind::Midx => {
                (
                    WriteMode::Put,
                    "this object is written with put or put_stream; only a repository's metadata record is ever replaced",
                )
            }
        };
        // `create` of a key `put` writes is M1's contract and stays allowed:
        // it never replaces, and reports whether it stored.
        if how == expected || (how == WriteMode::Create && expected == WriteMode::Put) {
            Ok(())
        } else {
            Err(StorageError::InvalidKey {
                key: self.to_string(),
                reason,
            })
        }
    }
}

/// A body being written block by block ([`Bucket::put_stream`]). A failure
/// partway ends it with the error.
pub type BodyStream = BoxStream<'static, Result<Bytes, std::io::Error>>;

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
    /// verify content before putting, not rely on this check.
    /// [`StorageError::InvalidKey`] for a [`RepoMeta`], which only
    /// [`Bucket::put_replace`] writes, and for a record written once with
    /// [`Bucket::create`]. And [`StorageError::Io`] if the write fails.
    /// Nothing is stored on error.
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError>;

    /// Stores `bytes` under `key` unless the key exists, for objects whose
    /// bytes the key doesn't determine: a [`Registration`] carries a
    /// sequence number its writer chose. Returns whether this call stored
    /// them. Either way, on `Ok` the object under `key` is durable, and
    /// every writer then reads the same one, the first stored.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for a [`RepoMeta`], which only
    /// [`Bucket::put_replace`] writes, and [`StorageError::Io`] if the write
    /// fails. Nothing is stored on error.
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

    /// Stores `bytes` under `key`, replacing any object there, for a
    /// [`Key::RepoMeta`] only: the one record that changes (a rename or a
    /// label change rewrites it). On `Ok` the new bytes are durable; at
    /// every moment, a crash included, readers see the old object whole or
    /// the new one whole, never a mix or a truncated record.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for any other kind of key: every other
    /// object is written once, and replacing it would delete data outside
    /// garbage collection. [`StorageError::Io`] if the write fails; the old
    /// object is then left as it was.
    async fn put_replace(&self, key: Key, bytes: Bytes) -> Result<(), StorageError>;

    /// Stores `len` bytes read from `body` under `key`, holding one of its
    /// blocks at a time, never the object: what packs are written with. On
    /// `Ok` the bytes are durable.
    ///
    /// Callers yield blocks of at most [`STREAM_BLOCK_BYTES`]: the bucket
    /// writes each block as it comes and doesn't split it, so the block
    /// size bounds the memory a write holds (ruling M1-R27).
    ///
    /// The object is written once. If `key` exists with `len` bytes the
    /// first object stays, so its [`Bucket::last_modified`] remains its
    /// publish time; `body` is still read to check its length.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] if `key` isn't a kind [`Bucket::put`]
    /// writes,
    /// [`StorageError::LengthMismatch`] if `body` doesn't hold exactly `len`
    /// bytes, [`StorageError::SizeConflict`] if `key` exists with another
    /// size, and [`StorageError::Io`] if reading `body` or the write fails.
    /// Nothing is stored on error.
    async fn put_stream(&self, key: Key, body: BodyStream, len: u64) -> Result<(), StorageError>;

    /// Copies the object to a file at `path`, replacing any file there, and
    /// returns its size: how a node hydrates its pack cache. On `Ok` the
    /// file and its directory entry are durable, and at no moment does
    /// `path` hold a partial copy.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the key is absent and
    /// [`StorageError::Io`] if reading or writing fails; `path` is then
    /// left as it was.
    async fn get_to_file(&self, key: Key, path: &Path) -> Result<u64, StorageError>;

    /// When the object was stored, in seconds since the Unix epoch, or
    /// `None` if it does not exist: a reindexed pack's `created_at` (ADR
    /// 0004 item 4).
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the lookup fails.
    async fn last_modified(&self, key: Key) -> Result<Option<u64>, StorageError>;

    /// Every stored object of `kind` belonging to `repo`, in no particular
    /// order. Entries whose names are not a key of that kind are skipped.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] if `kind` is not a repository's
    /// ([`KeyKind::is_per_repo`]), and [`StorageError::Io`] if listing
    /// fails.
    async fn list_under(&self, repo: RepoIdHex, kind: KeyKind) -> Result<Vec<Key>, StorageError>;

    /// Deletes the object under `key`, a [`Key::Pack`] or [`Key::Midx`]
    /// only, and returns whether it existed: the pack sweep's one way to
    /// remove a Git pack's files once garbage collection has decided they
    /// may go (non-negotiable 3, ADR 0004 item 5). On `Ok` the removal is
    /// durable.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for any other kind of key: nothing else
    /// is ever deleted outside the garbage collection of later milestones.
    /// [`StorageError::Io`] if the removal fails.
    async fn delete(&self, key: &Key) -> Result<bool, StorageError>;

    /// Writes a few bytes to a temporary object outside every key and
    /// removes it, so a bucket that can't take writes (a full disk, a lost
    /// mount, revoked permissions) is found by `GET /readyz` (issue #49)
    /// rather than by the next upload. Nothing a key names is touched.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the write or the removal fails.
    async fn probe(&self) -> Result<(), StorageError>;
}
