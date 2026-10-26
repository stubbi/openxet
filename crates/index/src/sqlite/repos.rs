//! The SQLite implementation of [`Repos`]. A write of one statement
//! (`create_repo`, `set_labels`, `audit`) runs without an explicit
//! transaction, since a single SQLite statement is atomic; writes of
//! several take `BEGIN IMMEDIATE`.

use openxet_protocol::MerkleHash;
use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;

use super::{SqliteIndex, to_i64, to_u64};
use crate::types::{decode_terms, hash_from};
use crate::{
    AuditEntry, FileRecord, FileSummary, IndexError, Page, Repo, RepoFilter, RepoId, Repos,
};

const REPO_COLUMNS: &str = "id, namespace, name, private, labels, created_at, full_repack_gen";

/// The unique key of `{namespace}/{name}`: ASCII lowercase, since names are
/// ASCII (ruling M2-R2). Computed here rather than in SQL, so every database
/// agrees on it.
fn name_key(namespace: &str, name: &str) -> String {
    format!(
        "{}/{}",
        namespace.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    )
}

fn repo_from(row: &SqliteRow) -> Result<Repo, IndexError> {
    let labels: &str = row.try_get(4)?;
    Ok(Repo {
        id: RepoId::from_slice(row.try_get(0)?, "a repository id")?,
        namespace: row.try_get(1)?,
        name: row.try_get(2)?,
        private: row.try_get(3)?,
        labels: serde_json::from_str(labels)
            .map_err(|e| IndexError::Corrupt(format!("repository labels {labels:?}: {e}")))?,
        created_at: to_u64(row.try_get(5)?, "repository creation time")?,
        full_repack_gen: to_u64(row.try_get(6)?, "full-repack generation")?,
    })
}

fn labels_json(labels: &[String]) -> String {
    serde_json::Value::from(labels).to_string()
}

