//! The writer reachability rule (ADR 0004 item 3, R22 for writers): a push
//! may make reachable only objects it sent itself, or objects already
//! reachable from the snapshot's refs.
//!
//! receive-pack has checked connectivity against the snapshot's committed
//! packs, which also hold objects no ref reaches any more. So every object
//! `git rev-list --objects <new tips> --not <snapshot tips>` lists must be
//! one the client sent, and every delta base `index-pack --fix-thin`
//! appended to the client's pack must be reachable: a sent delta can copy
//! bytes out of its base. What isn't sent is proven reachable by the fast
//! paths (one tree lookup at the path the push uses it, in the trees of the
//! boundary commits and ref tips), then by one full walk under
//! `git.thin_bases`, failing closed.
//!
//! An object `rev-list` lists that the client didn't send goes through the
//! same proof rather than failing outright: without bitmaps, `rev-list`
//! lists an old object that a new commit reintroduces, which a client with
//! bitmaps knows the server has and rightly doesn't send.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;

use openxet_index::Oid;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::time::Instant;

use crate::{Budget, GitError, SnapshotRepo, git_command, io_err};

/// git's reason for a ref whose objects the connectivity check can't find
/// (`builtin/receive-pack.c`, `set_connectivity_errors`, git v2.39.5). Every
/// refusal under the rule uses it, so a pusher can't tell an unreachable
/// object from an absent one by the text.
pub const MISSING_OBJECTS: &str = "missing necessary objects";

/// The hint for a refused thin pack. Over smart HTTP, git ignores
/// `git push --no-thin` (git v2.39.5 `transport-helper.c` lists
/// `TRANS_OPT_THIN` among the helper's unsupported options, and
/// `remote-curl` always passes `--thin` to `send-pack`), so the hint names
/// what does stop thin bases there: no delta search (`pack.window=0`).
pub const THIN_HINT: &str = "hint: the server could not use the delta bases this thin pack \
                             relies on; fetch, then push again with `git -c pack.window=0 push`, \
                             which sends complete objects (over HTTP git ignores --no-thin)";

/// The most tree lookups the fast paths make for one push; past it, the
/// remaining objects go to the walk.
const MAX_FAST_PATH_LOOKUPS: usize = 100_000;

/// A pack trailer: the SHA-1 of everything before it.
const PACK_TRAILER: u64 = 20;
/// A pack header: `PACK`, the version and the object count.
const PACK_HEADER: u64 = 12;

/// Which objects of a pushed pack the client sent and which
/// `index-pack --fix-thin` appended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentObjects {
    sent: HashSet<Oid>,
    appended: Vec<Oid>,
}

impl SentObjects {
    /// Whether the client sent `oid`.
    #[must_use]
    pub fn contains(&self, oid: &Oid) -> bool {
        self.sent.contains(oid)
    }

    /// How many objects the client sent.
    #[must_use]
    pub fn sent_len(&self) -> usize {
        self.sent.len()
    }

    /// The bases `index-pack --fix-thin` appended.
    #[must_use]
    pub fn appended(&self) -> &[Oid] {
        &self.appended
    }
}

/// Splits the objects of the pack whose index is `idx` into those the
/// client sent and those appended after them (ADR 0004 item 3): the client's
/// pack was `client_pack_bytes` long and its header counted `header_count`
/// objects, so its entries start before `client_pack_bytes - 20`, where its
/// trailer was, and the appended bases at or after it. If the split doesn't
/// find exactly `header_count` client entries, every object counts as
/// appended, so the rule fails closed.
///
/// # Errors
///
/// [`GitError::Io`] if the index can't be read, and [`GitError::Corrupt`]
/// if it isn't a version 2 pack index.
pub fn classify_sent(
    idx: &Path,
    client_pack_bytes: u64,
    header_count: u32,
) -> Result<SentObjects, GitError> {
    let bytes = std::fs::read(idx).map_err(io_err("read", idx))?;
    let entries = parse_idx(&bytes).ok_or_else(|| {
        GitError::Corrupt(format!("{} is not a version 2 pack index", idx.display()))
    })?;
    let mut out = SentObjects::default();
    let boundary = client_pack_bytes.checked_sub(PACK_TRAILER);
    let client = boundary.filter(|b| *b >= PACK_HEADER);
    for (oid, offset) in &entries {
        match client {
            Some(end) if *offset < end => {
                out.sent.insert(*oid);
            }
            _ => out.appended.push(*oid),
        }
    }
    if out.sent.len() != usize::try_from(header_count).unwrap_or(usize::MAX) {
        out.appended = entries.into_iter().map(|(oid, _)| oid).collect();
        out.sent.clear();
    }
    Ok(out)
}

