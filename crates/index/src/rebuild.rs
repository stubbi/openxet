//! The index's scope and content tables rebuilt from the bucket alone
//! (ruling M1-R37, non-negotiable 2), and the same tables read from a live
//! index, so a test can prove the two agree. This is the core of `openxet reindex`
//! (<https://github.com/stubbi/openxet/issues/26>), which will write the
//! rebuilt rows into a fresh index.
//!
//! The rebuild reads:
//! - every stored xorb: its `xorbs` row (size and chunk table), its layout,
//!   and its dedup-eligible chunks as `dedup` candidates;
//! - every `xorb-owners/` record: a tenant uploaded a xorb;
//! - every `registrations/` record, in `(seq, shard bytes, tenant)` order,
//!   with the stored shard it names: the shard's files are registered to
//!   the record's scope (the first such row is kept per file and scope), its
//!   xorbs are owned by the record's tenant (ruling M1-R39), and its term
//!   order is the shard's; each file's first chunk is a `dedup` candidate
//!   for its first term's xorb, and each term's byte range comes from the
//!   stored xorb.
//!
//! Nothing else: not the index, and not anything a client sent that the
//! bucket doesn't hold.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use openxet_protocol::{MerkleHash, is_dedup_eligible, read_upload_shard, xorb_with_footer};
use openxet_storage::{Bucket, Key, KeyKind, Registration, XorbOwner};
use sqlx::Row as _;

use crate::types::{
    decode_term_bytes, decode_terms, encode_chunk_layout, encode_chunk_table, hash_from,
};
use crate::{ChunkTable, SqliteIndex, Term, TermBytes};

/// One file registered to one scope: a `file_scopes` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// The file's length in bytes.
    pub size: u64,
    /// Its reconstruction.
    pub terms: Vec<Term>,
    /// The client-asserted SHA-256.
    pub sha256: Option<[u8; 32]>,
    /// The shard whose registration the row comes from.
    pub shard: MerkleHash,
    /// Where each term is stored; `None` only for rows older than
    /// migration 0004, which a rebuild never produces.
    pub term_bytes: Option<TermBytes>,
    /// The number of the registration the row comes from.
    pub registered_seq: u64,
    /// The tenant of the registration the row comes from.
    pub registered_tenant: String,
}

/// The index's tables, as comparable rows: the scope tables and the content
/// tables a rebuild restores. Left out: `xorbs.ingested_seq`, which only
/// orders dedup offers between xorbs and which a rebuild may number afresh
/// (the review of C1 judged its loss harmless), and the `registration_seq`
/// counter, which `reindex` sets past the highest number in the bucket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeTables {
    /// `file_scopes`: per (file, scope).
    pub files: BTreeMap<(MerkleHash, String), FileRow>,
    /// `shard_registrations`: per (shard, scope, tenant), the sequence
    /// number.
    pub registrations: BTreeMap<(MerkleHash, String, String), u64>,
    /// `xorb_owners`: (xorb, tenant).
    pub owners: BTreeSet<(MerkleHash, String)>,
    /// `shard_xorbs`: per shard, its distinct xorbs in term order.
    pub shard_xorbs: BTreeMap<MerkleHash, Vec<MerkleHash>>,
    /// `xorbs`: per xorb, its stored size and encoded chunk table.
    pub xorbs: BTreeMap<MerkleHash, (u64, Vec<u8>)>,
    /// `xorb_layouts`: per xorb, its encoded chunk layout.
    pub layouts: BTreeMap<MerkleHash, Vec<u8>>,
    /// `dedup`: (chunk, xorb) candidates, the eligible chunks of every xorb
    /// and the first chunk of every registered file.
    pub dedup: BTreeSet<(MerkleHash, MerkleHash)>,
}

/// Why a rebuild failed: a bucket read, or an object that doesn't decode.
pub type RebuildError = Box<dyn std::error::Error + Send + Sync>;

