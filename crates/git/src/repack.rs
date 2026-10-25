//! Repacks on one leaseholder per repository (ADR 0004 item 5).
//!
//! A repack runs in a working copy built like a request's snapshot, from
//! the committed packs only, plus temporary keep refs
//! `refs/openxet/keep/<n>`: every old and new target in the `ref_log`
//! within the grace window, and every unplaced `commit`, `tag` and `edge`
//! entry (ruling R24), so that nothing a pending writer or an operator's
//! restore may still need is dropped. Right before git runs, every input
//! pack's mtime is pinned again to its row's `created_at`: the working copy
//! hard-links the node cache, and git may have freshened a pack's mtime
//! while serving. Outputs follow the pushed-pack pattern: rows `pending`,
//! files uploaded, then one publish transaction under the lease.
//!
//! - A geometric repack (`git repack --geometric=2 -d --write-midx
//!   --write-bitmap-index`) rolls packs up with `pack-objects --stdin-packs`,
//!   which keeps every object of its inputs and leaves cruft packs alone, so
//!   it drops nothing and takes no fence. It records its multi-pack-index
//!   generation on its outputs.
//! - A full repack (`git repack -a -d -b --cruft --cruft-expiration=<grace>`)
//!   puts what even the keep refs don't reach into a cruft pack, whose
//!   `.mtimes` expire objects by their pinned pack time. Its publish bumps
//!   `full_repack_gen`, failing every writer whose snapshot predates it, and
//!   aborts if the `ref_log` moved past the high-water mark read before the
//!   working copy: the fence in both directions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use openxet_index::{
    CommitError, Lease, Oid, Pack, PackId, PackState, RepackPublish, RepoId, UnplacedKind,
};
use openxet_storage::{Key, MidxExt, PackExt, PackName};

use crate::reflog::repo_hex;
use crate::snapshot::write_refs;
use crate::store::pin;
use crate::thin::{oid_lines, run};
use crate::{
    DEFAULT_GRACE, Engine, GitError, SnapshotRepo, blocking, check_grace, io_err, unix_now,
};

/// A pack's files in upload order: the `.pack` last, as the pushed-pack
/// path and hydration do, so a `.pack` in the bucket means its side files
/// are there.
const UPLOAD_ORDER: [PackExt; 5] = [
    PackExt::Idx,
    PackExt::Rev,
    PackExt::Bitmap,
    PackExt::Mtimes,
    PackExt::Pack,
];

/// The name git gives a working copy's multi-pack index.
const MIDX_FILE: &str = "multi-pack-index";

/// What a repack published.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepackReport {
    /// The packs it replaced, now `superseded`.
    pub superseded: Vec<Pack>,
    /// The packs it wrote, now `committed`. Empty when there was nothing to
    /// repack, and then nothing changed.
    pub published: Vec<Pack>,
    /// The multi-pack-index generation a geometric repack recorded.
    pub midx_gen: Option<u64>,
    /// How many keep refs the working copy held.
    pub keep_refs: usize,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Geometric,
    Full { grace: Duration },
}

/// A repack whose outputs are recorded `pending` and uploaded, waiting for
/// its publish transaction. Dropping it unpublished is what a node that
/// dies at this point does: the outputs stay `pending` until GC expires
/// them.
#[derive(Debug)]
pub struct Prepared {
    repo: RepoId,
    holder: String,
    epoch: u64,
    high_water: Option<u64>,
    inputs: Vec<Pack>,
    outputs: Vec<Pack>,
    midx_gen: Option<u64>,
    keep_refs: usize,
}

/// Runs a geometric repack of `repo` under `lease` and publishes it.
///
/// # Errors
///
/// [`GitError::RepackFenced`] if the lease was lost or the inputs changed,
/// with the outputs marked `lost`; [`GitError::GitFailed`],
/// [`GitError::Index`], [`GitError::Storage`] and [`GitError::Io`].
pub async fn repack_geometric(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
) -> Result<RepackReport, GitError> {
    prepare_geometric(engine, repo, lease)
        .await?
        .publish(engine)
        .await
}

/// Runs a full repack of `repo` under `lease`, expiring unreachable objects
/// older than `grace` into nothing, and publishes it.
///
/// # Errors
///
/// [`GitError::GraceTooShort`] below [`crate::MIN_GRACE`], and those of
/// [`repack_geometric`]; [`GitError::RepackFenced`] also when a ref changed
/// since the working copy was taken, which a later run retries.
pub async fn repack_full(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
    grace: Duration,
) -> Result<RepackReport, GitError> {
    prepare_full(engine, repo, lease, grace)
        .await?
        .publish(engine)
        .await
}

