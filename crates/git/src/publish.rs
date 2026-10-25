//! Publish, then commit (ADR 0004 item 3; design §5.3, bucket first): a
//! writer's pack row is inserted `pending`, its files are uploaded, and only
//! then does the fenced commit transaction move the refs and the pack to
//! `committed`. Any failure after the row exists marks the pack `lost`; a
//! node that dies first leaves it `pending` for GC to expire. Pushes use it
//! now, Hub commits later; repacks follow the same pattern with their own
//! publish transaction (`repack`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use openxet_index::{
    CommitError, CommitTx, HubIndex, IndexError, Pack, PackId, PackState, RefUpdate, RepoId,
};
use openxet_storage::{Bucket, Key, PackExt, PackName, RepoIdHex, StorageError};

/// With the `fault-injection` feature, a publish by this actor stops after
/// the upload, before the commit transaction, as if the node died there.
#[cfg(feature = "fault-injection")]
pub const FAULT_AFTER_UPLOAD_ACTOR: &str = "openxet-test/fault-after-upload";

/// With the `fault-injection` feature, a publish by this actor stops after
/// the commit transaction, before the writer is answered.
#[cfg(feature = "fault-injection")]
pub const FAULT_AFTER_COMMIT_ACTOR: &str = "openxet-test/fault-after-commit";

/// A pack's files on local disk: `{dir}/{name}.pack`, `.idx`, and `.rev`
/// when git wrote one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFiles {
    /// The directory holding them.
    pub dir: PathBuf,
    /// The pack's name.
    pub name: PackName,
}

impl PackFiles {
    /// The file with extension `ext`.
    #[must_use]
    pub fn path(&self, ext: PackExt) -> PathBuf {
        self.dir.join(format!("{}.{}", self.name, ext.as_str()))
    }
}

/// One writer's publish: its pack, if it wrote one, and the ref commands
/// its commit transaction applies.
#[derive(Debug, Clone, Copy)]
pub struct Publication<'a> {
    /// The repository.
    pub repo: RepoId,
    /// The writer's pack.
    pub pack: Option<&'a PackFiles>,
    /// The ref commands.
    pub updates: &'a [RefUpdate],
    /// The full-repack generation of the writer's snapshot.
    pub full_repack_gen: u64,
    /// Who writes: the principal's subject.
    pub actor: &'a str,
    /// The request id.
    pub request: &'a str,
}

/// Why a publish changed no ref.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The commit transaction refused; the pack is `lost`.
    #[error(transparent)]
    Commit(#[from] CommitError),
    /// The index failed before the transaction; a pack row that was written
    /// is `lost`.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// An upload failed; the pack is `lost`.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A pack file couldn't be read.
    #[error("failed to read the pushed pack {}: {source}", .path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: std::io::Error,
    },
    /// A fault-injection point stopped the publish, as a crash would: no
    /// clean-up ran.
    #[cfg(feature = "fault-injection")]
    #[error("fault injected: {0}")]
    Injected(&'static str),
}

/// Runs publishes against one index and bucket.
#[derive(Clone)]
pub struct Publisher {
    index: Arc<dyn HubIndex>,
    bucket: Arc<dyn Bucket>,
}

impl std::fmt::Debug for Publisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publisher").finish_non_exhaustive()
    }
}

impl Publisher {
    /// A publisher for `index` and `bucket`.
    #[must_use]
    pub fn new(index: Arc<dyn HubIndex>, bucket: Arc<dyn Bucket>) -> Self {
        Self { index, bucket }
    }

    /// Publishes `p`: inserts its pack row `pending`, uploads `.idx`, `.rev`
    /// (if any) and `.pack`, then runs the commit transaction. Returns the
    /// committed pack row. On any failure after the insert, the pack is
    /// marked `lost`.
    ///
    /// # Errors
    ///
    /// [`PublishError`]; [`PublishError::Commit`] carries the transaction's
    /// reason (a moved ref, the repack fence, a pack no longer pending).
    pub async fn publish(&self, p: Publication<'_>) -> Result<Option<Pack>, PublishError> {
        let row = match p.pack {
            Some(files) => Some(self.insert(p.repo, files).await?),
            None => None,
        };
        let result = self.upload_and_commit(&p, row.as_ref()).await;
        match result {
            Ok(()) => Ok(row.map(|r| Pack {
                state: PackState::Committed,
                ..r
            })),
            #[cfg(feature = "fault-injection")]
            Err(e @ PublishError::Injected(_)) => Err(e),
            Err(e) => {
                if let Some(row) = &row {
                    self.mark_lost(row.id).await;
                }
                Err(e)
            }
        }
    }

    async fn insert(&self, repo: RepoId, files: &PackFiles) -> Result<Pack, PublishError> {
        let pack_path = files.path(PackExt::Pack);
        let bytes = tokio::fs::metadata(&pack_path)
            .await
            .map_err(|source| PublishError::Read {
                path: pack_path.clone(),
                source,
            })?
            .len();
        let row = Pack {
            id: PackId::generate(),
            repo,
            name: files.name.to_string(),
            state: PackState::Pending,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            bytes,
            midx_gen: None,
        };
        self.index.insert_pack(&row).await?;
        Ok(row)
    }

    async fn upload_and_commit(
        &self,
        p: &Publication<'_>,
        row: Option<&Pack>,
    ) -> Result<(), PublishError> {
        if let Some(files) = p.pack {
            let repo = RepoIdHex::parse(&p.repo.to_string())?;
            for ext in [PackExt::Idx, PackExt::Rev, PackExt::Pack] {
                let path = files.path(ext);
                if ext == PackExt::Rev && !path.exists() {
                    continue;
                }
                let key = Key::Pack {
                    repo,
                    name: files.name.clone(),
                    ext,
                };
                self.upload(key, &path).await?;
            }
        }
        #[cfg(feature = "fault-injection")]
        if p.actor == FAULT_AFTER_UPLOAD_ACTOR {
            return Err(PublishError::Injected("after upload, before the commit"));
        }
        self.index
            .commit(CommitTx {
                repo: p.repo,
                updates: p.updates,
                pack: row.map(|r| r.id),
                full_repack_gen: p.full_repack_gen,
                actor: p.actor,
                request: p.request,
            })
            .await?;
        #[cfg(feature = "fault-injection")]
        if p.actor == FAULT_AFTER_COMMIT_ACTOR {
            return Err(PublishError::Injected(
                "after the commit, before the answer",
            ));
        }
        Ok(())
    }

    async fn upload(&self, key: Key, path: &Path) -> Result<(), PublishError> {
        let read_err = |source| PublishError::Read {
            path: path.to_path_buf(),
            source,
        };
        let file = tokio::fs::File::open(path).await.map_err(read_err)?;
        let len = file.metadata().await.map_err(read_err)?.len();
        let body = Box::pin(tokio_util::io::ReaderStream::new(file));
        self.bucket.put_stream(key, body, len).await?;
        Ok(())
    }

    /// Marks a failed writer's pack `lost`, only if it is still `pending`
    /// (ADR 0004 item 3). If this fails too, the row stays `pending` and GC
    /// expires it.
    async fn mark_lost(&self, id: PackId) {
        if let Err(e) = self
            .index
            .mark_pack(id, PackState::Pending, PackState::Lost)
            .await
        {
            tracing::warn!(pack = %id, error = %e, "marking a failed writer's pack lost failed; GC expires it");
        }
    }
}