/// Rebuilds the scope tables from `bucket` alone.
///
/// # Errors
///
/// A bucket failure, a record or shard or xorb that doesn't decode, or a
/// record naming a shard or xorb the bucket lacks.
pub async fn rebuild_scope_tables(bucket: &dyn Bucket) -> Result<ScopeTables, RebuildError> {
    let mut tables = ScopeTables::default();
    for key in bucket.list(KeyKind::XorbOwner).await? {
        let owner = XorbOwner::from_bytes(&key, &read(bucket, &key).await?)?;
        tables.owners.insert((owner.xorb, owner.tenant));
    }
    let mut registrations = Vec::new();
    for key in bucket.list(KeyKind::Registration).await? {
        registrations.push(Registration::from_bytes(&key, &read(bucket, &key).await?)?);
    }
    // The index breaks ties on the number the same way (`register_shard`):
    // numbers repeat only after an index was lost.
    registrations.sort_by(|a, b| {
        (a.seq, a.shard.as_bytes(), &a.tenant).cmp(&(b.seq, b.shard.as_bytes(), &b.tenant))
    });

    // Every stored xorb, re-read as ingest read it: its row, its layout and
    // its eligible chunks as dedup candidates.
    let mut stored_tables: HashMap<MerkleHash, ChunkTable> = HashMap::new();
    for key in bucket.list(KeyKind::Xorb).await? {
        let Key::Xorb(hash) = key else { continue };
        let stored = xorb_with_footer(&read(bucket, &key).await?)?;
        let table = ChunkTable {
            hashes: stored.chunks.hashes.into(),
            offsets: stored.chunk_offsets.into(),
        };
        tables.xorbs.insert(
            hash,
            (stored.bytes.len() as u64, encode_chunk_table(&table)?),
        );
        tables.layouts.insert(hash, encode_chunk_layout(&table));
        for (chunk, _) in table.hashes.iter().filter(|(c, _)| is_dedup_eligible(c)) {
            tables.dedup.insert((*chunk, hash));
        }
        stored_tables.insert(hash, table);
    }
    let table_of = |xorb: &MerkleHash| {
        stored_tables
            .get(xorb)
            .ok_or_else(|| RebuildError::from(format!("xorb {xorb} is missing from the bucket")))
    };

    for record in registrations {
        let shard = read_upload_shard(&read(bucket, &Key::Shard(record.shard)).await?[..])?;
        let referenced: Vec<MerkleHash> = shard.referenced_xorbs().copied().collect();
        for xorb in &referenced {
            table_of(xorb)?;
            tables.owners.insert((*xorb, record.tenant.clone()));
        }
        tables.shard_xorbs.entry(record.shard).or_insert(referenced);
        tables.registrations.insert(
            (record.shard, record.scope.clone(), record.tenant.clone()),
            record.seq,
        );
        let mut seen = HashSet::new();
        for file in shard.files.iter().filter(|f| seen.insert(f.file_hash)) {
            let terms: Vec<Term> = file
                .segments
                .iter()
                .map(|s| Term {
                    xorb: s.xorb,
                    chunk_start: s.chunk_start,
                    chunk_end: s.chunk_end,
                    unpacked_bytes: s.unpacked_bytes,
                })
                .collect();
            let term_bytes = terms
                .iter()
                .map(|t| {
                    let o = &table_of(&t.xorb)?.offsets;
                    Ok((o[t.chunk_start as usize], o[t.chunk_end as usize]))
                })
                .collect::<Result<_, RebuildError>>()?;
            if let Some(first) = terms.first() {
                let chunk = table_of(&first.xorb)?.hashes[first.chunk_start as usize].0;
                tables.dedup.insert((chunk, first.xorb));
            }
            tables
                .files
                .entry((file.file_hash, record.scope.clone()))
                .or_insert(FileRow {
                    size: terms.iter().map(|t| u64::from(t.unpacked_bytes)).sum(),
                    terms,
                    sha256: file.sha256,
                    shard: record.shard,
                    term_bytes: Some(term_bytes),
                    registered_seq: record.seq,
                    registered_tenant: record.tenant.clone(),
                });
        }
    }
    Ok(tables)
}

