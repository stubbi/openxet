//! `reindex` of a repository's Git data from the bucket (ADR 0004 item 4,
//! design §5.3, ruling R24); the operator's restore and discard of what it
//! couldn't place are in `recover`. It runs with the repository's writes
//! stopped, which the `openxet reindex` command (M4) sees to, and holding
//! the repository's repack lease, which excludes repacks and pack sweeps:
//! a sweep holding a deletion claim could otherwise go on deleting the
//! files of a row reindex has just committed.
//!
//! Refs and `ref_log` rows come from the bucket ref log. A ref whose
//! closure is incomplete in the bucket's packs falls back to its newest
//! earlier value since it was last created whose closure is complete, and a
//! correction row records it: a ref deleted and created again never
//! resurrects a value of its earlier lifetime. A pack holding any object a rebuilt ref reaches is `committed`, as
//! is every cruft pack (its objects were unreachable at the last full
//! repack and keep their cruft lifecycle); every other pack is `recovered`:
//! never served, never repacked, never collected, until an operator
//! restores or discards it. What no rebuilt ref reaches is persisted in
//! `reindex_unplaced`: commit and annotated tag tips, recovered packs, and
//! the committed-pack objects recovered packs reference directly (edges),
//! which full repacks then keep as roots.

use std::collections::{BTreeMap, HashMap, HashSet};

use openxet_index::{
    Lease, Oid, Pack, PackId, PackState, RefLogRow, RefUpdate, Reindexed, RepoId, Unplaced,
    UnplacedKind,
};
use openxet_storage::{Key, KeyKind, PackExt, PackName};

use crate::lease::{renew, while_renewing};
use crate::reach::{open, to_gix};
use crate::receive::valid_ref_name;
use crate::reflog::{bucket_rows, repo_hex};
use crate::repack::{complete, git_out, present};
use crate::thin::{oid_lines, parse_idx};
use crate::{Engine, GitError, SnapshotRepo, blocking, io_err, unix_now};

/// The actor of the ref log rows `reindex` and its restore write.
pub const REINDEX_ACTOR: &str = "openxet reindex";

/// A ref `reindex` could not rebuild at its last logged value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledBack {
    /// The ref.
    pub name: String,
    /// Its last value in the bucket ref log, whose closure is incomplete.
    pub logged: Oid,
    /// The newest earlier value whose closure is complete, or `None` if
    /// none is and the ref was left out.
    pub now: Option<Oid>,
}

/// What `reindex` rebuilt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReindexReport {
    /// The rebuilt refs.
    pub refs: Vec<(String, Oid)>,
    /// The refs rebuilt at an earlier value, or left out.
    pub rolled_back: Vec<RolledBack>,
    /// How many `ref_log` rows came from the bucket.
    pub ref_log_rows: usize,
    /// The packs now `committed`, by name.
    pub committed: Vec<String>,
    /// The cruft packs among them: their unreachable objects keep their
    /// cruft lifecycle and are listed for information only.
    pub cruft: Vec<String>,
    /// The packs now `recovered`, by name.
    pub recovered: Vec<String>,
    /// Pack files in the bucket without their `.pack` or `.idx`: an upload
    /// a crash interrupted. No row describes them; they are left alone.
    pub incomplete: Vec<String>,
    /// What could not be placed, as `reindex_unplaced` now holds it.
    pub unplaced: Vec<Unplaced>,
}

/// One pack found in the bucket.
struct Found {
    pack: Pack,
    cruft: bool,
    objects: HashSet<Oid>,
}

/// Rebuilds `repo`'s refs, `ref_log` rows and pack rows from the bucket,
/// records what it could not place, and starts a fresh full-repack
/// generation (ADR 0004 item 4), holding `lease`, the repository's repack
/// lease, which it renews while it works and checks again right before
/// its index transaction.
///
/// # Errors
///
/// [`GitError::LeaseLost`] if `lease` isn't held, then or at the end;
/// [`GitError::Storage`] (a malformed ref log record included),
/// [`GitError::Index`], [`GitError::Corrupt`] for a ref log row naming an
/// object id openxet doesn't store, [`GitError::GitFailed`] and
/// [`GitError::Io`]. Nothing in the index changes on error.
pub async fn reindex_repo(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
) -> Result<ReindexReport, GitError> {
    if lease.repo != repo {
        return Err(GitError::LeaseLost);
    }
    renew(engine, lease).await?;
    while_renewing(engine, lease, reindex_under(engine, repo, lease))
        .await
        .0
}