/// The first half of [`repack_geometric`]: everything up to the publish.
///
/// # Errors
///
/// Those of [`repack_geometric`] but [`GitError::RepackFenced`].
pub async fn prepare_geometric(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
) -> Result<Prepared, GitError> {
    prepare(engine, repo, lease, Kind::Geometric).await
}

/// The first half of [`repack_full`]: everything up to the publish.
///
/// # Errors
///
/// Those of [`repack_full`] but [`GitError::RepackFenced`].
pub async fn prepare_full(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
    grace: Duration,
) -> Result<Prepared, GitError> {
    check_grace(grace)?;
    prepare(engine, repo, lease, Kind::Full { grace }).await
}

impl Prepared {
    /// Publishes the repack in one transaction under its lease (ADR 0004
    /// item 5). On failure its outputs are marked `lost`.
    ///
    /// # Errors
    ///
    /// [`GitError::RepackFenced`] and [`GitError::Index`].
    pub async fn publish(self, engine: &Engine) -> Result<RepackReport, GitError> {
        if self.outputs.is_empty() {
            return Ok(RepackReport {
                keep_refs: self.keep_refs,
                ..RepackReport::default()
            });
        }
        let outputs: Vec<PackId> = self.outputs.iter().map(|p| p.id).collect();
        let inputs: Vec<PackId> = self.inputs.iter().map(|p| p.id).collect();
        let published = engine
            .index
            .publish_repack(RepackPublish {
                repo: self.repo,
                holder: &self.holder,
                epoch: self.epoch,
                outputs: &outputs,
                inputs: &inputs,
                ref_log_high_water: self.high_water,
            })
            .await;
        match published {
            Ok(()) => Ok(RepackReport {
                superseded: with_state(self.inputs, PackState::Superseded),
                published: with_state(self.outputs, PackState::Committed),
                midx_gen: self.midx_gen,
                keep_refs: self.keep_refs,
            }),
            Err(e) => {
                mark_lost(engine, &self.outputs).await;
                match e {
                    CommitError::Database(e) => Err(e.into()),
                    CommitError::Fenced
                    | CommitError::PackNotPending
                    | CommitError::RefMismatch(_) => Err(GitError::RepackFenced),
                }
            }
        }
    }
}

fn with_state(packs: Vec<Pack>, state: PackState) -> Vec<Pack> {
    packs.into_iter().map(|p| Pack { state, ..p }).collect()
}

async fn prepare(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
    kind: Kind,
) -> Result<Prepared, GitError> {
    if lease.repo != repo {
        return Err(GitError::RepackFenced);
    }
    // Read before the working copy: a ref change between the two then fails
    // the publish, to be retried, and can never slip past the fence.
    let high_water = match kind {
        Kind::Full { .. } => Some(engine.index.ref_log_head(repo).await?),
        Kind::Geometric => None,
    };
    let window = match kind {
        Kind::Full { grace } => grace,
        Kind::Geometric => DEFAULT_GRACE,
    };
    let snap = engine.snapshot(repo).await?;
    let keep = keep_roots(engine, &snap, window).await?;
    let pack_dir = snap.dir.path().join("objects/pack");
    let (before, after) = run_git_repack(engine, &snap, &keep, kind).await?;
    let inputs: Vec<Pack> = snap
        .packs
        .iter()
        .filter(|p| !after.contains(&p.name))
        .cloned()
        .collect();
    let mut new_names: Vec<&String> = after.iter().filter(|n| !before.contains(*n)).collect();
    new_names.sort();
    let mut prepared = Prepared {
        repo,
        holder: lease.holder.clone(),
        epoch: lease.epoch,
        high_water,
        inputs,
        outputs: Vec::new(),
        midx_gen: None,
        keep_refs: keep.len(),
    };
    if new_names.is_empty() {
        return Ok(prepared);
    }
    if matches!(kind, Kind::Geometric) && pack_dir.join(MIDX_FILE).is_file() {
        prepared.midx_gen = Some(next_midx_gen(engine, repo, lease.epoch).await?);
    }
    // Objects keep their age through a repack: the outputs carry the oldest
    // input's time, so cruft expiry and a discard (ADR 0004 item 4) treat a
    // repacked object as they treated it before. Objects that became
    // unreachable recently are held by the keep refs, not by their mtime.
    let created_at = prepared
        .inputs
        .iter()
        .map(|p| p.created_at)
        .min()
        .unwrap_or_else(unix_now);
    for name in new_names {
        let path = pack_dir.join(format!("{name}.pack"));
        let bytes = tokio::fs::metadata(&path)
            .await
            .map_err(io_err("stat", &path))?
            .len();
        let row = Pack {
            id: PackId::generate(),
            repo,
            name: name.clone(),
            state: PackState::Pending,
            created_at,
            bytes,
            midx_gen: prepared.midx_gen,
        };
        if let Err(e) = engine.index.insert_pack(&row).await {
            mark_lost(engine, &prepared.outputs).await;
            return Err(e.into());
        }
        prepared.outputs.push(row);
    }
    if let Err(e) = upload_outputs(engine, &prepared, &pack_dir).await {
        mark_lost(engine, &prepared.outputs).await;
        return Err(e);
    }
    Ok(prepared)
}

