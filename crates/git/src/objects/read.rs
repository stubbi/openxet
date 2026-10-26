//! Hub reads at a revision (ADR 0004 item 6): `tree`, `resolve`,
//! `paths-info`, `commits` and repository info. A revision is a ref of the
//! snapshot, or a commit or tag id that passes R22 from the snapshot's refs
//! exactly as a fetch want does; anything else, an absent object included,
//! answers "revision not found" alike. Paths are then read inside the
//! commit's tree, which is reachable by construction, so the Hub serves
//! exactly what a fetch would.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use bytes::Bytes;
use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix::objs::Kind;
use gix::objs::tree::EntryMode;
use gix::traverse::commit::simple::CommitTimeOrder;
use openxet_index::Oid;

use super::pointer::{MAX_POINTER_BYTES, parse_pointer};
use super::{
    Blob, CommitInfo, Entry, EntryKind, LocatedBlob, Resolved, Revision, Signature, TreeListing,
    TreePage,
};
use crate::reach::{from_gix, open, peel, to_gix};
use crate::snapshot::HEAD_REF;
use crate::{Engine, GitError, SnapshotRepo, blocking};

impl Engine {
    /// Resolves `rev` in `snap` to a commit: a ref name (a branch, then a
    /// tag, or a full name), or a commit or tag id that passes R22 from the
    /// snapshot's refs (a ref tip or peeled tag target, otherwise the walk
    /// within `git.reachability`). Tags are peeled. `who` is the reading
    /// principal's subject: a read by id takes R22's per-principal slot
    /// under it, as a fetch does.
    ///
    /// # Errors
    ///
    /// [`GitError::RevisionNotFound`] for a missing ref, an id R22 refuses
    /// or the snapshot lacks, a tree or blob id, or a ref that peels to no
    /// commit, all alike; [`GitError::Busy`] over R22's concurrency limits
    /// for this repository or `who`.
    pub async fn resolve_revision(
        &self,
        snap: &SnapshotRepo,
        rev: Revision<'_>,
        who: &str,
    ) -> Result<Resolved, GitError> {
        let not_found = || GitError::RevisionNotFound {
            revision: rev.to_string(),
        };
        let target = match rev {
            Revision::Ref(name) => find_ref(&snap.refs, name).ok_or_else(not_found)?,
            Revision::Oid(oid) => {
                if let Err(refused) = self.reach.check(snap, &[oid], who).await {
                    return Err(if refused.retryable() {
                        GitError::Busy
                    } else {
                        not_found()
                    });
                }
                oid
            }
        };
        let dir = snap.dir.path().to_path_buf();
        blocking(move || peel_to_commit(&open_snapshot(&dir)?, target))
            .await?
            .ok_or_else(not_found)
    }

    /// Lists the tree at `path` in `commit` (from [`Engine::resolve_revision`]
    /// on the same snapshot): its entries, or with `recursive` everything
    /// under it, folders included, ordered by path. `""` is the root;
    /// leading, trailing and repeated `/` are ignored. Submodule entries are
    /// left out: the Hub has no type for them.
    ///
    /// # Errors
    ///
    /// [`GitError::EntryNotFound`] if `path` names no folder, or has a `.`
    /// or `..` component; [`GitError::Objects`] if the snapshot can't be
    /// read.
    pub async fn read_tree(
        &self,
        snap: &SnapshotRepo,
        commit: Oid,
        path: &str,
        recursive: bool,
    ) -> Result<Vec<Entry>, GitError> {
        let page = TreePage {
            after: None,
            limit: usize::MAX,
        };
        Ok(self
            .read_tree_page(snap, commit, path, recursive, page)
            .await?
            .entries)
    }

