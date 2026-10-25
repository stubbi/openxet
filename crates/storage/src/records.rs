//! The records that make the bucket the source of truth for who may read
//! what (ruling M1-R37), for repositories and their refs (ruling M2-R2, ADR
//! 0004 item 3) and for purged files (ruling M2-R13); design §5.3.
//!
//! Each is one JSON object with a format version `v`, written before the
//! index records the same fact. A reader refuses a version it doesn't know
//! and any field it doesn't know: a later format bumps `v`, so `reindex`
//! never half-reads a record.

use bytes::Bytes;
use openxet_protocol::{MerkleHash, hash_from_hex, hash_to_hex};
use serde::{Deserialize, Serialize};

use crate::{Key, RepoIdHex, StorageError};

/// The record format this build writes and reads.
const VERSION: u32 = 1;

/// A shard registered to a scope, for a tenant, at
/// [`Key::registration`]: `{"v":1,"scope":…,"tenant":…,"shard":…,"seq":…}`.
///
/// The files it registers are those of the stored shard
/// (`shards/{shard hex}`), each kept as the scope's reconstruction unless a
/// registration with a lower `(seq, shard bytes, tenant)` already
/// registered that file to the scope. The xorbs the shard's files
/// reference are owned by the tenant from then on (ruling M1-R39). The record is written once and never
/// changed: a registration repeated after a failure reuses it, sequence
/// number included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The scope the files are registered to: who may read them.
    pub scope: String,
    /// The tenant the scope belongs to: who owns the referenced xorbs.
    pub tenant: String,
    /// The registered shard.
    pub shard: MerkleHash,
    /// Orders registrations: newer ones have a higher number. Unique among
    /// registrations allocated by one index.
    pub seq: u64,
}

/// A xorb uploaded by a tenant, at [`Key::xorb_owner`]:
/// `{"v":1,"tenant":…,"xorb":…}`. Its bytes depend only on its key, so
/// writing it again changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XorbOwner {
    /// The tenant the xorb was uploaded under.
    pub tenant: String,
    /// The xorb.
    pub xorb: MerkleHash,
}

/// A repository's metadata, at [`Key::RepoMeta`]:
/// `{"v":1,"id":…,"namespace":…,"name":…,"private":…,"labels":[…],"created_at":…}`,
/// plus `"deleted_at":…` once the repository is deleted.
///
/// The one record the bucket ever replaces ([`crate::Bucket::put_replace`]):
/// a rename, a label change or a deletion rewrites it before the index
/// records the change (ruling M2-R12), so `reindex` rebuilds every
/// repository row from these records, and skips a deleted one rather than
/// resurrect it. A deleted repository's data still leaves only through GC
/// (non-negotiable 3), so the record stays, tombstoned.
///
/// `deleted_at` is the one optional field a version 1 record may carry, an
/// exception to "a later format bumps `v`": it is left out until set, so
/// every record written before it existed reads unchanged, and no release
/// has shipped a reader that would refuse it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMeta {
    /// The repository's immutable id (ruling M2-R2).
    pub id: RepoIdHex,
    /// Its namespace, as written.
    pub namespace: String,
    /// Its name, as written.
    pub name: String,
    /// Whether the repository is private: only callers the authorizer
    /// grants `read` on it may see it.
    pub private: bool,
    /// Its labels (ADR 0010), such as `model` or `dataset`.
    pub labels: Vec<String>,
    /// When it was created, in seconds since the Unix epoch.
    pub created_at: u64,
    /// When it was deleted, in seconds since the Unix epoch; `None` while it
    /// exists. A create that lost its name, or whose index write failed,
    /// tombstones its own record too.
    pub deleted_at: Option<u64>,
}

/// Ref changes copied from the database's `ref_log` rows (ADR 0004 item 3),
/// at [`Key::RefLog`]: `{"v":1,"repo":…,"seq":…,"entries":[…]}`.
///
/// Derived from those rows, never written independently, and written once
/// ([`crate::Bucket::create`]). Its key's `seq` is its last entry's, so the
/// highest stored key is how far the bucket's ref log reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLogRecord {
    /// The repository whose refs changed.
    pub repo: RepoIdHex,
    /// The last entry's sequence number.
    pub seq: u64,
    /// The changes, in increasing `seq` order; never empty.
    pub entries: Vec<RefLogEntry>,
}

/// One ref change of a [`RefLogRecord`]: one `ref_log` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLogEntry {
    /// The row's sequence number.
    pub seq: u64,
    /// The full ref name, such as `refs/heads/main`.
    pub ref_name: String,
    /// The object id before, as 40 or 64 lowercase hex digits; `None` for
    /// a created ref.
    pub old: Option<String>,
    /// The object id after; `None` for a deleted ref.
    pub new: Option<String>,
    /// Who made the change.
    pub actor: String,
    /// The request that made it.
    pub request: String,
    /// When, in seconds since the Unix epoch.
    pub at: u64,
}