async fn reindex_under(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
) -> Result<ReindexReport, GitError> {
    let started = unix_now();
    let ref_log = read_ref_log(engine, repo).await?;
    let (mut found, incomplete) = list_packs(engine, repo, started).await?;
    let packs: Vec<Pack> = found.iter().map(|f| f.pack.clone()).collect();
    let snap = engine
        .snapshot_of(repo, Vec::new(), packs, 0, false)
        .await?;
    for f in &mut found {
        f.objects = pack_objects(&snap, &f.pack.name).await?;
    }
    let (refs, rolled_back) = rebuild_refs(engine, &snap, &ref_log).await?;
    let corrections: Vec<RefUpdate> = rolled_back
        .iter()
        .map(|r| RefUpdate {
            name: r.name.clone(),
            old: Some(r.logged),
            new: r.now,
        })
        .collect();

    let tips: Vec<Oid> = refs.iter().map(|(_, o)| *o).collect();
    let reachable = reachable_objects(engine, &snap, &tips).await?;
    for f in &mut found {
        let placed = f.cruft || f.objects.iter().any(|o| reachable.contains(o));
        f.pack.state = if placed {
            PackState::Committed
        } else {
            PackState::Recovered
        };
    }
    let unplaced = find_unplaced(engine, &snap, &found, &reachable, started).await?;
    drop(snap);

    let packs: Vec<Pack> = found.iter().map(|f| f.pack.clone()).collect();
    let request = uuid::Uuid::new_v4().simple().to_string();
    renew(engine, lease).await?;
    engine
        .index
        .reindex(
            repo,
            Reindexed {
                refs: &refs,
                ref_log: &ref_log,
                corrections: &corrections,
                actor: REINDEX_ACTOR,
                request: &request,
                packs: &packs,
                unplaced: &unplaced,
            },
        )
        .await?;
    let names = |state: PackState| -> Vec<String> {
        found
            .iter()
            .filter(|f| f.pack.state == state)
            .map(|f| f.pack.name.clone())
            .collect()
    };
    Ok(ReindexReport {
        refs,
        rolled_back,
        ref_log_rows: ref_log.len(),
        committed: names(PackState::Committed),
        cruft: found
            .iter()
            .filter(|f| f.cruft)
            .map(|f| f.pack.name.clone())
            .collect(),
        recovered: names(PackState::Recovered),
        incomplete,
        unplaced,
    })
}

/// The bucket ref log of `repo` as `ref_log` rows.
async fn read_ref_log(engine: &Engine, repo: RepoId) -> Result<Vec<RefLogRow>, GitError> {
    bucket_rows(engine, repo)
        .await?
        .into_iter()
        .map(|e| {
            Ok(RefLogRow {
                seq: e.seq,
                repo,
                old: parse_oid(e.old.as_deref())?,
                new: parse_oid(e.new.as_deref())?,
                name: e.ref_name,
                actor: e.actor,
                request: e.request,
                at: e.at,
            })
        })
        .collect()
}

/// Each ref at its last logged value whose closure is complete in `snap`,
/// and the refs that couldn't take their last value.
async fn rebuild_refs(
    engine: &Engine,
    snap: &SnapshotRepo,
    ref_log: &[RefLogRow],
) -> Result<(Vec<(String, Oid)>, Vec<RolledBack>), GitError> {
    // Each ref's values of its current lifetime, newest first: back to
    // the row that last created it, which the walk stops after.
    let mut history: BTreeMap<&str, (Vec<Option<Oid>>, bool)> = BTreeMap::new();
    for row in ref_log.iter().rev() {
        let (values, created) = history.entry(&row.name).or_default();
        if !*created {
            values.push(row.new);
            *created = row.old.is_none();
        }
    }
    let mut refs = Vec::new();
    let mut rolled_back = Vec::new();
    for (name, (values, _)) in history {
        let Some(Some(logged)) = values.first().copied() else {
            continue;
        };
        if !valid_ref_name(name) {
            return Err(GitError::Corrupt(format!(
                "the bucket ref log of repository {} names the ref {:?}",
                snap.repo,
                name.escape_debug().to_string()
            )));
        }
        let mut rebuilt = None;
        let mut tried = HashSet::new();
        for oid in values.into_iter().flatten() {
            if tried.insert(oid) && closure_complete(engine, snap, oid).await? {
                rebuilt = Some(oid);
                break;
            }
        }
        if rebuilt != Some(logged) {
            rolled_back.push(RolledBack {
                name: name.to_owned(),
                logged,
                now: rebuilt,
            });
        }
        if let Some(oid) = rebuilt {
            refs.push((name.to_owned(), oid));
        }
    }
    Ok((refs, rolled_back))
}