    /// One page of [`Engine::read_tree`]'s listing: the entries whose path
    /// sorts after `page.after`, at most `page.limit`. Only the page's
    /// blobs are read for their size and LFS pointer, so a page of a large
    /// folder costs the page, not the folder.
    ///
    /// # Errors
    ///
    /// As [`Engine::read_tree`].
    pub async fn read_tree_page(
        &self,
        snap: &SnapshotRepo,
        commit: Oid,
        path: &str,
        recursive: bool,
        page: TreePage<'_>,
    ) -> Result<TreeListing, GitError> {
        let comps = read_components(path).ok_or_else(|| entry_not_found(path))?;
        let dir = snap.dir.path().to_path_buf();
        let path = path.to_owned();
        let after = page.after.map(str::to_owned);
        let limit = page.limit;
        blocking(move || {
            let repo = open_snapshot(&dir)?;
            let root = commit_tree(&repo, commit)?;
            match find_path(&repo, root, &comps)? {
                Some((mode, tree)) if mode.is_tree() => {
                    let prefix = comps.join("/");
                    if !recursive
                        && let Some(listing) =
                            folder_page(&repo, tree, &prefix, after.as_deref(), limit)?
                    {
                        return Ok(listing);
                    }
                    let listed = list(&repo, tree, &prefix, recursive)?;
                    let start = after.map_or(0, |after| {
                        listed.partition_point(|e| e.path.as_bytes() <= after.as_bytes())
                    });
                    let end = listed.len().min(start.saturating_add(limit));
                    let entries = describe_all(&repo, &listed[start..end])?;
                    Ok(TreeListing {
                        entries,
                        more: end < listed.len(),
                    })
                }
                _ => Err(entry_not_found(&path)),
            }
        })
        .await
    }