/// Adds the keep refs to the working copy `snap`, pins its packs' mtimes,
/// and runs git's repack of `kind` in it; returns the pack names before and
/// after.
async fn run_git_repack(
    engine: &Engine,
    snap: &SnapshotRepo,
    keep: &[Oid],
    kind: Kind,
) -> Result<(HashSet<String>, HashSet<String>), GitError> {
    let mut refs = snap.refs.clone();
    refs.extend(
        keep.iter()
            .enumerate()
            .map(|(n, oid)| (format!("refs/openxet/keep/{n}"), *oid)),
    );
    let root = snap.dir.path().to_path_buf();
    let pack_dir = root.join("objects/pack");
    let packs = snap.packs.clone();
    let before = {
        let pack_dir = pack_dir.clone();
        blocking(move || {
            write_refs(&root, &refs)?;
            repin(&pack_dir, &packs)?;
            pack_names(&pack_dir)
        })
        .await?
    };
    let expiration;
    let args: Vec<&str> = match kind {
        Kind::Geometric => vec![
            "repack",
            "-q",
            "--geometric=2",
            "-d",
            "--write-midx",
            "--write-bitmap-index",
        ],
        Kind::Full { grace } => {
            expiration = format!("--cruft-expiration={}.seconds.ago", grace.as_secs());
            vec!["repack", "-q", "-a", "-d", "-b", "--cruft", &expiration]
        }
    };
    git_out(engine, snap, &args, Vec::new()).await?;
    let after = blocking(move || pack_names(&pack_dir)).await?;
    Ok((before, after))
}

/// The keep roots of a repack (ADR 0004 items 4 and 5): every old and new
/// target in the `ref_log` within `window`, and every unplaced commit, tag
/// and edge, that the working copy holds with a complete closure. A root
/// whose closure is broken (a value `reindex` rolled back from) would fail
/// the repack, and keeps nothing a ref could reach, so it is left out.
async fn keep_roots(
    engine: &Engine,
    snap: &SnapshotRepo,
    window: Duration,
) -> Result<Vec<Oid>, GitError> {
    let since = unix_now().saturating_sub(window.as_secs());
    let mut candidates = Vec::new();
    for row in engine.index.ref_log_since(snap.repo, 0).await? {
        if row.at >= since {
            candidates.extend(row.old);
            candidates.extend(row.new);
        }
    }
    for u in engine.index.unplaced(snap.repo).await? {
        match u.kind {
            UnplacedKind::Commit(oid) | UnplacedKind::Tag(oid) | UnplacedKind::Edge(oid) => {
                candidates.push(oid);
            }
            UnplacedKind::Pack => {}
        }
    }
    let tips: HashSet<Oid> = snap.refs.iter().map(|(_, o)| *o).collect();
    let mut seen = HashSet::new();
    candidates.retain(|o| !tips.contains(o) && seen.insert(*o));
    let present = present(engine, snap, &candidates).await?;
    if complete(snap, &present).await? {
        return Ok(present);
    }
    let mut kept = Vec::with_capacity(present.len());
    for oid in present {
        if complete(snap, &[oid]).await? {
            kept.push(oid);
        } else {
            tracing::warn!(repo = %snap.repo, %oid, "a keep root's closure is incomplete; the repack leaves it out");
        }
    }
    Ok(kept)
}

/// The objects of `oids` the snapshot holds.
pub(crate) async fn present(
    engine: &Engine,
    snap: &SnapshotRepo,
    oids: &[Oid],
) -> Result<Vec<Oid>, GitError> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let out = git_out(
        engine,
        snap,
        &["cat-file", "--batch-check=%(objectname)"],
        oid_lines("", oids).into_bytes(),
    )
    .await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| !l.ends_with(" missing"))
        .filter_map(|l| l.trim().parse().ok())
        .collect())
}