/// A file purged from every scope (ruling M2-R13), at [`Key::Purge`]:
/// `{"v":1,"file":…,"actor":…,"at":…}`.
///
/// Written once ([`crate::Bucket::create`]) before any index row is
/// removed, so `reindex` skips the file even if the purge was interrupted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeRecord {
    /// The purged file's Xet hash.
    pub file: MerkleHash,
    /// Who purged it.
    pub actor: String,
    /// When, in seconds since the Unix epoch.
    pub at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrationV1 {
    v: u32,
    scope: String,
    tenant: String,
    shard: String,
    seq: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct XorbOwnerV1 {
    v: u32,
    tenant: String,
    xorb: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoMetaV1 {
    v: u32,
    id: String,
    namespace: String,
    name: String,
    private: bool,
    labels: Vec<String>,
    created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted_at: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefLogRecordV1 {
    v: u32,
    repo: String,
    seq: u64,
    entries: Vec<RefLogEntryV1>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefLogEntryV1 {
    seq: u64,
    #[serde(rename = "ref")]
    ref_name: String,
    old: Option<String>,
    new: Option<String>,
    actor: String,
    request: String,
    at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PurgeV1 {
    v: u32,
    file: String,
    actor: String,
    at: u64,
}

/// Only the version, read first so a newer record is reported as newer
/// rather than as malformed.
#[derive(Deserialize)]
struct Version {
    v: u32,
}

impl Registration {
    /// Where the record is stored.
    #[must_use]
    pub fn key(&self) -> Key {
        Key::registration(&self.scope, &self.tenant, self.shard)
    }

    /// The record's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        to_json(&RegistrationV1 {
            v: VERSION,
            scope: self.scope.clone(),
            tenant: self.tenant.clone(),
            shard: hash_to_hex(&self.shard),
            seq: self.seq,
        })
    }

    /// Reads the record stored under `key`.
    ///
    /// # Errors
    ///
    /// [`StorageError::CorruptRecord`] if `bytes` isn't a version 1
    /// registration record or names another key.
    pub fn from_bytes(key: &Key, bytes: &[u8]) -> Result<Self, StorageError> {
        let raw: RegistrationV1 = from_json(key, bytes)?;
        let record = Self {
            shard: parse_hash(key, &raw.shard)?,
            scope: raw.scope,
            tenant: raw.tenant,
            seq: raw.seq,
        };
        check_key(key, &record.key())?;
        Ok(record)
    }
}

impl XorbOwner {
    /// Where the record is stored.
    #[must_use]
    pub fn key(&self) -> Key {
        Key::xorb_owner(&self.tenant, self.xorb)
    }

    /// The record's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        to_json(&XorbOwnerV1 {
            v: VERSION,
            tenant: self.tenant.clone(),
            xorb: hash_to_hex(&self.xorb),
        })
    }

    /// Reads the record stored under `key`.
    ///
    /// # Errors
    ///
    /// [`StorageError::CorruptRecord`] if `bytes` isn't a version 1 xorb
    /// owner record or names another key.
    pub fn from_bytes(key: &Key, bytes: &[u8]) -> Result<Self, StorageError> {
        let raw: XorbOwnerV1 = from_json(key, bytes)?;
        let record = Self {
            xorb: parse_hash(key, &raw.xorb)?,
            tenant: raw.tenant,
        };
        check_key(key, &record.key())?;
        Ok(record)
    }
}

impl RepoMeta {
    /// Where the record is stored.
    #[must_use]
    pub fn key(&self) -> Key {
        Key::RepoMeta { repo: self.id }
    }

    /// The record's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        to_json(&RepoMetaV1 {
            v: VERSION,
            id: self.id.to_string(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            private: self.private,
            labels: self.labels.clone(),
            created_at: self.created_at,
            deleted_at: self.deleted_at,
        })
    }

    /// Reads the record stored under `key`.
    ///
    /// # Errors
    ///
    /// [`StorageError::CorruptRecord`] if `bytes` isn't a version 1
    /// repository metadata record or names another key.
    pub fn from_bytes(key: &Key, bytes: &[u8]) -> Result<Self, StorageError> {
        let raw: RepoMetaV1 = from_json(key, bytes)?;
        let record = Self {
            id: parse_repo(key, &raw.id)?,
            namespace: raw.namespace,
            name: raw.name,
            private: raw.private,
            labels: raw.labels,
            created_at: raw.created_at,
            deleted_at: raw.deleted_at,
        };
        check_key(key, &record.key())?;
        Ok(record)
    }
}

impl RefLogRecord {
    /// Where the record is stored.
    #[must_use]
    pub fn key(&self) -> Key {
        Key::RefLog {
            repo: self.repo,
            seq: self.seq,
        }
    }

