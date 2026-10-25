//! The pack of a Hub write: built in-process, written into the snapshot's
//! quarantine, checked by git with the quarantine as its object directory,
//! then linked into the snapshot (ADR 0004 item 6).

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use gix_pack::data::output;
use openxet_storage::{PackExt, PackName, create_private_dir_all};
use tokio::io::AsyncWriteExt as _;

use super::read::objects_err;
use super::tree::NewObject;
use crate::publish::PackFiles;
use crate::{GitError, SnapshotRepo, git_command, io_err};

/// Writes `objects` whole, undeltified, as one version 2 pack in `dir`,
/// named by its checksum as git names packs. Returns the files, the object
/// count and the pack's size.
pub(super) fn write_pack(
    dir: &Path,
    objects: &[NewObject],
) -> Result<(PackFiles, u32, u64), GitError> {
    create_private_dir_all(dir).map_err(io_err("create", dir))?;
    let entries = objects
        .iter()
        .map(|o| {
            output::Entry::from_data(
                &output::Count::from_data(o.id, None),
                &gix::objs::Data::new(&o.data, o.kind, gix::hash::Kind::Sha1),
                gix::zlib::Compression::default(),
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(objects_err)?;
    let count = u32::try_from(entries.len()).map_err(|_| {
        GitError::InvalidHubWrite("a Hub write creates too many objects".to_owned())
    })?;
    let tmp = dir.join("incoming.pack");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(io_err("create", &tmp))?;
    let mut writer = output::bytes::FromEntriesIter::new(
        std::iter::once(Ok(entries)),
        std::io::BufWriter::new(file),
        count,
        gix_pack::data::Version::V2,
        gix::hash::Kind::Sha1,
    );
    for written in writer.by_ref() {
        written.map_err(objects_err)?;
    }
    let digest = writer
        .digest()
        .ok_or_else(|| GitError::Objects("the Hub write's pack has no checksum".to_owned()))?;
    let mut out = writer.into_write();
    out.flush().map_err(io_err("write", &tmp))?;
    let file = out
        .into_inner()
        .map_err(|e| io_err("write", &tmp)(e.into_error()))?;
    file.sync_all().map_err(io_err("write", &tmp))?;
    let name = PackName::parse(&format!("pack-{digest}"))
        .map_err(|_| GitError::Corrupt(format!("pack checksum {digest} makes no pack name")))?;
    let files = PackFiles {
        dir: dir.to_path_buf(),
        name,
    };
    let pack = files.path(PackExt::Pack);
    std::fs::rename(&tmp, &pack).map_err(io_err("rename", &tmp))?;
    let bytes = std::fs::metadata(&pack)
        .map_err(io_err("read", &pack))?
        .len();
    Ok((files, count, bytes))
}

/// Links the checked pack into the snapshot's own pack directory, as
/// receive-pack migrates its quarantine, so the writer reachability rule
/// sees it. The snapshot is this request's alone.
pub(super) fn link_pack(files: &PackFiles, objects_pack: &Path) -> Result<(), GitError> {
    for ext in [PackExt::Pack, PackExt::Idx, PackExt::Rev] {
        let from = files.path(ext);
        if ext == PackExt::Rev && !from.exists() {
            continue;
        }
        let to = objects_pack.join(format!("{}.{}", files.name, ext.as_str()));
        std::fs::hard_link(&from, &to).map_err(io_err("link the Hub write's pack into", &to))?;
    }
    Ok(())
}

/// Runs git in `snapshot` with `quarantine` as its object directory and the
/// snapshot's objects as its only alternate (the layout receive-pack gives
/// its connectivity check): `Ok(stdout)` when it succeeds, `Err(stderr)`
/// when it exits with an error.
pub(super) async fn run_quarantined(
    snapshot: &SnapshotRepo,
    quarantine: &Path,
    args: &[&OsStr],
    input: Vec<u8>,
) -> Result<Result<Vec<u8>, String>, GitError> {
    let git = snapshot.process.git();
    let alternate: PathBuf = snapshot.dir.path().join("objects");
    let mut child = git_command(git, snapshot, None)
        .args(args)
        .env("GIT_OBJECT_DIRECTORY", quarantine)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &alternate)
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
