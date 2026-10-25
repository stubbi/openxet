//! Per-request snapshot repositories (ADR 0004 items 1 and 4): a bare
//! repository in a temp dir on the cache's filesystem, holding the refs of
//! one database snapshot as `packed-refs`, the pinned config, and hard
//! links to exactly that snapshot's committed packs. There is no
//! `objects/info/alternates`, so git sees no other pack.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openxet_index::{Oid, Pack};
use openxet_storage::{MidxExt, PackExt, PackName};
use tempfile::TempDir;

use crate::process::{PINNED_CONFIG, ProcessEnv};
use crate::store::midx_file;
use crate::{GitError, io_err};

/// The branch `HEAD` names (Task 7 resolves it the same way until a
/// repository can store another default branch).
const HEAD_REF: &str = "refs/heads/main";

/// One request's view of a repository: a bare repository in `dir`, removed
/// when this is dropped. Requests already in flight keep their links when
/// packs are superseded or evicted (ADR 0004 item 4).
#[derive(Debug)]
pub struct SnapshotRepo {
    /// The repository directory, under `{cache_dir}/snapshots/{uuid}`.
    pub dir: TempDir,
    /// Every ref and its target, ordered by name, as `packed-refs` holds
    /// them.
    pub refs: Vec<(String, Oid)>,
    /// The committed packs linked into `objects/pack`.
    pub packs: Vec<Pack>,
    /// The full-repack generation read with them, which a commit must still
    /// match (ADR 0004 item 3).
    pub full_repack_gen: u64,
    pub(crate) process: Arc<ProcessEnv>,
}

/// What a snapshot directory is built from.
pub(crate) struct Layout {
    pub(crate) snapshots: PathBuf,
    pub(crate) hooks: PathBuf,
    pub(crate) refs: Vec<(String, Oid)>,
    pub(crate) pack_dir: PathBuf,
    pub(crate) packs: Vec<PackName>,
    /// The cached MIDX generation to link if it covers only `packs`.
    pub(crate) midx: Option<(PathBuf, u64)>,
}

/// Builds the snapshot directory. The packs (and MIDX generation) must be
/// hydrated, and the cache's shared lock held.
pub(crate) fn build(layout: &Layout) -> Result<TempDir, GitError> {
    let name = uuid::Uuid::new_v4().simple().to_string();
    let dir = tempfile::Builder::new()
        .prefix(&name)
        .rand_bytes(0)
        .tempdir_in(&layout.snapshots)
        .map_err(io_err("create a snapshot in", &layout.snapshots))?;
    let root = dir.path();
    let objects = root.join("objects/pack");
    for sub in [
        objects.clone(),
        root.join("objects/info"),
        root.join("refs"),
    ] {
        fs::create_dir_all(&sub).map_err(io_err("create", &sub))?;
    }
    write(&root.join("HEAD"), &format!("ref: {HEAD_REF}\n"))?;
    write(&root.join("config"), &config(&layout.hooks))?;
    write(&root.join("packed-refs"), &packed_refs(&layout.refs))?;

    for name in &layout.packs {
        for ext in [
            PackExt::Pack,
            PackExt::Idx,
            PackExt::Rev,
            PackExt::Bitmap,
            PackExt::Mtimes,
        ] {
            let file = format!("{name}.{}", ext.as_str());
            let from = layout.pack_dir.join(&file);
            let required = matches!(ext, PackExt::Pack | PackExt::Idx);
            if required || from.exists() {
                link(&from, &objects.join(&file))?;
            }
        }
    }
    if let Some((dir, generation)) = &layout.midx {
        link_midx(dir, *generation, &layout.packs, &objects)?;
    }
    Ok(dir)
}

fn write(path: &Path, contents: &str) -> Result<(), GitError> {
    fs::write(path, contents).map_err(io_err("write", path))
}

fn link(from: &Path, to: &Path) -> Result<(), GitError> {
    fs::hard_link(from, to).map_err(io_err("hard-link into the snapshot", from))
}

/// The snapshot's `config`: a bare repository with [`PINNED_CONFIG`] and
/// `core.hooksPath` (ADR 0004 item 1). Every value is quoted, so a cache path
/// can't break out of its line.
fn config(hooks: &Path) -> String {
    let mut out = String::from("[core]\n\trepositoryformatversion = 0\n\tbare = true\n");
    let _ = writeln!(out, "\thooksPath = {}", quote(&hooks.to_string_lossy()));
    for (key, value) in PINNED_CONFIG {
        if let Some((section, name)) = key.split_once('.') {
            let _ = write!(out, "[{section}]\n\t{name} = {}\n", quote(value));
        }
    }
    out
}

/// A git config value in double quotes, with the escapes git reads.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `packed-refs` for `refs`, which the caller has checked and sorted by
/// name in byte order, as git's `sorted` trait promises.
fn packed_refs(refs: &[(String, Oid)]) -> String {
    let mut out = String::from("# pack-refs with: sorted \n");
    for (name, oid) in refs {
        let _ = writeln!(out, "{oid} {name}");
    }
    out
}