    /// Finds the file at `path` in `commit` (from
    /// [`Engine::resolve_revision`] on the same snapshot) and reads its size
    /// from the object header, without inflating it, so a caller can reserve
    /// memory before [`Engine::read_located`] buffers it. One tree walk
    /// serves both.
    ///
    /// # Errors
    ///
    /// As [`Engine::read_blob`].
    pub async fn locate_blob(
        &self,
        snap: &SnapshotRepo,
        commit: Oid,
        path: &str,
    ) -> Result<LocatedBlob, GitError> {
        let comps = read_components(path)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| entry_not_found(path))?;
        let dir = snap.dir.path().to_path_buf();
        let path = path.to_owned();
        blocking(move || {
            let repo = open_snapshot(&dir)?;
            let id = find_file(&repo, commit, &comps, &path)?;
            let size = repo.find_header(id).map_err(objects_err)?.size();
            Ok(LocatedBlob {
                oid: oid_of(id)?,
                size,
            })
        })
        .await
    }

    /// Reads the file [`Engine::locate_blob`] found in `snap`: its bytes, or
    /// the LFS pointer it holds. The whole blob is buffered, so a caller
    /// reserves [`LocatedBlob::size`] first (streaming large blobs is
    /// <https://github.com/stubbi/openxet/issues/91>).
    ///
    /// # Errors
    ///
    /// [`GitError::Objects`] if the snapshot can't be read.
    pub async fn read_located(
        &self,
        snap: &SnapshotRepo,
        blob: LocatedBlob,
    ) -> Result<Blob, GitError> {
        let dir = snap.dir.path().to_path_buf();
        blocking(move || read_object(&open_snapshot(&dir)?, to_gix(blob.oid))).await
    }

    /// Reads the file at `path` in `commit` (from
    /// [`Engine::resolve_revision`] on the same snapshot): its bytes, or the
    /// LFS pointer it holds. The whole blob is buffered; a caller that must
    /// bound it uses [`Engine::locate_blob`] and [`Engine::read_located`].
    ///
    /// # Errors
    ///
    /// [`GitError::EntryNotFound`] if `path` names no file (a folder or the
    /// root included); [`GitError::Objects`] if the snapshot can't be read.
    pub async fn read_blob(
        &self,
        snap: &SnapshotRepo,
        commit: Oid,
        path: &str,
    ) -> Result<Blob, GitError> {
        let comps = read_components(path)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| entry_not_found(path))?;
        let dir = snap.dir.path().to_path_buf();
        let path = path.to_owned();
        blocking(move || {
            let repo = open_snapshot(&dir)?;
            read_object(&repo, find_file(&repo, commit, &comps, &path)?)
        })
        .await
    }

    /// Up to `limit` commits of `from`'s history (from
    /// [`Engine::resolve_revision`] on the same snapshot), newest first by
    /// commit time as `git log` lists them, starting after `cursor`, the
    /// last commit of the previous page. The walk stays within
    /// `git.reachability`'s commit and time budget, skipped commits
    /// included: a page ends where the budget does, and a cursor that isn't
    /// in the history, or not within the budget, gives an empty page. A
    /// page after a cursor takes R22's concurrency slots for this
    /// repository and `who` (the reading principal's subject) while it
    /// walks, as R22's own walk does.
    ///
    /// # Errors
    ///
    /// [`GitError::Busy`] over R22's concurrency limits for a page after a
    /// cursor; [`GitError::Objects`] if the snapshot can't be read.
    pub async fn log(
        &self,
        snap: &SnapshotRepo,
        from: Oid,
        cursor: Option<Oid>,
        limit: usize,
        who: &str,
    ) -> Result<Vec<CommitInfo>, GitError> {
        // A first page walks at most `limit` commits; one after a cursor
        // may walk the whole budget to find it.
        let _slots = match cursor {
            Some(_) => Some(
                self.reach
                    .walk_slots(snap.repo, who)
                    .map_err(|_| GitError::Busy)?,
            ),
            None => None,
        };
        let dir = snap.dir.path().to_path_buf();
        let budget = self.config.reachability;
        blocking(move || {
            let started = Instant::now();
            let repo = open_snapshot(&dir)?;
            let mut skipping = cursor.is_some();
            let mut out = Vec::new();
            for (visited, id) in (0u64..).zip(walk(&repo, from)?) {
                if out.len() >= limit
                    || visited >= budget.max_objects
                    || started.elapsed() >= budget.max_time
                {
                    break;
                }
                let id = id?;
                if skipping {
                    skipping = Some(id) != cursor;
                    continue;
                }
                out.push(commit_info(&repo, id)?);
            }
            Ok(out)
        })
        .await
    }

    /// How many commits `from`'s history holds (from
    /// [`Engine::resolve_revision`] on the same snapshot): the Hub's
    /// `X-Total-Count` of `commits`. `None` when the walk doesn't finish
    /// within `git.reachability`'s commit and time budget, so a huge history
    /// costs no more than one budget.
    ///
    /// # Errors
    ///
    /// [`GitError::Objects`] if the snapshot can't be read.
    pub async fn count_commits(
        &self,
        snap: &SnapshotRepo,
        from: Oid,
    ) -> Result<Option<u64>, GitError> {
        let dir = snap.dir.path().to_path_buf();
        let budget = self.config.reachability;
        blocking(move || {
            let started = Instant::now();
            let repo = open_snapshot(&dir)?;
            let mut count = 0u64;
            for id in walk(&repo, from)? {
                if count >= budget.max_objects || started.elapsed() >= budget.max_time {
                    return Ok(None);
                }
                id?;
                count += 1;
            }
            Ok(Some(count))
        })
        .await
    }

    /// For each of `paths`, the newest commit of `commit`'s history (by
    /// commit time) whose entry at the path differs from the entry in every
    /// one of its parents, and exists: the Hub's `lastCommit` with
    /// `expand=true`. `None` for a path that doesn't exist in `commit`, and
    /// for one not settled within `git.reachability`'s commit and time
    /// budget.
    ///
    /// # Errors
    ///
    /// [`GitError::Objects`] if the snapshot can't be read.
    pub async fn last_commit_for_paths(
        &self,
        snap: &SnapshotRepo,
        commit: Oid,
        paths: &[&str],
    ) -> Result<Vec<Option<CommitInfo>>, GitError> {
        let comps: Vec<Option<Vec<String>>> = paths.iter().map(|p| read_components(p)).collect();
        let dir = snap.dir.path().to_path_buf();
        let budget = self.config.reachability;
        blocking(move || {
            let started = Instant::now();
            let repo = open_snapshot(&dir)?;
            let mut out: Vec<Option<CommitInfo>> = vec![None; comps.len()];
            let mut trees = Trees::default();
            let tip_tree = trees.of(&repo, commit)?;
            let mut pending = Vec::new();
            for (i, c) in comps.iter().enumerate() {
                if let Some(c) = c
                    && find_path(&repo, tip_tree, c)?.is_some()
                {
                    pending.push(i);
                }
            }
            for (visited, id) in (0u64..).zip(walk(&repo, commit)?) {
                if pending.is_empty()
                    || visited >= budget.max_objects
                    || started.elapsed() >= budget.max_time
                {
                    break;
                }
                let id = id?;
                let tree = trees.of(&repo, id)?;
                let parents = parents(&repo, id)?
                    .into_iter()
                    .map(|p| trees.of(&repo, p))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut still = Vec::with_capacity(pending.len());
                for i in pending {
                    let Some(c) = &comps[i] else { continue };
                    let here = find_path(&repo, tree, c)?.map(|(_, id)| id);
                    let mut changed = here.is_some();
                    for parent in &parents {
                        if !changed {
                            break;
                        }
                        changed = find_path(&repo, *parent, c)?.map(|(_, id)| id) != here;
                    }
                    if changed {
                        out[i] = Some(commit_info(&repo, id)?);
                    } else {
                        still.push(i);
                    }
                }
                pending = still;
            }
            Ok(out)
        })
        .await
    }
}

