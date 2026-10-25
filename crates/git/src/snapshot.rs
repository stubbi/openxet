//! Per-request snapshot repositories (ADR 0004 items 1 and 4): a bare
//! repository in a temp dir on the cache's filesystem, holding the refs of
//! one database snapshot as `packed-refs`, the pinned config, and hard
//! links to exactly that snapshot's committed packs. There is no
//! `objects/info/alternates`, so git sees no other pack.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openxet_index::{Oid, Pack, RepoId};
use openxet_storage::{MidxExt, PackExt, PackName, create_private_dir_all};
use tempfile::TempDir;

use crate::process::{PINNED_CONFIG, ProcessEnv};
use crate::store::midx_file;
use crate::{GitError, io_err};

/// The branch `HEAD` names, in the snapshot and in `ls-refs`, until a
/// repository can store another default branch.
pub(crate) const HEAD_REF: &str = "refs/heads/main";

/// One request's view of a repository: a bare repository in `dir`, removed
/// when this is dropped. Requests already in flight keep their links when
/// packs are superseded or evicted (ADR 0004 item 4).
#[derive(Debug)]
pub struct SnapshotRepo {
    /// The repository it is a snapshot of.
    pub repo: RepoId,
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
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&layout.snapshots)
        .map_err(io_err("create a snapshot in", &layout.snapshots))?;
    let root = dir.path();
    let objects = root.join("objects/pack");
    for sub in [
        objects.clone(),
        root.join("objects/info"),
        root.join("refs"),
    ] {
        create_private_dir_all(&sub).map_err(io_err("create", &sub))?;
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

/// Writes `path`, created readable by this process's user only (design
/// §5.9: the data directory is private).
fn write(path: &Path, contents: &str) -> Result<(), GitError> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .map_err(io_err("write", path))
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

/// Replaces the `packed-refs` of the snapshot repository at `dir` with
/// `refs`, checked and sorted here: how a repack's working copy gains its
/// keep refs (ADR 0004 item 5).
pub(crate) fn write_refs(dir: &Path, refs: &[(String, Oid)]) -> Result<(), GitError> {
    let mut refs = refs.to_vec();
    for (name, _) in &refs {
        check_ref_name(name)?;
    }
    refs.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    write(&dir.join("packed-refs"), &packed_refs(&refs))
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
    let midx = match read_midx(&index) {
        Ok(Some(midx)) => midx,
        Ok(None) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err("read", &index)(e)),
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
const SHA1_BYTES: usize = 20;
const SHA1_LEN: u64 = SHA1_BYTES as u64;
const HEADER_LEN: usize = 12;
const CHUNK_ENTRY_LEN: usize = 12;

/// Reads the pack names and checksum of a SHA-1 `multi-pack-index` (git
/// `Documentation/gitformat-pack.txt`, "multi-pack-index files"): only the
/// 12-byte header (signature, version 1, hash version 1, chunk count, base
/// count, pack count), the chunk table, the `PNAM` chunk of NUL-terminated
/// `.idx` names, and the trailing checksum, never the object tables.
/// `Ok(None)` for anything malformed; the file comes from openxet's own
/// repack, so a bad one only means serving without it.
fn read_midx(path: &Path) -> std::io::Result<Option<Midx>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut header = [0u8; HEADER_LEN];
    if len < HEADER_LEN as u64 + SHA1_LEN {
        return Ok(None);
    }
    file.read_exact(&mut header)?;
    let Some((chunks, pack_count)) = parse_header(&header) else {
        return Ok(None);
    };
    let mut table = vec![0u8; (chunks + 1) * CHUNK_ENTRY_LEN];
    if (HEADER_LEN + table.len()) as u64 > len {
        return Ok(None);
    }
    file.read_exact(&mut table)?;
    let Some((start, end)) = pnam_range(&table, chunks) else {
        return Ok(None);
    };
    if start > end || end > len - SHA1_LEN {
        return Ok(None);
    }
    let Ok(pnam_len) = usize::try_from(end - start) else {
        return Ok(None);
    };
    let mut names = vec![0u8; pnam_len];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut names)?;
    let mut sum = [0u8; SHA1_BYTES];
    file.seek(SeekFrom::Start(len - SHA1_LEN))?;
    file.read_exact(&mut sum)?;
    let Some(packs) = parse_names(&names, pack_count) else {
        return Ok(None);
    };
    let checksum = sum.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    Ok(Some(Midx { packs, checksum }))
}

/// The chunk and pack counts of a SHA-1 MIDX header without base files.
fn parse_header(header: &[u8; HEADER_LEN]) -> Option<(usize, usize)> {
    if &header[..4] != MIDX_SIGNATURE || header[4] != 1 || header[5] != 1 || header[7] != 0 {
        return None;
    }
    let packs = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    Some((usize::from(header[6]), usize::try_from(packs).ok()?))
}

/// The `PNAM` chunk's byte range, from the table of `chunks` entries plus
/// the terminating one: each entry is a 4-byte id and an 8-byte offset, and
/// a chunk ends where the next begins.
fn pnam_range(table: &[u8], chunks: usize) -> Option<(u64, u64)> {
    let entry = |i: usize| -> Option<(&[u8], u64)> {
        let e = table.get(i * CHUNK_ENTRY_LEN..(i + 1) * CHUNK_ENTRY_LEN)?;
        Some((&e[..4], u64::from_be_bytes(e[4..].try_into().ok()?)))
    };
    (0..chunks).find_map(|i| {
        let (id, start) = entry(i)?;
        (id == PNAM).then(|| entry(i + 1).map(|(_, end)| (start, end)))?
    })
}

/// `count` NUL-terminated `.idx` names, as pack names without extension.
fn parse_names(chunk: &[u8], count: usize) -> Option<Vec<String>> {
    let packs: Vec<String> = chunk
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .map(|n| {
            let n = std::str::from_utf8(n).ok()?;
            Some(n.strip_suffix(".idx").unwrap_or(n).to_owned())
        })
        .collect::<Option<_>>()?;
    (packs.len() == count).then_some(packs)
}

#[cfg(test)]
mod tests {
    use super::{check_ref_name, parse_header, parse_names, pnam_range, quote, read_midx};

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
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("midx");
        for bytes in [
            &b""[..],
            b"MIDX\x01\x01\x01\x00\x00\x00\x00\x01",
            &[b'M'; 64],
        ] {
            std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("{e}"));
            assert!(matches!(read_midx(&path), Ok(None)), "{bytes:?}");
        }
        assert!(parse_header(b"MIDX\x02\x01\x01\x00\x00\x00\x00\x01").is_none());
        let mut table = b"PNAM".to_vec();
        table.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(pnam_range(&table, 1).is_none(), "no terminating entry");
        assert!(parse_names(b"pack-a.idx\0pack-b.idx\0\0\0", 3).is_none());
        assert_eq!(
            parse_names(b"pack-a.idx\0pack-b.idx\0\0\0", 2),
            Some(vec!["pack-a".to_owned(), "pack-b".to_owned()])
        );
    }
}