fn parse_oid(hex: Option<&str>) -> Result<Option<Oid>, GitError> {
    hex.map(|h| {
        h.parse().map_err(|_| {
            GitError::Corrupt(format!(
                "the bucket ref log names the object {h:?}, which isn't a SHA-1 object id"
            ))
        })
    })
    .transpose()
}

/// Every complete pack in `repo`'s bucket prefix, with its `created_at`
/// from the `.pack`'s `Last-Modified` (packs are written once, so that is
/// when it was published) or `started` if the store doesn't say; and the
/// names of incomplete ones.
async fn list_packs(
    engine: &Engine,
    repo: RepoId,
    started: u64,
) -> Result<(Vec<Found>, Vec<String>), GitError> {
    let repo_hex = repo_hex(repo)?;
    let mut exts: BTreeMap<PackName, HashSet<PackExt>> = BTreeMap::new();
    for key in engine.bucket.list_under(repo_hex, KeyKind::Pack).await? {
        if let Key::Pack { name, ext, .. } = key {
            exts.entry(name).or_default().insert(ext);
        }
    }
    let mut found = Vec::new();
    let mut incomplete = Vec::new();
    for (name, exts) in exts {
        if !(exts.contains(&PackExt::Pack) && exts.contains(&PackExt::Idx)) {
            incomplete.push(name.to_string());
            continue;
        }
        let key = Key::Pack {
            repo: repo_hex,
            name: name.clone(),
            ext: PackExt::Pack,
        };
        let created_at = engine
            .bucket
            .last_modified(key.clone())
            .await?
            .unwrap_or(started);
        let bytes = engine.bucket.size(&key).await?.unwrap_or(0);
        found.push(Found {
            pack: Pack {
                id: PackId::generate(),
                repo,
                name: name.to_string(),
                state: PackState::Recovered,
                created_at,
                bytes,
                midx_gen: None,
            },
            cruft: exts.contains(&PackExt::Mtimes),
            objects: HashSet::new(),
        });
    }
    Ok((found, incomplete))
}

/// The object ids of pack `name` in `snap`, from its index.
pub(crate) async fn pack_objects(
    snap: &SnapshotRepo,
    name: &str,
) -> Result<HashSet<Oid>, GitError> {
    let idx = snap.dir.path().join(format!("objects/pack/{name}.idx"));
    blocking(move || {
        let bytes = std::fs::read(&idx).map_err(io_err("read", &idx))?;
        parse_idx(&bytes)
            .map(|entries| entries.into_iter().map(|(oid, _)| oid).collect())
            .ok_or_else(|| {
                GitError::Corrupt(format!("{} is not a version 2 pack index", idx.display()))
            })
    })
    .await
}

/// Whether `oid` and everything it reaches is in `snap`.
pub(crate) async fn closure_complete(
    engine: &Engine,
    snap: &SnapshotRepo,
    oid: Oid,
) -> Result<bool, GitError> {
    Ok(!present(engine, snap, &[oid]).await?.is_empty() && complete(snap, &[oid]).await?)
}

/// Every object reachable from `tips` in `snap`, which must hold them all.
pub(crate) async fn reachable_objects(
    engine: &Engine,
    snap: &SnapshotRepo,
    tips: &[Oid],
) -> Result<HashSet<Oid>, GitError> {
    if tips.is_empty() {
        return Ok(HashSet::new());
    }
    let out = git_out(
        engine,
        snap,
        &["rev-list", "--objects", "--stdin"],
        oid_lines("", tips).into_bytes(),
    )
    .await?;
    Ok(first_oids(&out))
}

/// The objects reachable from `oid` but not from `snap`'s refs, or `None`
/// if its closure is incomplete there.
pub(crate) async fn closure_beyond_refs(
    snap: &SnapshotRepo,
    oid: Oid,
) -> Result<Option<HashSet<Oid>>, GitError> {
    let mut input = oid_lines("", [oid]);
    input.push_str(&oid_lines("^", snap.refs.iter().map(|(_, o)| *o)));
    let ran = crate::thin::run(
        snap,
        &["rev-list", "--objects", "--stdin"],
        input.into_bytes(),
    )
    .await?;
    Ok(ran.ok().map(|out| first_oids(&out)))
}