#[async_trait::async_trait]
impl Repos for SqliteIndex {
    async fn create_repo(&self, repo: &Repo) -> Result<(), IndexError> {
        let created_at = to_i64(repo.created_at, "repository creation time")?;
        let generation = to_i64(repo.full_repack_gen, "full-repack generation")?;
        let inserted = sqlx::query(
            "INSERT INTO repos \
             (id, namespace, name, name_key, private, labels, created_at, full_repack_gen) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (name_key) DO NOTHING",
        )
        .bind(&repo.id.0[..])
        .bind(&repo.namespace)
        .bind(&repo.name)
        .bind(name_key(&repo.namespace, &repo.name))
        .bind(repo.private)
        .bind(labels_json(&repo.labels))
        .bind(created_at)
        .bind(generation)
        .execute(&self.writer)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Err(IndexError::NameTaken {
                name: format!("{}/{}", repo.namespace, repo.name),
            });
        }
        Ok(())
    }

    async fn repo_by_name(&self, namespace: &str, name: &str) -> Result<Option<Repo>, IndexError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REPO_COLUMNS} FROM repos WHERE name_key = ?"
        )))
        .bind(name_key(namespace, name))
        .fetch_optional(&self.reader)
        .await?
        .as_ref()
        .map(repo_from)
        .transpose()
    }

    async fn repo_by_id(&self, id: RepoId) -> Result<Option<Repo>, IndexError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REPO_COLUMNS} FROM repos WHERE id = ?"
        )))
        .bind(&id.0[..])
        .fetch_optional(&self.reader)
        .await?
        .as_ref()
        .map(repo_from)
        .transpose()
    }

    async fn list_repos(
        &self,
        filter: RepoFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Repo>, IndexError> {
        if limit == 0 {
            // An empty page would carry no cursor and read as the end.
            return Err(IndexError::InvalidInput(
                "a repository listing needs a limit of at least 1".to_owned(),
            ));
        }
        // One more than asked, to know whether a next page exists.
        let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        // A namespace's keys are those from `ns/` up to `ns0` exclusive ('0'
        // follows '/' in ASCII): a range the unique index on name_key
        // serves, ordered as the listing is.
        let (from, until) = match &filter.namespace {
            Some(ns) => {
                let ns = ns.to_ascii_lowercase();
                (Some(format!("{ns}/")), Some(format!("{ns}0")))
            }
            None => (None, None),
        };
        // `json_each` is SQLite's; the Postgres implementation of M3 reads the
        // same JSON array with `jsonb_array_elements_text`.
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REPO_COLUMNS}, name_key FROM repos r \
             WHERE (? IS NULL OR name_key > ?) \
             AND (? IS NULL OR name_key >= ?) AND (? IS NULL OR name_key < ?) \
             AND (? IS NULL OR EXISTS (SELECT 1 FROM json_each(r.labels) WHERE value = ?)) \
             ORDER BY name_key LIMIT ?"
        )))
        .bind(cursor)
        .bind(cursor)
        .bind(&from)
        .bind(&from)
        .bind(&until)
        .bind(&until)
        .bind(&filter.label)
        .bind(&filter.label)
        .bind(fetch)
        .fetch_all(&self.reader)
        .await?;
        let more = rows.len() > limit;
        let page = &rows[..rows.len().min(limit)];
        let next = match page.last() {
            Some(last) if more => Some(last.try_get::<String, _>(7)?),
            _ => None,
        };
        let items = page.iter().map(repo_from).collect::<Result<_, _>>()?;
        Ok(Page { items, next })
    }

    async fn set_labels(
        &self,
        id: RepoId,
        expected: &[String],
        labels: &[String],
    ) -> Result<(), IndexError> {
        // Every write stores `labels_json` of the list, so equal lists are
        // equal text and the comparison can run in the one statement.
        let updated = sqlx::query("UPDATE repos SET labels = ? WHERE id = ? AND labels = ?")
            .bind(labels_json(labels))
            .bind(&id.0[..])
            .bind(labels_json(expected))
            .execute(&self.writer)
            .await?
            .rows_affected();
        if updated == 0 {
            return Err(match self.repo_by_id(id).await? {
                Some(_) => IndexError::LabelsChanged { repo: id },
                None => IndexError::UnknownRepo { repo: id },
            });
        }
        Ok(())
    }

    async fn delete_repo(&self, id: RepoId) -> Result<bool, IndexError> {
        let id = &id.0[..];
        let mut tx = self.begin_write().await?;
        for statement in [
            "DELETE FROM refs WHERE repo = ?",
            "DELETE FROM repo_leases WHERE repo = ?",
            "DELETE FROM reindex_unplaced WHERE repo = ?",
            "UPDATE packs SET state = 'superseded', superseded_at = unixepoch() \
             WHERE repo = ? AND state = 'committed'",
            "UPDATE packs SET state = 'lost' WHERE repo = ? AND state = 'recovered'",
        ] {
            sqlx::query(statement).bind(id).execute(&mut *tx).await?;
        }
        let deleted = sqlx::query("DELETE FROM repos WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(deleted > 0)
    }

    async fn file_by_sha256(
        &self,
        scope: &str,
        sha256: &[u8; 32],
    ) -> Result<Option<FileRecord>, IndexError> {
        let row = sqlx::query(
            "SELECT file, size, terms FROM file_scopes WHERE scope = ? AND sha256 = ? \
             ORDER BY registered_seq, file LIMIT 1",
        )
        .bind(scope)
        .bind(&sha256[..])
        .fetch_optional(&self.reader)
        .await?;
        row.map(|row| {
            Ok(FileRecord {
                hash: hash_from(row.try_get::<&[u8], _>(0)?)?,
                size: to_u64(row.try_get(1)?, "file size")?,
                terms: decode_terms(row.try_get::<&[u8], _>(2)?)?,
            })
        })
        .transpose()
    }

    async fn file_summary(
        &self,
        scope: &str,
        file: &MerkleHash,
    ) -> Result<Option<FileSummary>, IndexError> {
        let row: Option<(i64, Option<Vec<u8>>)> =
            sqlx::query_as("SELECT size, sha256 FROM file_scopes WHERE file = ? AND scope = ?")
                .bind(file.as_bytes())
                .bind(scope)
                .fetch_optional(&self.reader)
                .await?;
        row.map(|(size, sha256)| {
            let size = u64::try_from(size)
                .map_err(|_| IndexError::Corrupt(format!("a file's size is {size}")))?;
            let sha256 = sha256
                .map(|bytes| {
                    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
                        IndexError::Corrupt(format!(
                            "a file's SHA-256 is {} bytes, not 32",
                            bytes.len()
                        ))
                    })
                })
                .transpose()?;
            Ok(FileSummary { size, sha256 })
        })
        .transpose()
    }

    async fn file_scope_count(&self, file: &MerkleHash) -> Result<u64, IndexError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_scopes WHERE file = ?")
            .bind(file.as_bytes())
            .fetch_one(&self.reader)
            .await?;
        to_u64(count, "file scope count")
    }

    async fn purge_file(&self, file: &MerkleHash) -> Result<u64, IndexError> {
        let mut tx = self.begin_write().await?;
        let removed = sqlx::query("DELETE FROM file_scopes WHERE file = ?")
            .bind(file.as_bytes())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query("INSERT INTO purges (file, at) VALUES (?, unixepoch()) ON CONFLICT DO NOTHING")
            .bind(file.as_bytes())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(removed)
    }

    async fn audit(&self, entry: &AuditEntry) -> Result<(), IndexError> {
        sqlx::query(
            "INSERT INTO audit_log (actor, action, repo, detail, at) \
             VALUES (?, ?, ?, ?, unixepoch())",
        )
        .bind(&entry.actor)
        .bind(&entry.action)
        .bind(entry.repo.as_ref().map(|r| &r.0[..]))
        .bind(entry.detail.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }
}