/// The object ids and offsets of a version 2 pack index
/// (gitformat-pack(5), "Version 2 pack-*.idx files"): magic `\377tOc`,
/// version 2, a 256-entry fan-out table, the sorted ids, their CRCs, their
/// 4-byte offsets (the high bit points into the 8-byte offset table), the
/// 8-byte offsets, and two trailing checksums.
fn parse_idx(bytes: &[u8]) -> Option<Vec<(Oid, u64)>> {
    const FANOUT: usize = 8;
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if bytes.get(..4)? != b"\xfftOc" || be32(4)? != 2 {
        return None;
    }
    let count = usize::try_from(be32(FANOUT + 255 * 4)?).ok()?;
    let ids = FANOUT + 256 * 4;
    let offsets = ids.checked_add(count.checked_mul(24)?)?;
    let large = offsets.checked_add(count.checked_mul(4)?)?;
    if large.checked_add(40)? > bytes.len() {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let id: [u8; 20] = bytes
            .get(ids + i * 20..ids + i * 20 + 20)?
            .try_into()
            .ok()?;
        let small = be32(offsets + i * 4)?;
        let offset = if small & 0x8000_0000 == 0 {
            u64::from(small)
        } else {
            let at = large.checked_add(usize::try_from(small & 0x7fff_ffff).ok()? * 8)?;
            u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?)
        };
        out.push((Oid(id), offset));
    }
    Some(out)
}

/// How the rule accepted a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Proof {
    /// The client sent every object the push makes reachable, and nothing
    /// was appended.
    Sent,
    /// What the client didn't send was found by the fast paths.
    FastPath,
    /// The full walk under `git.thin_bases` was needed.
    Walk,
}

/// Why the rule refused a push.
#[derive(Debug, thiserror::Error)]
pub enum WriterRefused {
    /// An object the push makes reachable, or a base it relies on, is
    /// neither sent nor reachable from the snapshot's refs.
    #[error("{MISSING_OBJECTS}")]
    Unreachable {
        /// Whether an appended base was among them.
        thin_base: bool,
    },
    /// The walk ran out of `git.thin_bases` before placing every object.
    #[error("{MISSING_OBJECTS}")]
    OverBudget {
        /// Whether an appended base was among them.
        thin_base: bool,
    },
    /// git failed while checking.
    #[error("the push could not be checked: {0}")]
    Failed(#[from] GitError),
}

impl WriterRefused {
    /// The `ng` reason the pusher sees.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Unreachable { .. } | Self::OverBudget { .. } => MISSING_OBJECTS,
            Self::Failed(_) => {
                "the server failed to check this push; push again, and report it \
                                to the server's operator if it persists"
            }
        }
    }

    /// The hint shown with the refusal, [`THIN_HINT`], when a thin base
    /// couldn't be placed.
    #[must_use]
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Unreachable { thin_base: true } | Self::OverBudget { thin_base: true } => {
                Some(THIN_HINT)
            }
            _ => None,
        }
    }
}

/// A new blob the push adds to history, for the large-file report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewBlob {
    pub(crate) path: String,
    pub(crate) size: u64,
}

/// The rule's verdict and the large new blobs.
#[derive(Debug)]
pub(crate) struct Analysis {
    pub(crate) proof: Proof,
    pub(crate) large_blobs: Vec<NewBlob>,
}