pub(crate) fn first_oids(out: &[u8]) -> HashSet<Oid> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|l| l.get(..40)?.parse().ok())
        .collect()
}

pub(crate) async fn object_kind(
    engine: &Engine,
    snap: &SnapshotRepo,
    oid: Oid,
) -> Result<Option<String>, GitError> {
    let out = git_out(
        engine,
        snap,
        &["cat-file", "--batch-check=%(objecttype)"],
        oid_lines("", [oid]).into_bytes(),
    )
    .await?;
    let line = String::from_utf8_lossy(&out).trim().to_owned();
    Ok((!line.ends_with(" missing") && !line.is_empty()).then_some(line))
}

/// The types of `oids` in `snap`.
async fn kinds(
    engine: &Engine,
    snap: &SnapshotRepo,
    oids: &[Oid],
) -> Result<HashMap<Oid, String>, GitError> {
    if oids.is_empty() {
        return Ok(HashMap::new());
    }
    let out = git_out(
        engine,
        snap,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        oid_lines("", oids).into_bytes(),
    )
    .await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let (oid, kind) = l.split_once(' ')?;
            (kind != "missing").then(|| Some((oid.parse().ok()?, kind.to_owned())))?
        })
        .collect())
}

/// The unplaced entries (ruling R24): commit and annotated tag tips no
/// rebuilt ref reaches in recovered packs and non-cruft committed packs,
/// each recovered pack, and the committed-pack objects a recovered pack's
/// objects reference directly.
async fn find_unplaced(
    engine: &Engine,
    snap: &SnapshotRepo,
    found: &[Found],
    reachable: &HashSet<Oid>,
    found_at: u64,
) -> Result<Vec<Unplaced>, GitError> {
    let repo = snap.repo;
    // An object's pack for its entry: a recovered pack when one holds it,
    // as restore needs those.
    let mut home: HashMap<Oid, PackId> = HashMap::new();
    for f in found.iter().filter(|f| !f.cruft) {
        let recovered = f.pack.state == PackState::Recovered;
        for oid in f.objects.iter().filter(|o| !reachable.contains(o)) {
            if recovered || !home.contains_key(oid) {
                home.insert(*oid, f.pack.id);
            }
        }
    }
    let candidates: Vec<Oid> = home.keys().copied().collect();
    let types = kinds(engine, snap, &candidates).await?;
    let of_kind = |kind: &str| -> Vec<Oid> {
        let mut v: Vec<Oid> = types
            .iter()
            .filter(|(_, k)| k.as_str() == kind)
            .map(|(o, _)| *o)
            .collect();
        v.sort_unstable_by_key(|o| o.0);
        v
    };
    let (commits, tags) = (of_kind("commit"), of_kind("tag"));

    let recovered: Vec<&Found> = found
        .iter()
        .filter(|f| f.pack.state == PackState::Recovered)
        .collect();
    let committed: HashMap<Oid, PackId> = found
        .iter()
        .filter(|f| f.pack.state == PackState::Committed)
        .flat_map(|f| f.objects.iter().map(move |o| (*o, f.pack.id)))
        .collect();
    let rec_objects: Vec<(PackId, Vec<(Oid, String)>)> = recovered
        .iter()
        .map(|f| {
            let objs = f
                .objects
                .iter()
                .filter_map(|o| Some((*o, types.get(o)?.clone())))
                .collect();
            (f.pack.id, objs)
        })
        .collect();
    let rec_sets: HashMap<PackId, HashSet<Oid>> = recovered
        .iter()
        .map(|f| (f.pack.id, f.objects.clone()))
        .collect();
    let dir = snap.dir.path().to_path_buf();
    let (commit_tips, tag_tips, edges) = blocking(move || {
        let repo = open(&dir).map_err(GitError::Objects)?;
        let (commit_tips, tag_tips) = tips(&repo, &commits, &tags)?;
        let edges = edges(&repo, &rec_objects, &rec_sets, &committed)?;
        Ok((commit_tips, tag_tips, edges))
    })
    .await?;

    let mut out = Vec::new();
    let entry = |kind: UnplacedKind, pack: PackId| Unplaced {
        repo,
        kind,
        pack,
        found_at,
    };
    let home_of = |oid: Oid| {
        home.get(&oid).copied().ok_or_else(|| {
            GitError::Corrupt(format!(
                "unplaced object {oid} of repository {repo} is in no listed pack"
            ))
        })
    };
    for c in commit_tips {
        out.push(entry(UnplacedKind::Commit(c), home_of(c)?));
    }
    for t in tag_tips {
        out.push(entry(UnplacedKind::Tag(t), home_of(t)?));
    }
    for f in &recovered {
        out.push(entry(UnplacedKind::Pack, f.pack.id));
    }
    for (oid, pack) in edges {
        out.push(entry(UnplacedKind::Edge(oid), pack));
    }
    Ok(out)
}