    /// The record's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        to_json(&RefLogRecordV1 {
            v: VERSION,
            repo: self.repo.to_string(),
            seq: self.seq,
            entries: self
                .entries
                .iter()
                .map(|e| RefLogEntryV1 {
                    seq: e.seq,
                    ref_name: e.ref_name.clone(),
                    old: e.old.clone(),
                    new: e.new.clone(),
                    actor: e.actor.clone(),
                    request: e.request.clone(),
                    at: e.at,
                })
                .collect(),
        })
    }

    /// Reads the record stored under `key`.
    ///
    /// # Errors
    ///
    /// [`StorageError::CorruptRecord`] if `bytes` isn't a version 1 ref log
    /// record, names another key, has no entries or entries out of order,
    /// doesn't end at its own `seq`, or holds an object id that isn't 40 or
    /// 64 lowercase hex digits.
    pub fn from_bytes(key: &Key, bytes: &[u8]) -> Result<Self, StorageError> {
        let raw: RefLogRecordV1 = from_json(key, bytes)?;
        let record = Self {
            repo: parse_repo(key, &raw.repo)?,
            seq: raw.seq,
            entries: raw
                .entries
                .into_iter()
                .map(|e| RefLogEntry {
                    seq: e.seq,
                    ref_name: e.ref_name,
                    old: e.old,
                    new: e.new,
                    actor: e.actor,
                    request: e.request,
                    at: e.at,
                })
                .collect(),
        };
        check_key(key, &record.key())?;
        if record.entries.last().map(|e| e.seq) != Some(record.seq) {
            return Err(corrupt(key, "its last entry's seq is not the record's"));
        }
        if record.entries.windows(2).any(|w| w[0].seq >= w[1].seq) {
            return Err(corrupt(key, "its entries are not in increasing seq order"));
        }
        let oids = record
            .entries
            .iter()
            .flat_map(|e| [e.old.as_deref(), e.new.as_deref()])
            .flatten();
        if let Some(bad) = oids.into_iter().find(|oid| !is_oid(oid)) {
            return Err(corrupt(
                key,
                format!("{bad:?} is not an object id of 40 or 64 lowercase hex digits"),
            ));
        }
        Ok(record)
    }
}

impl PurgeRecord {
    /// Where the record is stored.
    #[must_use]
    pub fn key(&self) -> Key {
        Key::Purge(self.file)
    }

    /// The record's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        to_json(&PurgeV1 {
            v: VERSION,
            file: hash_to_hex(&self.file),
            actor: self.actor.clone(),
            at: self.at,
        })
    }

    /// Reads the record stored under `key`.
    ///
    /// # Errors
    ///
    /// [`StorageError::CorruptRecord`] if `bytes` isn't a version 1 purge
    /// record or names another key.
    pub fn from_bytes(key: &Key, bytes: &[u8]) -> Result<Self, StorageError> {
        let raw: PurgeV1 = from_json(key, bytes)?;
        let record = Self {
            file: parse_hash(key, &raw.file)?,
            actor: raw.actor,
            at: raw.at,
        };
        check_key(key, &record.key())?;
        Ok(record)
    }
}

fn to_json(record: &impl Serialize) -> Bytes {
    Bytes::from(serde_json::to_vec(record).unwrap_or_else(|_| {
        unreachable!("a record has only string, integer, boolean and list fields")
    }))
}

fn corrupt(key: &Key, reason: impl Into<String>) -> StorageError {
    StorageError::CorruptRecord {
        key: Box::new(key.clone()),
        reason: reason.into(),
        newer: false,
    }
}

fn from_json<T: serde::de::DeserializeOwned>(key: &Key, bytes: &[u8]) -> Result<T, StorageError> {
    let Version { v } = serde_json::from_slice(bytes)
        .map_err(|e| corrupt(key, format!("not a versioned JSON record ({e})")))?;
    if v != VERSION {
        return Err(StorageError::CorruptRecord {
            key: Box::new(key.clone()),
            reason: format!("format version {v}, but this build reads version {VERSION}"),
            newer: v > VERSION,
        });
    }
    serde_json::from_slice(bytes).map_err(|e| corrupt(key, e.to_string()))
}

fn parse_hash(key: &Key, hex: &str) -> Result<MerkleHash, StorageError> {
    hash_from_hex(hex).map_err(|_| corrupt(key, format!("{hex:?} is not a Xet hash")))
}

fn parse_repo(key: &Key, hex: &str) -> Result<RepoIdHex, StorageError> {
    RepoIdHex::parse(hex).map_err(|_| corrupt(key, format!("{hex:?} is not a repository id")))
}

/// Whether `oid` is a Git object id: SHA-1 or SHA-256, in lowercase hex.
fn is_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn check_key(stored_under: &Key, named: &Key) -> Result<(), StorageError> {
    if stored_under == named {
        Ok(())
    } else {
        Err(corrupt(
            stored_under,
            format!("its contents belong under {named}"),
        ))
    }
}
