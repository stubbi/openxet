//! The SQLite implementation of [`GitStore`]. Every transaction that checks
//! a row and then writes starts with `BEGIN IMMEDIATE`, so the check holds
//! until it commits; on Postgres (M3) the same checks take their rows `FOR
//! UPDATE`. `mark_pack` is one guarded statement, atomic on its own.

use std::collections::HashSet;
use std::time::Duration;

use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;
use sqlx::{Sqlite, Transaction};

use super::{HASHES_PER_QUERY, SqliteIndex, to_i64, to_u64, with_placeholders};
use crate::{
    CommitError, CommitTx, GitStore, IndexError, Lease, Oid, Pack, PackId, PackRecord, PackState,
    RefLogRow, RefUpdate, Reindexed, RepackPublish, RepoId, Snapshot, Unplaced, UnplacedKind,
};

/// The database clock in Unix milliseconds, which leases are compared on
/// (ADR 0004 item 5). A macro, so queries using it stay `&'static str`.
macro_rules! now_ms {
    () => {
        "CAST(unixepoch('subsec') * 1000 AS INTEGER)"
    };
}

fn oid_from(bytes: Option<&[u8]>) -> Result<Option<Oid>, IndexError> {
    bytes
        .map(|b| Oid::from_slice(b, "an object id"))
        .transpose()
}

fn pack_from(row: &SqliteRow) -> Result<Pack, IndexError> {
    Ok(Pack {
        id: PackId::from_slice(row.try_get(0)?, "a pack id")?,
        repo: RepoId::from_slice(row.try_get(1)?, "a repository id")?,
        name: row.try_get(2)?,
        state: PackState::parse(row.try_get(3)?)?,
        created_at: to_u64(row.try_get(4)?, "pack creation time")?,
        bytes: to_u64(row.try_get(5)?, "pack size")?,
        midx_gen: row
            .try_get::<Option<i64>, _>(6)?
            .map(|g| to_u64(g, "multi-pack-index generation"))
            .transpose()?,
    })
}

/// Refuses commands that can't form a commit transaction, before any write.
fn check_updates(updates: &[RefUpdate]) -> Result<(), IndexError> {
    if updates.is_empty() {
        return Err(IndexError::InvalidInput(
            "a commit transaction needs at least one ref command".to_owned(),
        ));
    }
    let mut names = HashSet::with_capacity(updates.len());
    for u in updates {
        if u.old.is_none() && u.new.is_none() {
            return Err(IndexError::InvalidInput(format!(
                "the command for {} neither creates, updates nor deletes it",
                u.name
            )));
        }
        if !names.insert(u.name.as_str()) {
            return Err(IndexError::InvalidInput(format!(
                "{} is named by two commands of one transaction",
                u.name
            )));
        }
    }
    Ok(())
}

/// Fails with [`IndexError::UnknownRepo`] unless `repo` exists, read inside
/// `tx`.
async fn require_repo(tx: &mut Transaction<'_, Sqlite>, repo: RepoId) -> Result<(), IndexError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM repos WHERE id = ?")
        .bind(&repo.0[..])
        .fetch_optional(&mut **tx)
        .await?;
    found.map(drop).ok_or(IndexError::UnknownRepo { repo })
}

