//! The bucket ref log (design §5.3, ADR 0004 item 3): derived from the
//! database's `ref_log` rows, never written independently, so that after a
//! total database loss `reindex` rebuilds refs at most the documented RPO
//! (60 s) stale. The server derives it every [`crate::MAINTENANCE_INTERVAL`].
//!
//! Each derivation writes the rows past the highest one the bucket holds as
//! [`RefLogRecord`]s, written once and keyed by their last row. Two nodes
//! deriving at once may write overlapping records; `reindex` reads every
//! record and keeps each row once, by its sequence number. On SQLite rows
//! commit in sequence order (writes are serialized), so no row can appear
//! below one already derived. That ordering is SQLite's only: a Postgres
//! sequence hands numbers out before commit, so a row can commit below one
//! already derived, and the Postgres index of M3 must derive by commit
//! order instead (deferred to M3 with the Postgres migrations).

use std::sync::Arc;

use openxet_index::{RefLogRow, RepoId};
use openxet_storage::{Key, KeyKind, RefLogEntry, RefLogRecord, RepoIdHex};

use crate::{Engine, GitError};

/// The most rows one bucket ref log record holds.
pub const REF_LOG_RECORD_MAX_ENTRIES: usize = 1000;

/// Writes `repo`'s `ref_log` rows that the bucket ref log lacks to the
/// bucket, and returns how many it wrote.
///
/// # Errors
///
/// [`GitError::Index`] and [`GitError::Storage`]; rows not yet written are
/// written by the next call.
pub async fn derive_bucket_ref_log(engine: &Engine, repo: RepoId) -> Result<u64, GitError> {
    let lock = derived_lock(engine, repo);
    let mut derived = lock.lock().await;
    let repo_hex = repo_hex(repo)?;
    let head = match *derived {
        Some(head) => head,
        None => bucket_head(engine, repo_hex).await?,
    };
    *derived = Some(head);
    let rows = engine.index.ref_log_since(repo, head).await?;
    let mut written = 0;
    for chunk in rows.chunks(REF_LOG_RECORD_MAX_ENTRIES) {
        let Some(last) = chunk.last() else { continue };
        let record = RefLogRecord {
            repo: repo_hex,
            seq: last.seq,
            entries: chunk.iter().map(entry).collect(),
        };
        engine
            .bucket
            .create(&record.key(), record.to_bytes())
            .await?;
        *derived = Some(last.seq);
        written += chunk.len() as u64;
    }
    Ok(written)
}

/// The highest `ref_log` row the bucket ref log holds for `repo`, listed
/// from the bucket, since another node may have derived further than this
/// one knows: what the ref log's retention may prune through (ADR 0004
/// item 3).
pub(crate) async fn derived_head(engine: &Engine, repo: RepoId) -> Result<u64, GitError> {
    let lock = derived_lock(engine, repo);
    let mut derived = lock.lock().await;
    let head = bucket_head(engine, repo_hex(repo)?).await?;
    *derived = Some(derived.map_or(head, |known| known.max(head)));
    Ok(head)
}

/// `repo`'s entry in [`Engine`]'s derivation state, made on first use.
fn derived_lock(engine: &Engine, repo: RepoId) -> Arc<tokio::sync::Mutex<Option<u64>>> {
    let mut all = engine
        .derived
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(all.entry(repo).or_default())
}

/// Every row of `repo`'s bucket ref log, each once, in sequence order.
///
/// # Errors
///
/// [`GitError::Storage`], with
/// [`openxet_storage::StorageError::CorruptRecord`] for a record that isn't
/// one.
pub(crate) async fn bucket_rows(
    engine: &Engine,
    repo: RepoId,
) -> Result<Vec<RefLogEntry>, GitError> {
    let keys = engine
        .bucket
        .list_under(repo_hex(repo)?, KeyKind::RefLog)
        .await?;
    let mut rows = std::collections::BTreeMap::new();
    for key in keys {
        let Some(bytes) = engine.bucket.get(&key).await? else {
            continue;
        };
        for e in RefLogRecord::from_bytes(&key, &bytes)?.entries {
            rows.entry(e.seq).or_insert(e);
        }
    }
    Ok(rows.into_values().collect())
}

/// The highest sequence number among `repo`'s bucket records: each key
/// carries its last row's.
async fn bucket_head(engine: &Engine, repo: RepoIdHex) -> Result<u64, GitError> {
    Ok(engine
        .bucket
        .list_under(repo, KeyKind::RefLog)
        .await?
        .iter()
        .filter_map(|k| match k {
            Key::RefLog { seq, .. } => Some(*seq),
            _ => None,
        })
        .max()
        .unwrap_or(0))
}

pub(crate) fn repo_hex(repo: RepoId) -> Result<RepoIdHex, GitError> {
    Ok(RepoIdHex::parse(&repo.to_string())?)
}

fn entry(row: &RefLogRow) -> RefLogEntry {
    RefLogEntry {
        seq: row.seq,
        ref_name: row.name.clone(),
        old: row.old.map(|o| o.to_string()),
        new: row.new.map(|o| o.to_string()),
        actor: row.actor.clone(),
        request: row.request.clone(),
        at: row.at,
    }
}