/// The ref `name` names in `refs`: `HEAD`, a full ref name, or a branch
/// name, then a tag name, as huggingface.co resolves a revision.
fn find_ref(refs: &[(String, Oid)], name: &str) -> Option<Oid> {
    let candidates = if name == "HEAD" {
        vec![HEAD_REF.to_owned()]
    } else if name.starts_with("refs/") {
        vec![name.to_owned()]
    } else {
        vec![format!("refs/heads/{name}"), format!("refs/tags/{name}")]
    };
    candidates
        .iter()
        .find_map(|c| refs.iter().find(|(n, _)| n == c).map(|(_, oid)| *oid))
}

/// Opens a snapshot for gix, with an object cache for walks that read the
/// same trees again.
pub(super) fn open_snapshot(dir: &Path) -> Result<gix::Repository, GitError> {
    let mut repo = open(dir).map_err(GitError::Objects)?;
    repo.object_cache_size_if_unset(OBJECT_CACHE_BYTES);
    Ok(repo)
}

const OBJECT_CACHE_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn objects_err(e: impl std::fmt::Display) -> GitError {
    GitError::Objects(e.to_string())
}

pub(super) fn oid_of(id: impl Into<ObjectId>) -> Result<Oid, GitError> {
    from_gix(id.into()).ok_or_else(|| GitError::Corrupt("an object id is not SHA-1".to_owned()))
}

/// The blob `id`, as a [`Blob`]: an LFS pointer, or the bytes.
fn read_object(repo: &gix::Repository, id: ObjectId) -> Result<Blob, GitError> {
    let data = repo.find_blob(id).map_err(objects_err)?.take_data();
    let oid = oid_of(id)?;
    if data.len() <= MAX_POINTER_BYTES
        && let Some(pointer) = parse_pointer(&data)
    {
        return Ok(Blob::Lfs { oid, pointer });
    }
    Ok(Blob::Inline {
        oid,
        bytes: Bytes::from(data),
    })
}