/// Checks a ref name from the index before it goes into `packed-refs`:
/// under `refs/`, with no whitespace or control character that could start
/// another line. Writers validate names fully; this keeps a bad row from
/// changing the file's meaning.
pub(crate) fn check_ref_name(name: &str) -> Result<(), GitError> {
    if name.starts_with("refs/") && name.bytes().all(|b| b > b' ' && b != 0x7f) {
        Ok(())
    } else {
        Err(GitError::Corrupt(format!(
            "ref name {:?} is not a valid ref",
            name.escape_debug().to_string()
        )))
    }
}

/// Links MIDX generation `generation` if every pack it covers is in the
/// snapshot, so it never names a pack outside the set (ADR 0004 item 1);
/// otherwise the snapshot serves its packs without one. Its bitmap and
/// reverse index take the names git looks for, from the index's checksum.
fn link_midx(
    dir: &Path,
    generation: u64,
    packs: &[PackName],
    objects: &Path,
) -> Result<(), GitError> {
    let index = midx_file(dir, generation, MidxExt::MultiPackIndex);
    let bytes = match fs::read(&index) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err("read", &index)(e)),
    };
    let Some(midx) = parse_midx(&bytes) else {
        return Ok(());
    };
    let set: HashSet<&str> = packs.iter().map(PackName::as_str).collect();
    if !midx.packs.iter().all(|p| set.contains(p.as_str())) {
        return Ok(());
    }
    link(&index, &objects.join("multi-pack-index"))?;
    for ext in [MidxExt::Bitmap, MidxExt::Rev] {
        let from = midx_file(dir, generation, ext);
        if from.exists() {
            let to = objects.join(format!(
                "multi-pack-index-{}.{}",
                midx.checksum,
                ext.as_str()
            ));
            link(&from, &to)?;
        }
    }
    Ok(())
}

/// What a snapshot needs from a `multi-pack-index`.
struct Midx {
    /// The covered packs' names, without extension.
    packs: Vec<String>,
    /// The trailing checksum in hex, which names the bitmap and `.rev`.
    checksum: String,
}

const MIDX_SIGNATURE: &[u8; 4] = b"MIDX";
const PNAM: &[u8; 4] = b"PNAM";
const SHA1_LEN: usize = 20;

/// Reads the pack names and checksum of a SHA-1 `multi-pack-index` (git
/// `Documentation/gitformat-pack.txt`, "multi-pack-index files"): a 12-byte
/// header (signature, version 1, hash version 1, chunk count, base count,
/// pack count), the chunk table, and the `PNAM` chunk of NUL-terminated
/// `.idx` names. `None` for anything else; the file comes from openxet's own
/// repack, so a bad one only means serving without it.
fn parse_midx(bytes: &[u8]) -> Option<Midx> {
    let header = bytes.get(..12)?;
    if &header[..4] != MIDX_SIGNATURE || header[4] != 1 || header[5] != 1 || header[7] != 0 {
        return None;
    }
    let chunks = usize::from(header[6]);
    let pack_count = usize::try_from(u32::from_be_bytes(header[8..12].try_into().ok()?)).ok()?;
    let entry = |i: usize| -> Option<(&[u8], usize)> {
        let at = 12 + i * 12;
        let e = bytes.get(at..at + 12)?;
        let offset = usize::try_from(u64::from_be_bytes(e[4..].try_into().ok()?)).ok()?;
        Some((&e[..4], offset))
    };
    let mut names = None;
    for i in 0..chunks {
        let (id, start) = entry(i)?;
        if id == PNAM {
            let (_, end) = entry(i + 1)?;
            names = Some(bytes.get(start..end)?);
        }
    }
    let packs: Vec<String> = names?
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .map(|n| {
            let n = std::str::from_utf8(n).ok()?;
            Some(n.strip_suffix(".idx").unwrap_or(n).to_owned())
        })
        .collect::<Option<_>>()?;
    if packs.len() != pack_count {
        return None;
    }
    let sum = bytes.get(bytes.len().checked_sub(SHA1_LEN)?..)?;
    let checksum = sum.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    Some(Midx { packs, checksum })
}

#[cfg(test)]
mod tests {
    use super::{check_ref_name, parse_midx, quote};

    #[test]
    fn ref_names_cannot_add_lines() {
        assert!(check_ref_name("refs/heads/main").is_ok());
        assert!(check_ref_name("refs/tags/v1.0").is_ok());
        for bad in [
            "HEAD",
            "main",
            "refs/heads/a\nrefs/heads/b",
            "refs/heads/a b",
            "refs/x\0",
        ] {
            assert!(check_ref_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn config_values_are_quoted() {
        assert_eq!(quote("/a b/hooks"), "\"/a b/hooks\"");
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn malformed_midx_is_ignored() {
        assert!(parse_midx(b"").is_none());
        assert!(parse_midx(b"MIDX\x01\x01\x01\x00\x00\x00\x00\x01").is_none());
        let mut huge = b"MIDX\x01\x01\x01\x00\x00\x00\x00\x01PNAM".to_vec();
        huge.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(parse_midx(&huge).is_none());
    }
}