async fn read(bucket: &dyn Bucket, key: &Key) -> Result<bytes::Bytes, RebuildError> {
    bucket
        .get(key)
        .await?
        .ok_or_else(|| format!("{key} is missing from the bucket").into())
}

impl SqliteIndex {
    /// The live index's scope tables, for comparison with
    /// [`rebuild_scope_tables`].
    ///
    /// # Errors
    ///
    /// A database failure or a row that doesn't decode.
    pub async fn scope_tables(&self) -> Result<ScopeTables, RebuildError> {
        let mut tables = ScopeTables::default();
        let rows = sqlx::query(
            "SELECT file, scope, size, terms, sha256, shard, term_bytes, registered_seq, \
             registered_tenant FROM file_scopes",
        )
        .fetch_all(&self.reader)
        .await?;
        for row in rows {
            let terms = decode_terms(row.try_get::<&[u8], _>(3)?)?;
            let term_bytes = row
                .try_get::<Option<&[u8]>, _>(6)?
                .map(|b| decode_term_bytes(b, terms.len()))
                .transpose()?;
            let sha256 = row
                .try_get::<Option<&[u8]>, _>(4)?
                .map(<[u8; 32]>::try_from)
                .transpose()?;
            tables.files.insert(
                (hash_from(row.try_get(0)?)?, row.try_get(1)?),
                FileRow {
                    size: u64::try_from(row.try_get::<i64, _>(2)?)?,
                    terms,
                    sha256,
                    shard: hash_from(row.try_get(5)?)?,
                    term_bytes,
                    registered_seq: u64::try_from(row.try_get::<i64, _>(7)?)?,
                    registered_tenant: row.try_get(8)?,
                },
            );
        }
        for row in
            sqlx::query("SELECT shard, scope, tenant, registered_seq FROM shard_registrations")
                .fetch_all(&self.reader)
                .await?
        {
            tables.registrations.insert(
                (
                    hash_from(row.try_get(0)?)?,
                    row.try_get(1)?,
                    row.try_get(2)?,
                ),
                u64::try_from(row.try_get::<i64, _>(3)?)?,
            );
        }
        for row in sqlx::query("SELECT xorb, tenant FROM xorb_owners")
            .fetch_all(&self.reader)
            .await?
        {
            tables
                .owners
                .insert((hash_from(row.try_get(0)?)?, row.try_get(1)?));
        }
        for row in sqlx::query("SELECT shard, xorb FROM shard_xorbs ORDER BY shard, pos")
            .fetch_all(&self.reader)
            .await?
        {
            tables
                .shard_xorbs
                .entry(hash_from(row.try_get(0)?)?)
                .or_default()
                .push(hash_from(row.try_get(1)?)?);
        }
        for row in sqlx::query("SELECT hash, size, chunk_table FROM xorbs")
            .fetch_all(&self.reader)
            .await?
        {
            tables.xorbs.insert(
                hash_from(row.try_get(0)?)?,
                (u64::try_from(row.try_get::<i64, _>(1)?)?, row.try_get(2)?),
            );
        }
        for row in sqlx::query("SELECT xorb, chunk_layout FROM xorb_layouts")
            .fetch_all(&self.reader)
            .await?
        {
            tables
                .layouts
                .insert(hash_from(row.try_get(0)?)?, row.try_get(1)?);
        }
        for row in sqlx::query("SELECT chunk, xorb FROM dedup")
            .fetch_all(&self.reader)
            .await?
        {
            tables
                .dedup
                .insert((hash_from(row.try_get(0)?)?, hash_from(row.try_get(1)?)?));
        }
        Ok(tables)
    }
}