/// The blob of the file at `comps` (`path` as the caller named it) in
/// `commit`'s tree: not a folder, the root or a submodule.
fn find_file(
    repo: &gix::Repository,
    commit: Oid,
    comps: &[String],
    path: &str,
) -> Result<ObjectId, GitError> {
    let root = commit_tree(repo, commit)?;
    match find_path(repo, root, comps)? {
        Some((mode, id)) if !mode.is_tree() && !mode.is_commit() => Ok(id),
        _ => Err(entry_not_found(path)),
    }
}

fn entry_not_found(path: &str) -> GitError {
    GitError::EntryNotFound {
        path: path.to_owned(),
    }
}

/// `path`'s components for a read: empty components (leading, trailing or
/// repeated `/`) are dropped; `None` if one is `.` or `..`, which never
/// name a tree entry and must never step outside the commit's tree.
fn read_components(path: &str) -> Option<Vec<String>> {
    let comps: Vec<String> = path
        .split('/')
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect();
    comps.iter().all(|c| c != "." && c != "..").then_some(comps)
}

/// `oid` peeled through any tags to a commit, with the commit's tree;
/// `None` if it ends at anything else or at an object the snapshot lacks.
fn peel_to_commit(repo: &gix::Repository, oid: Oid) -> Result<Option<Resolved>, GitError> {
    let (chain, kind) = peel(repo, oid);
    let Some(commit) = chain.last().copied().filter(|_| kind == Some(Kind::Commit)) else {
        return Ok(None);
    };
    let tree = oid_of(commit_tree(repo, commit)?)?;
    Ok(Some(Resolved { commit, tree }))
}

pub(super) fn commit_tree(repo: &gix::Repository, commit: Oid) -> Result<ObjectId, GitError> {
    Ok(repo
        .find_commit(to_gix(commit))
        .map_err(objects_err)?
        .tree_id()
        .map_err(objects_err)?
        .detach())
}

/// The entry at `comps` under tree `root`: its mode and id; the root itself
/// for no components. `None` if a component is missing or a non-final one
/// isn't a tree.
fn find_path(
    repo: &gix::Repository,
    root: ObjectId,
    comps: &[String],
) -> Result<Option<(EntryMode, ObjectId)>, GitError> {
    let mut current = (EntryMode::from(gix::objs::tree::EntryKind::Tree), root);
    for name in comps {
        if !current.0.is_tree() {
            return Ok(None);
        }
        let tree = repo.find_tree(current.1).map_err(objects_err)?;
        let decoded = tree.decode().map_err(objects_err)?;
        let Some(entry) = decoded
            .entries
            .iter()
            .find(|e| e.filename.as_bytes() == name.as_bytes())
        else {
            return Ok(None);
        };
        current = (entry.mode, entry.oid.to_owned());
    }
    Ok(Some(current))
}

/// One page of the folder `tree` at `prefix`, sorting its entries in place
/// and making paths only for the page: a page of a folder of thousands of
/// files then costs the page. `None` if a name isn't UTF-8, whose lossy
/// path may sort differently from its bytes; [`list`] handles that folder.
fn folder_page(
    repo: &gix::Repository,
    tree: ObjectId,
    prefix: &str,
    after: Option<&str>,
    limit: usize,
) -> Result<Option<TreeListing>, GitError> {
    let tree = repo.find_tree(tree).map_err(objects_err)?;
    let decoded = tree.decode().map_err(objects_err)?;
    let mut items: Vec<_> = decoded
        .entries
        .iter()
        .filter(|e| !e.mode.is_commit())
        .collect();
    if items
        .iter()
        .any(|e| std::str::from_utf8(e.filename).is_err())
    {
        return Ok(None);
    }
    // Every path is `{prefix}/{name}` (or `{name}` at the root), so paths
    // sort as names do.
    items.sort_unstable_by(|a, b| a.filename.cmp(b.filename));
    let sep: &[u8] = if prefix.is_empty() { b"" } else { b"/" };
    let start = after.map_or(0, |after| {
        items.partition_point(|e| {
            let path = prefix.as_bytes().iter().chain(sep).chain(e.filename.iter());
            path.cmp(after.as_bytes().iter()) != std::cmp::Ordering::Greater
        })
    });
    let end = items.len().min(start.saturating_add(limit));
    let page: Vec<Listed> = items[start..end]
        .iter()
        .map(|e| {
            let name = String::from_utf8_lossy(e.filename);
            Listed {
                path: if prefix.is_empty() {
                    name.into_owned()
                } else {
                    format!("{prefix}/{name}")
                },
                id: e.oid.to_owned(),
                tree: e.mode.is_tree(),
            }
        })
        .collect();
    let entries = describe_all(repo, &page)?;
    Ok(Some(TreeListing {
        entries,
        more: end < items.len(),
    }))
}