/// The tips among unplaced `commits` (those no other one names as a
/// parent) and among unplaced `tags` (those no other one targets). Every
/// ancestor of an unplaced commit is unplaced or reachable, so this leaves
/// exactly the commits nothing unplaced reaches.
fn tips(
    repo: &gix::Repository,
    commits: &[Oid],
    tags: &[Oid],
) -> Result<(Vec<Oid>, Vec<Oid>), GitError> {
    let mut parents = HashSet::new();
    for c in commits {
        let commit = repo
            .find_commit(to_gix(*c))
            .map_err(|e| GitError::Objects(e.to_string()))?;
        for p in commit.parent_ids() {
            if let Ok(p) = p.detach().as_bytes().try_into().map(Oid) {
                parents.insert(p);
            }
        }
    }
    let commit_tips = commits
        .iter()
        .filter(|c| !parents.contains(c))
        .copied()
        .collect();
    let tag_set: HashSet<Oid> = tags.iter().copied().collect();
    let targets: HashSet<Oid> = tags
        .iter()
        .filter_map(|t| tag_target(repo, *t))
        .filter(|t| tag_set.contains(t))
        .collect();
    let tag_tips = tags
        .iter()
        .filter(|t| !targets.contains(t))
        .copied()
        .collect();
    Ok((commit_tips, tag_tips))
}

/// The objects in committed packs that a recovered pack's objects name
/// directly, each once with a committed pack holding it.
fn edges(
    repo: &gix::Repository,
    recovered: &[(PackId, Vec<(Oid, String)>)],
    own: &HashMap<PackId, HashSet<Oid>>,
    committed: &HashMap<Oid, PackId>,
) -> Result<Vec<(Oid, PackId)>, GitError> {
    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for (pack, objects) in recovered {
        for (oid, kind) in objects {
            for referenced in references(repo, *oid, kind)? {
                if own.get(pack).is_some_and(|o| o.contains(&referenced)) {
                    continue;
                }
                if let Some(home) = committed.get(&referenced)
                    && seen.insert(referenced)
                {
                    edges.push((referenced, *home));
                }
            }
        }
    }
    Ok(edges)
}

fn tag_target(repo: &gix::Repository, tag: Oid) -> Option<Oid> {
    let target = repo.find_tag(to_gix(tag)).ok()?.target_id().ok()?;
    target.detach().as_bytes().try_into().ok().map(Oid)
}

/// The objects `oid`, of type `kind`, names directly: a commit's tree and
/// parents, a tree's entries, a tag's target.
fn references(repo: &gix::Repository, oid: Oid, kind: &str) -> Result<Vec<Oid>, GitError> {
    let to_oid = |id: &gix::oid| -> Option<Oid> { id.as_bytes().try_into().ok().map(Oid) };
    let err = |e: &dyn std::fmt::Display| GitError::Objects(e.to_string());
    Ok(match kind {
        "commit" => {
            let commit = repo.find_commit(to_gix(oid)).map_err(|e| err(&e))?;
            let mut out: Vec<Oid> = commit
                .parent_ids()
                .filter_map(|p| to_oid(p.detach().as_ref()))
                .collect();
            out.extend(
                commit
                    .tree_id()
                    .ok()
                    .and_then(|t| to_oid(t.detach().as_ref())),
            );
            out
        }
        "tree" => {
            let tree = repo.find_tree(to_gix(oid)).map_err(|e| err(&e))?;
            tree.iter()
                .filter_map(|e| e.ok().and_then(|e| to_oid(e.oid())))
                .collect()
        }
        "tag" => tag_target(repo, oid).into_iter().collect(),
        _ => Vec::new(),
    })
}