/// Whether every object reachable from `oids` is in the snapshot. Objects
/// reachable from its refs are, by I1, so the walk stops at them.
pub(crate) async fn complete(snap: &SnapshotRepo, oids: &[Oid]) -> Result<bool, GitError> {
    if oids.is_empty() {
        return Ok(true);
    }
    let mut input = oid_lines("", oids);
    input.push_str(&oid_lines("^", snap.refs.iter().map(|(_, o)| *o)));
    let ran = run(
        snap,
        &["rev-list", "--objects", "--quiet", "--stdin"],
        input.into_bytes(),
    )
    .await?;
    Ok(ran.is_ok())
}

/// Runs git in `snap` and returns its stdout.
pub(crate) async fn git_out(
    engine: &Engine,
    snap: &SnapshotRepo,
    args: &[&str],
    input: Vec<u8>,
) -> Result<Vec<u8>, GitError> {
    run(snap, args, input)
        .await?
        .map_err(|stderr| GitError::GitFailed {
            git: engine.git.clone(),
            command: args.join(" "),
            status: "a non-zero exit".to_owned(),
            stderr,
        })
}

/// Pins the mtime of every file of `packs` in `dir` to its row's
/// `created_at` (ADR 0004 item 5): the files are hard links into the node
/// cache, whose mtimes git may have freshened while serving.
fn repin(dir: &Path, packs: &[Pack]) -> Result<(), GitError> {
    for p in packs {
        let mtime = UNIX_EPOCH + Duration::from_secs(p.created_at);
        for ext in UPLOAD_ORDER {
            pin(&dir.join(format!("{}.{}", p.name, ext.as_str())), mtime)?;
        }
    }
    Ok(())
}

/// The names of the `.pack` files in `dir`, without extension.
pub(crate) fn pack_names(dir: &Path) -> Result<HashSet<String>, GitError> {
    let mut names = HashSet::new();
    for entry in std::fs::read_dir(dir).map_err(io_err("list", dir))? {
        let path = entry.map_err(io_err("list", dir))?.path();
        if let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".pack"))
            && PackName::parse(name).is_ok()
        {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

/// The next multi-pack-index generation: above every generation any row
/// of the repository records, and above `epoch << 24`, so two holders
/// with different epochs never write the same generation's files even if
/// both compute it before either records it.
async fn next_midx_gen(engine: &Engine, repo: RepoId, epoch: u64) -> Result<u64, GitError> {
    let highest = engine
        .index
        .packs(repo)
        .await?
        .iter()
        .filter_map(|r| r.pack.midx_gen)
        .max()
        .unwrap_or(0);
    Ok(highest.saturating_add(1).max(epoch.saturating_mul(1 << 24)))
}

/// Uploads the outputs' files, then the multi-pack index's, its index file
/// last.
async fn upload_outputs(engine: &Engine, prepared: &Prepared, dir: &Path) -> Result<(), GitError> {
    let repo = repo_hex(prepared.repo)?;
    for out in &prepared.outputs {
        let name = PackName::parse(&out.name)?;
        for ext in UPLOAD_ORDER {
            let path = dir.join(format!("{name}.{}", ext.as_str()));
            if !path.exists() {
                continue;
            }
            let key = Key::Pack {
                repo,
                name: name.clone(),
                ext,
            };
            upload(engine, key, &path).await?;
        }
    }
    let Some(generation) = prepared.midx_gen else {
        return Ok(());
    };
    let mut files: Vec<(MidxExt, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io_err("list", dir))? {
        let path = entry.map_err(io_err("list", dir))?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("multi-pack-index-") {
            continue;
        }
        for ext in [MidxExt::Bitmap, MidxExt::Rev] {
            if name.ends_with(&format!(".{}", ext.as_str())) {
                files.push((ext, path.clone()));
            }
        }
    }
    files.push((MidxExt::MultiPackIndex, dir.join(MIDX_FILE)));
    for (ext, path) in files {
        let key = Key::Midx {
            repo,
            generation,
            ext,
        };
        upload(engine, key, &path).await?;
    }
    Ok(())
}

/// Streams the file at `path` to `key`.
pub(crate) async fn upload(engine: &Engine, key: Key, path: &Path) -> Result<(), GitError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(io_err("open", path))?;
    let len = file.metadata().await.map_err(io_err("stat", path))?.len();
    let body = Box::pin(tokio_util::io::ReaderStream::new(file));
    engine.bucket.put_stream(key, body, len).await?;
    Ok(())
}

/// Marks a failed repack's outputs `lost`, each only if still `pending`; a
/// row this can't mark stays `pending` and GC expires it.
async fn mark_lost(engine: &Engine, outputs: &[Pack]) {
    for out in outputs {
        if let Err(e) = engine
            .index
            .mark_pack(out.id, PackState::Pending, PackState::Lost)
            .await
        {
            tracing::warn!(pack = %out.id, error = %e, "marking a failed repack's output lost failed; GC expires it");
        }
    }
}