/// The most threads one page's blobs are read on.
const DESCRIBE_THREADS: usize = 4;
/// The fewest entries worth a thread of their own.
const DESCRIBE_PER_THREAD: usize = 128;

/// `listed`, each file's size and LFS pointer read from its blob. A large
/// page is read on up to [`DESCRIBE_THREADS`] threads: its blobs are
/// independent, and inflating 1,000 pointer blobs one after another was
/// the largest part of a `tree` page (M2 Task 20, design §5.8).
fn describe_all(repo: &gix::Repository, listed: &[Listed]) -> Result<Vec<Entry>, GitError> {
    let threads = (listed.len() / DESCRIBE_PER_THREAD).clamp(1, DESCRIBE_THREADS);
    let chunk = listed.len().div_ceil(threads).max(1);
    let mut parts = listed.chunks(chunk);
    let Some(first) = parts.next() else {
        return Ok(Vec::new());
    };
    std::thread::scope(|scope| {
        let helpers: Vec<_> = parts
            .map(|part| {
                // A clone shares the open object store (gix's `parallel`).
                let repo = repo.clone();
                scope.spawn(move || {
                    part.iter()
                        .map(|l| l.describe(&repo))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect();
        let mut out = Vec::with_capacity(listed.len());
        for l in first {
            out.push(l.describe(repo)?);
        }
        for helper in helpers {
            match helper.join() {
                Ok(entries) => out.extend(entries?),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        Ok(out)
    })
}

/// A tree entry as its tree lists it, before its blob is read.
struct Listed {
    path: String,
    id: ObjectId,
    tree: bool,
}

impl Listed {
    /// The entry, a file's size and LFS pointer read from its blob.
    fn describe(&self, repo: &gix::Repository) -> Result<Entry, GitError> {
        let oid = oid_of(self.id)?;
        let kind = if self.tree {
            EntryKind::Dir { oid }
        } else {
            let (size, lfs) = blob_info(repo, self.id)?;
            EntryKind::File { oid, size, lfs }
        };
        Ok(Entry {
            path: self.path.clone(),
            kind,
        })
    }
}

/// The entries under tree `root`, at `prefix`, ordered by path, without
/// reading their blobs.
fn list(
    repo: &gix::Repository,
    root: ObjectId,
    prefix: &str,
    recursive: bool,
) -> Result<Vec<Listed>, GitError> {
    let mut out = Vec::new();
    let mut stack = vec![(root, prefix.to_owned())];
    while let Some((id, prefix)) = stack.pop() {
        let tree = repo.find_tree(id).map_err(objects_err)?;
        let decoded = tree.decode().map_err(objects_err)?;
        for e in &decoded.entries {
            let name = e.filename.to_str_lossy();
            let path = if prefix.is_empty() {
                name.into_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let id = e.oid.to_owned();
            let tree = e.mode.is_tree();
            if tree && recursive {
                stack.push((id, path.clone()));
            }
            if !e.mode.is_commit() {
                out.push(Listed { path, id, tree });
            }
        }
    }
    out.sort_unstable_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(out)
}

/// A blob's size, and its pointer when it is small enough to be one.
fn blob_info(
    repo: &gix::Repository,
    id: ObjectId,
) -> Result<(u64, Option<super::Pointer>), GitError> {
    let size = repo.find_header(id).map_err(objects_err)?.size();
    if usize::try_from(size).is_ok_and(|s| s <= MAX_POINTER_BYTES) {
        let blob = repo.find_blob(id).map_err(objects_err)?;
        return Ok((size, parse_pointer(&blob.data)));
    }
    Ok((size, None))
}

/// `from`'s history, newest first by commit time.
fn walk(
    repo: &gix::Repository,
    from: Oid,
) -> Result<impl Iterator<Item = Result<Oid, GitError>> + '_, GitError> {
    let walk = repo
        .rev_walk([to_gix(from)])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            CommitTimeOrder::NewestFirst,
        ))
        .use_commit_graph(true)
        .all()
        .map_err(objects_err)?;
    Ok(walk.map(|info| oid_of(info.map_err(objects_err)?.id)))
}

fn parents(repo: &gix::Repository, commit: Oid) -> Result<Vec<Oid>, GitError> {
    repo.find_commit(to_gix(commit))
        .map_err(objects_err)?
        .parent_ids()
        .map(|p| oid_of(p.detach()))
        .collect()
}

/// Commits' root trees, each read once.
#[derive(Default)]
struct Trees(HashMap<Oid, ObjectId>);

impl Trees {
    fn of(&mut self, repo: &gix::Repository, commit: Oid) -> Result<ObjectId, GitError> {
        if let Some(tree) = self.0.get(&commit) {
            return Ok(*tree);
        }
        let tree = commit_tree(repo, commit)?;
        self.0.insert(commit, tree);
        Ok(tree)
    }
}

fn commit_info(repo: &gix::Repository, oid: Oid) -> Result<CommitInfo, GitError> {
    let commit = repo.find_commit(to_gix(oid)).map_err(objects_err)?;
    let author = commit.author().map_err(objects_err)?;
    let committer = commit.committer().map_err(objects_err)?;
    let signature = |s: gix::actor::SignatureRef<'_>| Signature {
        name: s.name.to_str_lossy().into_owned(),
        email: s.email.to_str_lossy().into_owned(),
        time: s.seconds(),
    };
    Ok(CommitInfo {
        oid,
        tree: oid_of(commit.tree_id().map_err(objects_err)?.detach())?,
        parents: commit
            .parent_ids()
            .map(|p| oid_of(p.detach()))
            .collect::<Result<_, _>>()?,
        author: signature(author),
        committer: signature(committer),
        message: commit
            .message_raw()
            .map_err(objects_err)?
            .to_str_lossy()
            .into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use openxet_index::Oid;

    use super::{find_ref, read_components};

    #[test]
    fn refs_resolve_as_the_hub_does() {
        let refs = vec![
            ("refs/heads/main".to_owned(), Oid([1; 20])),
            ("refs/heads/v1".to_owned(), Oid([2; 20])),
            ("refs/pr/1".to_owned(), Oid([4; 20])),
            ("refs/tags/v1".to_owned(), Oid([3; 20])),
        ];
        assert_eq!(find_ref(&refs, "HEAD"), Some(Oid([1; 20])));
        assert_eq!(find_ref(&refs, "v1"), Some(Oid([2; 20])), "branch first");
        assert_eq!(find_ref(&refs, "refs/tags/v1"), Some(Oid([3; 20])));
        assert_eq!(find_ref(&refs, "refs/pr/1"), Some(Oid([4; 20])));
        assert_eq!(find_ref(&refs, "heads/main"), None);
        assert_eq!(find_ref(&refs, "pr/1"), None);
    }

    #[test]
    fn read_paths_stay_inside_the_tree() {
        assert_eq!(read_components(""), Some(Vec::new()));
        assert_eq!(
            read_components("/a//b/"),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(read_components("a/../b"), None);
        assert_eq!(read_components("./a"), None);
    }
}
