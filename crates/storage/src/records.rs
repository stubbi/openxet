//! The records that make the bucket the source of truth for who may read
//! what (ruling M1-R37, design §5.3).
//!
//! Each is one JSON object with a format version `v`, written before the
//! index records the same fact. A reader refuses a version it doesn't know
//! and any field it doesn't know: a later format bumps `v`, so `reindex`
//! never half-reads a record.

use bytes::Bytes;
use openxet_protocol::{MerkleHash, hash_from_hex, hash_to_hex};
use serde::{Deserialize, Serialize};

use crate::{Key, StorageError};

/// The record format this build writes and reads.
const VERSION: u32 = 1;

/// A shard registered to a scope, for a tenant, at
/// [`Key::registration`]: `{"v":1,"scope":…,"tenant":…,"shard":…,"seq":…}`.
///
/// The files it registers are those of the stored shard
/// (`shards/{shard hex}`), each kept as the scope's reconstruction unless a
/// registration with a lower `seq` already registered that file to the
/// scope. The xorbs the shard's files reference are owned by the tenant
/// from then on (ruling M1-R39). The record is written once and never
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

fn to_json(record: &impl Serialize) -> Bytes {
    Bytes::from(
        serde_json::to_vec(record)
            .unwrap_or_else(|_| unreachable!("a record has only string and integer fields")),
    )
}

fn corrupt(key: &Key, reason: impl Into<String>) -> StorageError {
    StorageError::CorruptRecord {
        key: Box::new(*key),
        reason: reason.into(),
        newer: false,
    }
}

fn from_json<T: serde::de::DeserializeOwned>(key: &Key, bytes: &[u8]) -> Result<T, StorageError> {
    let Version { v } = serde_json::from_slice(bytes)
        .map_err(|e| corrupt(key, format!("not a versioned JSON record ({e})")))?;
    if v != VERSION {
        return Err(StorageError::CorruptRecord {
            key: Box::new(*key),
            reason: format!("format version {v}, but this build reads version {VERSION}"),
            newer: v > VERSION,
        });
    }
    serde_json::from_slice(bytes).map_err(|e| corrupt(key, e.to_string()))
}

fn parse_hash(key: &Key, hex: &str) -> Result<MerkleHash, StorageError> {
    hash_from_hex(hex).map_err(|_| corrupt(key, format!("{hex:?} is not a Xet hash")))
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