/// Applies the writer reachability rule to a push whose new ref targets are
/// `new_tips` and whose pack's objects are classified by `sent`, in
/// `snapshot`, where receive-pack has put the pack. Returns which path
/// proved it.
///
/// # Errors
///
/// [`WriterRefused`], carrying git's missing-object text and, for a thin base,
/// [`THIN_HINT`].
pub async fn check_writer_rule(
    snapshot: &SnapshotRepo,
    new_tips: &[Oid],
    sent: &SentObjects,
    budget: &Budget,
) -> Result<Proof, WriterRefused> {
    analyse(snapshot, new_tips, sent, budget, u64::MAX)
        .await
        .map(|a| a.proof)
}

/// [`check_writer_rule`], also returning the new blobs of at least
/// `large_blob_bytes`.
pub(crate) async fn analyse(
    snapshot: &SnapshotRepo,
    new_tips: &[Oid],
    sent: &SentObjects,
    budget: &Budget,
    large_blob_bytes: u64,
) -> Result<Analysis, WriterRefused> {
    let tips = snapshot_tips(snapshot);
    let (listed, boundary) = list_new(snapshot, new_tips, &tips).await?;
    let info = object_info(snapshot, listed.iter().map(|l| l.oid)).await?;

    let large_blobs = listed
        .iter()
        .filter_map(|l| {
            let i = info.get(&l.oid)?;
            (i.kind == "blob" && i.size >= large_blob_bytes).then(|| NewBlob {
                path: String::from_utf8_lossy(l.path.as_deref().unwrap_or_default()).into_owned(),
                size: i.size,
            })
        })
        .collect();

    let appended: HashSet<Oid> = sent.appended().iter().copied().collect();
    let mut need: Vec<Oid> = Vec::new();
    let mut seen = HashSet::new();
    for oid in listed
        .iter()
        .map(|l| l.oid)
        .filter(|o| !sent.contains(o))
        .chain(sent.appended().iter().copied())
    {
        if seen.insert(oid) {
            need.push(oid);
        }
    }
    if need.is_empty() {
        return Ok(Analysis {
            proof: Proof::Sent,
            large_blobs,
        });
    }

    // Where the push uses each object: its own path when it is listed, and
    // the path of each sent object stored as a delta against it, which is
    // where stock git takes its thin bases from (`builtin/pack-objects.c`,
    // `show_edge` → `add_preferred_base`).
    let mut paths: HashMap<Oid, Vec<Vec<u8>>> = HashMap::new();
    for l in &listed {
        let Some(path) = &l.path else { continue };
        if seen.contains(&l.oid) {
            paths.entry(l.oid).or_default().push(path.clone());
        }
        if let Some(base) = info.get(&l.oid).and_then(|i| i.delta_base)
            && seen.contains(&base)
            && sent.contains(&l.oid)
        {
            paths.entry(base).or_default().push(path.clone());
        }
    }
    let mut candidates: Vec<Oid> = boundary;
    candidates.extend(tips.iter().copied());
    dedup(&mut candidates);
    let mut remaining: HashSet<Oid> = need.iter().copied().collect();
    for proven in fast_paths(snapshot, &need, &paths, &candidates).await? {
        remaining.remove(&proven);
    }
    if remaining.is_empty() {
        return Ok(Analysis {
            proof: Proof::FastPath,
            large_blobs,
        });
    }
    let thin_base = remaining.iter().any(|o| appended.contains(o));
    walk(snapshot, &tips, remaining, budget, thin_base).await?;
    Ok(Analysis {
        proof: Proof::Walk,
        large_blobs,
    })
}

fn snapshot_tips(snapshot: &SnapshotRepo) -> Vec<Oid> {
    let mut tips: Vec<Oid> = snapshot.refs.iter().map(|(_, oid)| *oid).collect();
    dedup(&mut tips);
    tips
}

fn dedup(oids: &mut Vec<Oid>) {
    let mut seen = HashSet::new();
    oids.retain(|o| seen.insert(*o));
}

/// An object `rev-list --objects` listed, with its path for trees and
/// blobs (empty for a root tree).
struct Listed {
    oid: Oid,
    path: Option<Vec<u8>>,
}

