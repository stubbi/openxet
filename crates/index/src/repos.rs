//! Repositories, file lookups by SHA-256, purges and the audit log: what the
//! Hub layer records about a repository beside its Git data (design §5.3,
//! rulings M2-R2, M2-R12, M2-R13).

use openxet_protocol::MerkleHash;

use crate::{FileRecord, IndexError, RepoId};

/// A repository row. The bucket's metadata record is written first and is
/// the source of truth (design §5.3); this row indexes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The immutable id (ruling M2-R2).
    pub id: RepoId,
    /// The namespace, as written. The caller validates it (M2-R2).
    pub namespace: String,
    /// The name, as written. `{namespace}/{name}` is unique ignoring ASCII
    /// case, across labels (ADR 0010 item 5).
    pub name: String,
    /// Whether the repository is private.
    pub private: bool,
    /// Its labels, in the caller's order. The caller validates them.
    pub labels: Vec<String>,
    /// When it was created, Unix seconds, as its bucket record says.
    pub created_at: u64,
    /// The full-repack generation, the repack fence of ADR 0004 item 5. A
    /// new repository starts at whatever the caller passes, normally 0.
    pub full_repack_gen: u64,
}

/// Which repositories [`Repos::list_repos`] returns. Each `Some` narrows
/// the listing; both `None` lists every repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFilter {
    /// Only repositories carrying this label, compared exactly.
    pub label: Option<String>,
    /// Only repositories in this namespace, compared ignoring ASCII case.
    pub namespace: Option<String>,
}

/// One page of a listing (ruling M2-R8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The entries, in listing order.
    pub items: Vec<T>,
    /// The cursor that continues the listing after the last entry, or
    /// `None` if no entry follows. Opaque: pass it back as it is. The Hub
    /// base64url-encodes it into its `cursor` parameter (ruling M2-R8).
    pub next: Option<String>,
}

/// An `audit_log` row to append (ruling M2-R12). The database stamps the
/// time and the sequence number.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    /// Who acted: the principal's subject. Never a credential.
    pub actor: String,
    /// What was done, such as `label.add` or `purge`.
    pub action: String,
    /// The repository acted on, if any.
    pub repo: Option<RepoId>,
    /// The action's particulars, stored as JSON.
    pub detail: serde_json::Value,
}

/// Repository rows and the Hub layer's lookups over the M1 file tables.
#[async_trait::async_trait]
pub trait Repos: Send + Sync + 'static {
    /// Records a new repository.
    ///
    /// # Errors
    ///
    /// [`IndexError::NameTaken`] if a repository of the same
    /// `{namespace}/{name}` ignoring ASCII case exists, and
    /// [`IndexError::Database`] if the write fails.
    async fn create_repo(&self, repo: &Repo) -> Result<(), IndexError>;

    /// The repository named `{namespace}/{name}`, ignoring ASCII case.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn repo_by_name(&self, namespace: &str, name: &str) -> Result<Option<Repo>, IndexError>;

    /// The repository with id `id`.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn repo_by_id(&self, id: RepoId) -> Result<Option<Repo>, IndexError>;

    /// Up to `limit` repositories matching `filter`, ordered by
    /// `{namespace}/{name}` ignoring ASCII case, starting after `cursor`
    /// (a [`Page::next`] of the same listing), or from the first.
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidInput`] for a `limit` of 0,
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn list_repos(
        &self,
        filter: RepoFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Repo>, IndexError>;

    /// Replaces the repository's labels with `labels` if they are still
    /// `expected`, compared as lists: a compare-and-swap, so two writers
    /// that read the same labels can't both change them and one lose the
    /// other's change, on one node or several.
    ///
    /// # Errors
    ///
    /// [`IndexError::LabelsChanged`] if the labels are no longer
    /// `expected`, [`IndexError::UnknownRepo`] if no repository has id
    /// `id`, and [`IndexError::Database`] if the write fails. Nothing is
    /// changed on error.
    async fn set_labels(
        &self,
        id: RepoId,
        expected: &[String],
        labels: &[String],
    ) -> Result<(), IndexError>;

    /// Deletes the repository's rows: the repository, its refs, lease and
    /// unplaced entries. Its packs are handed to GC, never deleted here
    /// (non-negotiable 3): committed packs become `superseded` and recovered
    /// ones `lost`, so GC deletes them after the grace period; pending ones
    /// can no longer commit and GC expires them (ADR 0004 item 5). Its
    /// `ref_log` rows and its scope's file registrations stay for GC too.
    /// Returns whether the repository existed.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] if the write fails; nothing is changed.
    async fn delete_repo(&self, id: RepoId) -> Result<bool, IndexError>;

    /// The file `scope` registered with SHA-256 `sha256`. The SHA-256 is
    /// client-asserted, so it is only ever looked up within the scope that
    /// registered it (design §5.4). Of several files claiming it, the
    /// earliest registration answers.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn file_by_sha256(
        &self,
        scope: &str,
        sha256: &[u8; 32],
    ) -> Result<Option<FileRecord>, IndexError>;

    /// The number of scopes `file` is registered to: what
    /// [`Repos::purge_file`] would remove now (ruling M2-R13).
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn file_scope_count(&self, file: &MerkleHash) -> Result<u64, IndexError>;

    /// Purges `file` (ruling M2-R13): in one transaction, removes its
    /// registration from every scope and records the purge. Dedup rows are
    /// left alone: they describe xorbs, not files, and no bucket record
    /// mirrors their deletion, so removing them would make the index
    /// diverge from a rebuild. The caller has written the bucket's purge
    /// record first and writes the audit row after. The `purges` row only
    /// marks the file: the bucket record, with its `through_seq`, is what
    /// `reindex` reads to decide which registrations to skip (ruling M2-R13
    /// as amended). Returns the number of
    /// scopes it was removed from.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] if the write fails; nothing is changed.
    async fn purge_file(&self, file: &MerkleHash) -> Result<u64, IndexError>;

    /// Appends `entry` to the audit log. Rows are never changed or deleted.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] if the write fails.
    async fn audit(&self, entry: &AuditEntry) -> Result<(), IndexError>;
}