/// Applies one ref command's compare-and-swap, reporting whether it
/// matched.
async fn apply(
    tx: &mut Transaction<'_, Sqlite>,
    repo: RepoId,
    u: &RefUpdate,
) -> Result<bool, IndexError> {
    let repo = &repo.0[..];
    let query = match (u.old, u.new) {
        (None, Some(new)) => sqlx::query(
            "INSERT INTO refs (repo, name, oid) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(repo)
        .bind(&u.name)
        .bind(new.0.to_vec()),
        (Some(old), Some(new)) => {
            sqlx::query("UPDATE refs SET oid = ? WHERE repo = ? AND name = ? AND oid = ?")
                .bind(new.0.to_vec())
                .bind(repo)
                .bind(&u.name)
                .bind(old.0.to_vec())
        }
        (Some(old), None) => {
            sqlx::query("DELETE FROM refs WHERE repo = ? AND name = ? AND oid = ?")
                .bind(repo)
                .bind(&u.name)
                .bind(old.0.to_vec())
        }
        (None, None) => {
            return Err(IndexError::InvalidInput(format!(
                "the command for {} changes nothing",
                u.name
            )));
        }
    };
    Ok(query.execute(&mut **tx).await?.rows_affected() == 1)
}

/// Moves pack `id` of `repo` from `from` to `to` inside `tx`, reporting
/// whether it was in state `from`.
async fn transition(
    tx: &mut Transaction<'_, Sqlite>,
    repo: RepoId,
    id: PackId,
    from: PackState,
    to: PackState,
) -> Result<bool, IndexError> {
    let superseded = to == PackState::Superseded;
    let committed = to == PackState::Committed;
    let moved = sqlx::query(
        "UPDATE packs SET state = ?, \
         superseded_at = CASE WHEN ? THEN unixepoch() ELSE superseded_at END, \
         committed_at = CASE WHEN ? THEN unixepoch() ELSE committed_at END \
         WHERE id = ? AND repo = ? AND state = ?",
    )
    .bind(to.as_str())
    .bind(superseded)
    .bind(committed)
    .bind(&id.0[..])
    .bind(&repo.0[..])
    .bind(from.as_str())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(moved == 1)
}

/// The checks and writes of a commit transaction (ADR 0004 item 3) inside
/// `db`, which the caller commits: the generation, every ref
/// compare-and-swap, the writer's pack, and one `ref_log` row per command.
async fn commit_in(db: &mut Transaction<'_, Sqlite>, tx: CommitTx<'_>) -> Result<(), CommitError> {
    check_updates(tx.updates)?;
    let expected = to_i64(tx.full_repack_gen, "full-repack generation")?;
    let generation: Option<i64> =
        sqlx::query_scalar("SELECT full_repack_gen FROM repos WHERE id = ?")
            .bind(&tx.repo.0[..])
            .fetch_optional(&mut **db)
            .await?;
    match generation {
        None => return Err(IndexError::UnknownRepo { repo: tx.repo }.into()),
        Some(g) if g != expected => return Err(CommitError::Fenced),
        Some(_) => {}
    }
    for u in tx.updates {
        if !apply(db, tx.repo, u).await? {
            return Err(CommitError::RefMismatch(u.name.clone()));
        }
    }
    if let Some(pack) = tx.pack
        && !transition(db, tx.repo, pack, PackState::Pending, PackState::Committed).await?
    {
        return Err(CommitError::PackNotPending);
    }
    insert_ref_log(db, tx.repo, tx.updates, tx.actor, tx.request).await?;
    Ok(())
}

/// One `ref_log` row per command, stamped with the database clock.
async fn insert_ref_log(
    db: &mut Transaction<'_, Sqlite>,
    repo: RepoId,
    updates: &[RefUpdate],
    actor: &str,
    request: &str,
) -> Result<(), IndexError> {
    for u in updates {
        sqlx::query(
            "INSERT INTO ref_log (repo, ref, old, new, actor, request, at) \
             VALUES (?, ?, ?, ?, ?, ?, unixepoch())",
        )
        .bind(&repo.0[..])
        .bind(&u.name)
        .bind(u.old.map(|o| o.0.to_vec()))
        .bind(u.new.map(|o| o.0.to_vec()))
        .bind(actor)
        .bind(request)
        .execute(&mut **db)
        .await?;
    }
    Ok(())
}

/// Deletes the repository's commit, tag and edge entries whose object is in
/// `covered`, returning how many.
async fn delete_covered(
    db: &mut Transaction<'_, Sqlite>,
    repo: RepoId,
    covered: &[Oid],
) -> Result<u64, IndexError> {
    let mut cleared = 0;
    for batch in covered.chunks(HASHES_PER_QUERY) {
        let mut query = sqlx::query(with_placeholders(
            "DELETE FROM reindex_unplaced WHERE repo = ? AND oid IN (",
            "?",
            batch.len(),
            ")",
        ))
        .bind(&repo.0[..]);
        for oid in batch {
            query = query.bind(oid.0.to_vec());
        }
        cleared += query.execute(&mut **db).await?.rows_affected();
    }
    Ok(cleared)
}

#[async_trait::async_trait]
impl GitStore for SqliteIndex {
    async fn snapshot(&self, repo: RepoId) -> Result<Snapshot, IndexError> {
        let id = &repo.0[..];
        // A WAL read transaction sees one database state from its first
        // read on (https://sqlite.org/wal.html#concurrency), so refs, packs
        // and the generation agree (ADR 0004 item 4).
        let mut tx = self.reader.begin().await?;
        let generation: Option<i64> =
            sqlx::query_scalar("SELECT full_repack_gen FROM repos WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let generation = generation.ok_or(IndexError::UnknownRepo { repo })?;
        let refs = sqlx::query("SELECT name, oid FROM refs WHERE repo = ? ORDER BY name")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(|row| {
                let oid = Oid::from_slice(row.try_get(1)?, "an object id")?;
                Ok((row.try_get(0)?, oid))
            })
            .collect::<Result<_, IndexError>>()?;
        let packs = sqlx::query(
            "SELECT id, repo, name, state, created_at, bytes, midx_gen FROM packs \
             WHERE repo = ? AND state = 'committed' ORDER BY created_at, id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(pack_from)
        .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(Snapshot {
            refs,
            packs,
            full_repack_gen: to_u64(generation, "full-repack generation")?,
        })
    }

    async fn insert_pack(&self, pack: &Pack) -> Result<(), IndexError> {
        if pack.state != PackState::Pending {
            return Err(IndexError::InvalidInput(format!(
                "pack {} is inserted as {}, but every pack starts pending",
                pack.id,
                pack.state.as_str()
            )));
        }
        let created_at = to_i64(pack.created_at, "pack creation time")?;
        let bytes = to_i64(pack.bytes, "pack size")?;
        let midx_gen = pack
            .midx_gen
            .map(|g| to_i64(g, "multi-pack-index generation"))
            .transpose()?;
        let mut tx = self.begin_write().await?;
        require_repo(&mut tx, pack.repo).await?;
        let inserted = sqlx::query(
            "INSERT INTO packs (id, repo, name, state, created_at, bytes, midx_gen) \
             VALUES (?, ?, ?, 'pending', ?, ?, ?) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&pack.id.0[..])
        .bind(&pack.repo.0[..])
        .bind(&pack.name)
        .bind(created_at)
        .bind(bytes)
        .bind(midx_gen)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Err(IndexError::InvalidInput(format!(
                "pack id {} is already recorded; every attempt takes a new id",
                pack.id
            )));
        }
        tx.commit().await?;
        Ok(())
    }

    async fn commit(&self, tx: CommitTx<'_>) -> Result<(), CommitError> {
        let mut db = self.begin_write().await?;
        commit_in(&mut db, tx).await?;
        db.commit().await?;
        Ok(())
    }

    async fn mark_pack(
        &self,
        id: PackId,
        from: PackState,
        to: PackState,
    ) -> Result<bool, IndexError> {
        use PackState::{Expired, Lost, Pending, Recovered};
        if !matches!((from, to), (Pending, Lost | Expired) | (Recovered, Lost)) {
            return Err(IndexError::InvalidInput(format!(
                "pack {id} can't move from {} to {} outside a commit or repack transaction",
                from.as_str(),
                to.as_str()
            )));
        }
        let moved = sqlx::query("UPDATE packs SET state = ? WHERE id = ? AND state = ?")
            .bind(to.as_str())
            .bind(&id.0[..])
            .bind(from.as_str())
            .execute(&self.writer)
            .await?
            .rows_affected();
        Ok(moved == 1)
    }

    async fn ref_log_since(
        &self,
        repo: RepoId,
        since_seq: u64,
    ) -> Result<Vec<RefLogRow>, IndexError> {
        let since = to_i64(since_seq, "ref log sequence number")?;
        sqlx::query(
            "SELECT seq, ref, old, new, actor, request, at FROM ref_log \
             WHERE repo = ? AND seq > ? ORDER BY seq",
        )
        .bind(&repo.0[..])
        .bind(since)
        .fetch_all(&self.reader)
        .await?
        .iter()
        .map(|row| {
            Ok(RefLogRow {
                seq: to_u64(row.try_get(0)?, "ref log sequence number")?,
                repo,
                name: row.try_get(1)?,
                old: oid_from(row.try_get(2)?)?,
                new: oid_from(row.try_get(3)?)?,
                actor: row.try_get(4)?,
                request: row.try_get(5)?,
                at: to_u64(row.try_get(6)?, "ref log time")?,
            })
        })
        .collect()
    }

    async fn acquire_lease(
        &self,
        repo: RepoId,
        holder: &str,
        ttl: Duration,
    ) -> Result<Option<Lease>, IndexError> {
        let ttl_ms = i64::try_from(ttl.as_millis()).map_err(|_| {
            IndexError::InvalidInput(format!("a lease of {ttl:?} is too long to store"))
        })?;
        let mut tx = self.begin_write().await?;
        require_repo(&mut tx, repo).await?;
        let row = sqlx::query(concat!(
            "INSERT INTO repo_leases (repo, holder, epoch, expires_at) VALUES (?, ?, 1, ",
            now_ms!(),
            " + ?) ON CONFLICT (repo) DO UPDATE SET holder = excluded.holder, \
             epoch = repo_leases.epoch + 1, expires_at = excluded.expires_at \
             WHERE repo_leases.holder = excluded.holder OR repo_leases.expires_at <= ",
            now_ms!(),
            " RETURNING epoch, expires_at"
        ))
        .bind(&repo.0[..])
        .bind(holder)
        .bind(ttl_ms)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        row.map(|row| {
            Ok(Lease {
                repo,
                holder: holder.to_owned(),
                epoch: to_u64(row.try_get(0)?, "lease epoch")?,
                expires_at_ms: to_u64(row.try_get(1)?, "lease expiry")?,
            })
        })
        .transpose()
    }

    async fn publish_repack(&self, p: RepackPublish<'_>) -> Result<(), CommitError> {
        if p.ref_log_high_water.is_some() && p.outputs.is_empty() {
            return Err(IndexError::InvalidInput(
                "a full repack publishes at least one pack".to_owned(),
            )
            .into());
        }
        let epoch = to_i64(p.epoch, "lease epoch")?;
        let high_water = p
            .ref_log_high_water
            .map(|hw| to_i64(hw, "ref log sequence number"))
            .transpose()?;
        let repo = &p.repo.0[..];
        let mut tx = self.begin_write().await?;
        let leased: Option<i64> = sqlx::query_scalar(concat!(
            "SELECT 1 FROM repo_leases WHERE repo = ? AND holder = ? AND epoch = ? \
             AND expires_at > ",
            now_ms!()
        ))
        .bind(repo)
        .bind(p.holder)
        .bind(epoch)
        .fetch_optional(&mut *tx)
        .await?;
        if leased.is_none() {
            return Err(CommitError::Fenced);
        }
        if let Some(high_water) = high_water {
            let latest: i64 =
                sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM ref_log WHERE repo = ?")
                    .bind(repo)
                    .fetch_one(&mut *tx)
                    .await?;
            if latest > high_water {
                return Err(CommitError::Fenced);
            }
        }
        for &out in p.outputs {
            if !transition(
                &mut tx,
                p.repo,
                out,
                PackState::Pending,
                PackState::Committed,
            )
            .await?
            {
                return Err(CommitError::PackNotPending);
            }
        }
        for &input in p.inputs {
            let superseded = transition(
                &mut tx,
                p.repo,
                input,
                PackState::Committed,
                PackState::Superseded,
            )
            .await?;
            if !superseded {
                return Err(CommitError::Fenced);
            }
        }
        if high_water.is_some() {
            sqlx::query("UPDATE repos SET full_repack_gen = full_repack_gen + 1 WHERE id = ?")
                .bind(repo)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn unplaced(&self, repo: RepoId) -> Result<Vec<Unplaced>, IndexError> {
        sqlx::query(
            "SELECT kind, oid, pack, found_at FROM reindex_unplaced WHERE repo = ? \
             ORDER BY found_at, rowid",
        )
        .bind(&repo.0[..])
        .fetch_all(&self.reader)
        .await?
        .iter()
        .map(|row| {
            Ok(Unplaced {
                repo,
                kind: UnplacedKind::from_parts(row.try_get(0)?, oid_from(row.try_get(1)?)?)?,
                pack: PackId::from_slice(row.try_get(2)?, "a pack id")?,
                found_at: to_u64(row.try_get(3)?, "unplaced entry time")?,
            })
        })
        .collect()
    }

    async fn record_unplaced(&self, rows: &[Unplaced]) -> Result<(), IndexError> {
        let mut tx = self.begin_write().await?;
        for row in rows {
            let (kind, oid) = row.kind.parts();
            sqlx::query(
                "INSERT INTO reindex_unplaced (repo, kind, oid, pack, found_at) \
                 VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(&row.repo.0[..])
            .bind(kind)
            .bind(oid.map(|o| o.0.to_vec()))
            .bind(&row.pack.0[..])
            .bind(to_i64(row.found_at, "unplaced entry time")?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn clear_unplaced(&self, repo: RepoId, covered: &[Oid]) -> Result<u64, IndexError> {
        let mut tx = self.begin_write().await?;
        let cleared = delete_covered(&mut tx, repo, covered).await?;
        tx.commit().await?;
        Ok(cleared)
    }

    async fn packs(&self, repo: RepoId) -> Result<Vec<PackRecord>, IndexError> {
        sqlx::query(
            "SELECT id, repo, name, state, created_at, bytes, midx_gen, committed_at, \
             superseded_at FROM packs WHERE repo = ? ORDER BY created_at, id",
        )
        .bind(&repo.0[..])
        .fetch_all(&self.reader)
        .await?
        .iter()
        .map(|row| {
            let time = |i: usize, what: &str| -> Result<Option<u64>, IndexError> {
                row.try_get::<Option<i64>, _>(i)?
                    .map(|t| to_u64(t, what))
                    .transpose()
            };
            Ok(PackRecord {
                pack: pack_from(row)?,
                committed_at: time(7, "pack commit time")?,
                superseded_at: time(8, "pack supersede time")?,
            })
        })
        .collect()
    }

    async fn delete_pack(&self, id: PackId, state: PackState) -> Result<bool, IndexError> {
        use PackState::{Expired, Lost, Superseded};
        if !matches!(state, Superseded | Lost | Expired) {
            return Err(IndexError::InvalidInput(format!(
                "pack {id} is {}; only superseded, lost and expired pack rows are deleted",
                state.as_str()
            )));
        }
        let deleted = sqlx::query("DELETE FROM packs WHERE id = ? AND state = ?")
            .bind(&id.0[..])
            .bind(state.as_str())
            .execute(&self.writer)
            .await?
            .rows_affected();
        Ok(deleted == 1)
    }

    async fn ref_log_head(&self, repo: RepoId) -> Result<u64, IndexError> {
        let head: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM ref_log WHERE repo = ?")
                .bind(&repo.0[..])
                .fetch_one(&self.reader)
                .await?;
        to_u64(head, "ref log sequence number")
    }

    async fn prune_ref_log(
        &self,
        repo: RepoId,
        before: u64,
        through_seq: u64,
    ) -> Result<u64, IndexError> {
        Ok(
            sqlx::query("DELETE FROM ref_log WHERE repo = ? AND at < ? AND seq <= ?")
                .bind(&repo.0[..])
                .bind(to_i64(before, "ref log time")?)
                .bind(to_i64(through_seq, "ref log sequence number")?)
                .execute(&self.writer)
                .await?
                .rows_affected(),
        )
    }

    async fn restore(
        &self,
        tx: CommitTx<'_>,
        recovered: &[PackId],
        covered: &[Oid],
    ) -> Result<(), CommitError> {
        if tx.pack.is_some() {
            return Err(IndexError::InvalidInput(
                "a restore publishes no pack of its own; it commits recovered packs".to_owned(),
            )
            .into());
        }
        let mut db = self.begin_write().await?;
        commit_in(&mut db, tx).await?;
        for &pack in recovered {
            let moved = transition(
                &mut db,
                tx.repo,
                pack,
                PackState::Recovered,
                PackState::Committed,
            )
            .await?;
            if !moved {
                return Err(CommitError::Fenced);
            }
            sqlx::query(
                "DELETE FROM reindex_unplaced WHERE repo = ? AND kind = 'pack' AND pack = ?",
            )
            .bind(&tx.repo.0[..])
            .bind(&pack.0[..])
            .execute(&mut *db)
            .await?;
        }
        delete_covered(&mut db, tx.repo, covered).await?;
        db.commit().await?;
        Ok(())
    }

    async fn discard_unplaced(&self, repo: RepoId) -> Result<u64, IndexError> {
        Ok(sqlx::query("DELETE FROM reindex_unplaced WHERE repo = ?")
            .bind(&repo.0[..])
            .execute(&self.writer)
            .await?
            .rows_affected())
    }

    async fn reindex(&self, repo: RepoId, r: Reindexed<'_>) -> Result<(), IndexError> {
        if let Some(p) = r.packs.iter().find(|p| {
            !matches!(p.state, PackState::Committed | PackState::Recovered) || p.repo != repo
        }) {
            return Err(IndexError::InvalidInput(format!(
                "reindex writes pack {} as {} of repository {}; it writes only committed and \
                 recovered packs of {repo}",
                p.id,
                p.state.as_str(),
                p.repo
            )));
        }
        let id = &repo.0[..];
        let mut tx = self.begin_write().await?;
        require_repo(&mut tx, repo).await?;
        for table in [
            "DELETE FROM refs WHERE repo = ?",
            "DELETE FROM ref_log WHERE repo = ?",
            "DELETE FROM packs WHERE repo = ?",
            "DELETE FROM reindex_unplaced WHERE repo = ?",
        ] {
            sqlx::query(table).bind(id).execute(&mut *tx).await?;
        }
        for (name, oid) in r.refs {
            sqlx::query("INSERT INTO refs (repo, name, oid) VALUES (?, ?, ?)")
                .bind(id)
                .bind(name)
                .bind(oid.0.to_vec())
                .execute(&mut *tx)
                .await?;
        }
        for row in r.ref_log {
            let inserted = sqlx::query(
                "INSERT INTO ref_log (seq, repo, ref, old, new, actor, request, at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (seq) DO NOTHING",
            )
            .bind(to_i64(row.seq, "ref log sequence number")?)
            .bind(id)
            .bind(&row.name)
            .bind(row.old.map(|o| o.0.to_vec()))
            .bind(row.new.map(|o| o.0.to_vec()))
            .bind(&row.actor)
            .bind(&row.request)
            .bind(to_i64(row.at, "ref log time")?)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if inserted == 0 {
                return Err(IndexError::InvalidInput(format!(
                    "ref log row {} of repository {repo} is numbered like a row of another \
                     repository; reindex every repository of a lost database before any of them \
                     takes writes",
                    row.seq
                )));
            }
        }
        insert_ref_log(&mut tx, repo, r.corrections, r.actor, r.request).await?;
        for p in r.packs {
            sqlx::query(
                "INSERT INTO packs (id, repo, name, state, created_at, bytes, midx_gen) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.id.0[..])
            .bind(id)
            .bind(&p.name)
            .bind(p.state.as_str())
            .bind(to_i64(p.created_at, "pack creation time")?)
            .bind(to_i64(p.bytes, "pack size")?)
            .bind(
                p.midx_gen
                    .map(|g| to_i64(g, "multi-pack-index generation"))
                    .transpose()?,
            )
            .execute(&mut *tx)
            .await?;
        }
        for u in r.unplaced {
            let (kind, oid) = u.kind.parts();
            sqlx::query(
                "INSERT INTO reindex_unplaced (repo, kind, oid, pack, found_at) \
                 VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(kind)
            .bind(oid.map(|o| o.0.to_vec()))
            .bind(&u.pack.0[..])
            .bind(to_i64(u.found_at, "unplaced entry time")?)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE repos SET full_repack_gen = full_repack_gen + 1 WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