/// `git rev-list --objects --boundary <new tips> --not <tips>`: the objects
/// the push makes reachable, and the boundary commits.
async fn list_new(
    snapshot: &SnapshotRepo,
    new_tips: &[Oid],
    tips: &[Oid],
) -> Result<(Vec<Listed>, Vec<Oid>), WriterRefused> {
    if new_tips.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut input = oid_lines("", new_tips.iter());
    input.push_str(&oid_lines("^", tips.iter()));
    let args = ["rev-list", "--objects", "--boundary", "--stdin"];
    let out = match run(snapshot, &args, input.into_bytes()).await? {
        Ok(out) => out,
        // receive-pack's own connectivity check passed, so a failure here
        // means an object went missing: refuse as git would.
        Err(stderr) => {
            tracing::warn!(%stderr, "rev-list failed on a push's new objects");
            return Err(WriterRefused::Unreachable { thin_base: false });
        }
    };
    let mut listed = Vec::new();
    let mut boundary = Vec::new();
    for line in out.split(|b| *b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"-") {
            if let Some(oid) = parse_oid(rest) {
                boundary.push(oid);
            }
            continue;
        }
        let Some(oid) = line.get(..40).and_then(parse_oid) else {
            continue;
        };
        let path = line
            .get(40..)
            .and_then(|rest| rest.strip_prefix(b" "))
            .map(<[u8]>::to_vec);
        listed.push(Listed { oid, path });
    }
    Ok((listed, boundary))
}

/// One `{prefix}{oid}` line per object, as git's `--stdin` modes read them.
pub(crate) fn oid_lines<'a>(
    prefix: &str,
    oids: impl IntoIterator<Item = impl std::borrow::Borrow<Oid> + 'a>,
) -> String {
    use std::fmt::Write as _;
    oids.into_iter().fold(String::new(), |mut s, oid| {
        let _ = writeln!(s, "{prefix}{}", oid.borrow());
        s
    })
}

fn parse_oid(hex: &[u8]) -> Option<Oid> {
    std::str::from_utf8(hex).ok()?.parse().ok()
}

struct Info {
    kind: String,
    size: u64,
    delta_base: Option<Oid>,
}

/// Type, size and on-disk delta base of each object.
async fn object_info(
    snapshot: &SnapshotRepo,
    oids: impl Iterator<Item = Oid>,
) -> Result<HashMap<Oid, Info>, WriterRefused> {
    let input = oid_lines("", oids);
    if input.is_empty() {
        return Ok(HashMap::new());
    }
    let args = [
        "cat-file",
        "--batch-check=%(objectname) %(objecttype) %(objectsize) %(deltabase)",
    ];
    let out = run(snapshot, &args, input.into_bytes())
        .await?
        .map_err(|stderr| failed(snapshot, "cat-file --batch-check", &stderr))?;
    let mut info = HashMap::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let mut parts = line.split(' ');
        let (Some(oid), Some(kind), Some(size), Some(base)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(oid), Ok(size)) = (oid.parse::<Oid>(), size.parse::<u64>()) else {
            continue;
        };
        let delta_base = base.parse::<Oid>().ok().filter(|b| b.0 != [0; 20]);
        info.insert(
            oid,
            Info {
                kind: kind.to_owned(),
                size,
                delta_base,
            },
        );
    }
    Ok(info)
}

/// The objects of `need` found at one of their `paths` in the tree of one
/// of `candidates` (boundary commits and ref tips, reachable by
/// definition): one `cat-file` lookup each, never a tree walk.
async fn fast_paths(
    snapshot: &SnapshotRepo,
    need: &[Oid],
    paths: &HashMap<Oid, Vec<Vec<u8>>>,
    candidates: &[Oid],
) -> Result<Vec<Oid>, WriterRefused> {
    let mut queries: Vec<Oid> = Vec::new();
    let mut input = Vec::new();
    'outer: for oid in need {
        let Some(mut uses) = paths.get(oid).cloned() else {
            continue;
        };
        uses.sort();
        uses.dedup();
        for path in uses.iter().filter(|p| !p.contains(&b'\n')) {
            for commit in candidates {
                if queries.len() == MAX_FAST_PATH_LOOKUPS {
                    break 'outer;
                }
                if path.is_empty() {
                    input.extend_from_slice(format!("{commit}^{{tree}}\n").as_bytes());
                } else {
                    input.extend_from_slice(format!("{commit}:").as_bytes());
                    input.extend_from_slice(path);
                    input.push(b'\n');
                }
                queries.push(*oid);
            }
        }
    }
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let out = run(
        snapshot,
        &["cat-file", "--batch-check=%(objectname)"],
        input,
    )
    .await?
    .map_err(|stderr| failed(snapshot, "cat-file --batch-check", &stderr))?;
    let found = out
        .split(|b| *b == b'\n')
        .zip(&queries)
        .filter(|(line, want)| line.get(..40).and_then(parse_oid).as_ref() == Some(*want))
        .map(|(_, want)| *want)
        .collect();
    Ok(found)
}

/// One walk of every object reachable from the snapshot's refs, under
/// `budget`, until each of `remaining` is found.
async fn walk(
    snapshot: &SnapshotRepo,
    tips: &[Oid],
    mut remaining: HashSet<Oid>,
    budget: &Budget,
    thin_base: bool,
) -> Result<(), WriterRefused> {
    if tips.is_empty() {
        return Err(WriterRefused::Unreachable { thin_base });
    }
    let deadline = Instant::now() + budget.max_time;
    let mut child = git_command(snapshot.process.git(), snapshot, None)
        .args(["rev-list", "--objects", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io_err("run", snapshot.process.git()))?;
    let input = oid_lines("", tips.iter());
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
        });
    }
    let Some(stdout) = child.stdout.take() else {
        return Err(WriterRefused::Unreachable { thin_base });
    };
    let mut lines = BufReader::new(stdout);
    let mut visited: u64 = 0;
    let mut line = Vec::new();
    let result = tokio::time::timeout_at(deadline, async {
        loop {
            line.clear();
            match lines.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => return Err(WriterRefused::Unreachable { thin_base }),
                Ok(_) => {}
            }
            visited += 1;
            if visited > budget.max_objects {
                return Err(WriterRefused::OverBudget { thin_base });
            }
            if let Some(oid) = line.get(..40).and_then(parse_oid) {
                remaining.remove(&oid);
                if remaining.is_empty() {
                    return Ok(());
                }
            }
        }
    })
    .await;
    let _ = child.start_kill();
    let _ = child.wait().await;
    result.unwrap_or(Err(WriterRefused::OverBudget { thin_base }))
}

fn failed(snapshot: &SnapshotRepo, command: &str, stderr: &str) -> WriterRefused {
    WriterRefused::Failed(GitError::GitFailed {
        git: snapshot.process.git().to_path_buf(),
        command: command.to_owned(),
        status: "failed".to_owned(),
        stderr: stderr.to_owned(),
    })
}

/// Runs git in `snapshot` with `input` on stdin: `Ok(stdout)` when it
/// succeeds, `Err(stderr)` when it exits with an error.
pub(crate) async fn run(
    snapshot: &SnapshotRepo,
    args: &[&str],
    input: Vec<u8>,
) -> Result<Result<Vec<u8>, String>, GitError> {
    let git = snapshot.process.git();
    let mut child = git_command(git, snapshot, None)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_err("run", git))?;
    let writer = child.stdin.take().map(|mut stdin| {
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
        })
    });
    let out = child.wait_with_output().await.map_err(io_err("run", git))?;
    if let Some(writer) = writer {
        let _ = writer.await;
    }
    if out.status.success() {
        Ok(Ok(out.stdout))
    } else {
        Ok(Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_idx;

    #[test]
    fn malformed_idx_is_refused() {
        assert!(parse_idx(b"").is_none());
        assert!(parse_idx(b"\xfftOc\x00\x00\x00\x01").is_none());
        let mut huge = b"\xfftOc\x00\x00\x00\x02".to_vec();
        huge.extend(std::iter::repeat_n(0xff, 256 * 4));
        assert!(parse_idx(&huge).is_none(), "count past the end");
    }
}
