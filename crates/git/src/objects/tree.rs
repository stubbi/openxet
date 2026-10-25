//! Builds the objects of a Hub write in-process (ruling M2-R7): the
//! parent's tree is read from the snapshot only along the paths the
//! operations touch, and every blob, tree, commit and tag the write creates
//! is kept, so the write's pack carries all of them. A file whose bytes
//! match an object the snapshot already holds is sent all the same: the
//! writer reachability rule then never depends on what else is stored.

use std::collections::{BTreeMap, HashSet};

use gix::ObjectId;
use gix::bstr::BString;
use gix::objs::tree::{EntryKind as GitKind, EntryMode};
use gix::objs::{Kind, WriteTo};
use openxet_index::Oid;

use super::pointer::canonical_pointer;
use super::read::{commit_tree, objects_err, oid_of};
use super::{Ident, Op};
use crate::GitError;
use crate::reach::to_gix;

/// The longest path a Hub write takes, in bytes (Linux's `PATH_MAX`).
const MAX_PATH_BYTES: usize = 4096;

/// One object a Hub write creates.
pub(crate) struct NewObject {
    pub(crate) kind: Kind,
    pub(crate) id: ObjectId,
    pub(crate) data: Vec<u8>,
}

/// The objects a Hub write creates, each once, in creation order.
#[derive(Default)]
pub(crate) struct Objects {
    list: Vec<NewObject>,
    seen: HashSet<ObjectId>,
}

impl Objects {
    fn add(&mut self, kind: Kind, data: Vec<u8>) -> Result<ObjectId, GitError> {
        let id =
            gix::objs::compute_hash(gix::hash::Kind::Sha1, kind, &data).map_err(objects_err)?;
        if self.seen.insert(id) {
            self.list.push(NewObject { kind, id, data });
        }
        Ok(id)
    }

    pub(crate) fn write(&mut self, object: &impl WriteTo) -> Result<ObjectId, GitError> {
        let mut data = Vec::new();
        object.write_to(&mut data).map_err(objects_err)?;
        self.add(object.kind(), data)
    }

    pub(crate) fn into_vec(self) -> Vec<NewObject> {
        self.list
    }
}

/// `path`'s components for a write: non-empty, at most 4096 bytes, with no
/// empty, `.`, `..` or `.git` (in any case) component and no NUL, the names
/// `git fsck` refuses outright. git's stricter checks (`.git` look-alikes on
/// NTFS and HFS+) run in `index-pack --strict`.
///
/// # Errors
///
/// [`GitError::InvalidHubWrite`] naming the rule.
pub(crate) fn components(path: &str) -> Result<Vec<&str>, GitError> {
    let bad = |why: &str| {
        GitError::InvalidHubWrite(format!(
            "invalid path {path:?}: {why}; use a relative path of `/`-separated names"
        ))
    };
    if path.is_empty() {
        return Err(bad("it is empty"));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(bad("it is longer than 4096 bytes"));
    }
    let comps: Vec<&str> = path.split('/').collect();
    for c in &comps {
        if c.is_empty() {
            return Err(bad(
                "it has an empty component (a leading, trailing or double `/`)",
            ));
        }
        if *c == "." || *c == ".." {
            return Err(bad("it has a `.` or `..` component"));
        }
        if c.eq_ignore_ascii_case(".git") {
            return Err(bad("`.git` is reserved by Git"));
        }
        if c.contains('\0') {
            return Err(bad("it contains a NUL character"));
        }
    }
    Ok(comps)
}

/// The path an operation names.
pub(crate) fn op_path(op: &Op) -> &str {
    match op {
        Op::File { path, .. }
        | Op::LfsFile { path, .. }
        | Op::DeleteFile(path)
        | Op::DeleteFolder(path) => path,
    }
}

/// Builds a commit on `parent` (none for a repository's first commit) that
/// applies `ops` in order, authored by `author` and committed by openxet at
/// `now`. Returns the commit and every object it created.
pub(crate) fn build_commit(
    repo: &gix::Repository,
    parent: Option<Oid>,
    ops: &[Op],
    author: &Ident,
    message: &str,
    now: i64,
) -> Result<(Oid, Objects), GitError> {
    let mut objects = Objects::default();
    let mut root = match parent {
        Some(parent) => Dir::load(repo, commit_tree(repo, parent)?)?,
        None => Dir::default(),
    };
    for op in ops {
        let path = op_path(op);
        let comps = components(path)?;
        match op {
            Op::File { content, .. } => {
                let blob = objects.add(Kind::Blob, content.to_vec())?;
                root.put(repo, &comps, blob, path)?;
            }
            Op::LfsFile { sha256, size, .. } => {
                let blob = objects.add(Kind::Blob, canonical_pointer(sha256, *size))?;
                root.put(repo, &comps, blob, path)?;
            }
            Op::DeleteFile(_) => root.delete(repo, &comps, false, path)?,
            Op::DeleteFolder(_) => root.delete(repo, &comps, true, path)?,
        }
    }
    let tree = match root.write(&mut objects)? {
        Some(tree) => tree,
        None => objects.write(&gix::objs::Tree::empty())?,
    };
    let commit = gix::objs::Commit {
        tree,
        parents: parent.map(to_gix).into_iter().collect(),
        author: signature(author, now),
        committer: signature(&Ident::openxet(), now),
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    let id = objects.write(&commit)?;
    Ok((oid_of(id)?, objects))
}

/// `ident` at `now`, in UTC.
pub(crate) fn signature(ident: &Ident, now: i64) -> gix::actor::Signature {
    gix::actor::Signature {
        name: ident.name.as_str().into(),
        email: ident.email.as_str().into(),
        time: gix::date::Time {
            seconds: now,
            offset: 0,
        },
    }
}

/// A folder being edited: its entries by name, subfolders loaded only when
/// an operation enters them.
#[derive(Default)]
struct Dir {
    entries: BTreeMap<BString, Node>,
}

enum Node {
    /// A file (or symlink, or submodule): kept as it is unless replaced.
    Leaf { mode: EntryMode, id: ObjectId },
    /// A folder no operation entered.
    Tree(ObjectId),
    /// A folder an operation entered.
    Edited(Dir),
}

impl Dir {
    fn load(repo: &gix::Repository, id: ObjectId) -> Result<Self, GitError> {
        let tree = repo.find_tree(id).map_err(objects_err)?;
        let decoded = tree.decode().map_err(objects_err)?;
        let entries = decoded
            .entries
            .iter()
            .map(|e| {
                let id = e.oid.to_owned();
                let node = if e.mode.is_tree() {
                    Node::Tree(id)
                } else {
                    Node::Leaf { mode: e.mode, id }
                };
                (e.filename.to_owned(), node)
            })
            .collect();
        Ok(Self { entries })
    }

    /// The folder holding the last of `comps`. With `create`, missing
    /// folders are made and a file in the way is an error; without, `None`
    /// if the folder doesn't exist.
    fn parent_mut(
        &mut self,
        repo: &gix::Repository,
        comps: &[&str],
        create: bool,
        whole: &str,
    ) -> Result<Option<&mut Dir>, GitError> {
        let Some((first, rest)) = comps.split_first() else {
            return Ok(Some(self));
        };
        let name = BString::from(*first);
        if !self.entries.contains_key(&name) {
            if !create {
                return Ok(None);
            }
            self.entries
                .insert(name.clone(), Node::Edited(Dir::default()));
        }
        let Some(node) = self.entries.get_mut(&name) else {
            return Ok(None);
        };
        if let Node::Tree(id) = node {
            *node = Node::Edited(Dir::load(repo, *id)?);
        }
        match node {
            Node::Edited(dir) => dir.parent_mut(repo, rest, create, whole),
            Node::Leaf { .. } if create => Err(GitError::InvalidHubWrite(format!(
                "cannot write {whole:?}: {first:?} on its path is a file; delete it first"
            ))),
            _ => Ok(None),
        }
    }

    /// Puts blob `id` at `comps`, keeping an existing file's executable bit.
    fn put(
        &mut self,
        repo: &gix::Repository,
        comps: &[&str],
        id: ObjectId,
        whole: &str,
    ) -> Result<(), GitError> {
        let Some((last, folders)) = comps.split_last() else {
            return Ok(());
        };
        let Some(dir) = self.parent_mut(repo, folders, true, whole)? else {
            return Ok(());
        };
        let name = BString::from(*last);
        let mode = match dir.entries.get(&name) {
            Some(Node::Tree(_) | Node::Edited(_)) => {
                return Err(GitError::InvalidHubWrite(format!(
                    "cannot write {whole:?}: it is a folder; delete the folder first"
                )));
            }
            Some(Node::Leaf { mode, .. }) if mode.kind() == GitKind::BlobExecutable => *mode,
            _ => EntryMode::from(GitKind::Blob),
        };
        dir.entries.insert(name, Node::Leaf { mode, id });
        Ok(())
    }

    /// Deletes the file (or with `folder`, the folder) at `comps`.
    fn delete(
        &mut self,
        repo: &gix::Repository,
        comps: &[&str],
        folder: bool,
        whole: &str,
    ) -> Result<(), GitError> {
        let missing = || GitError::EntryNotFound {
            path: whole.to_owned(),
        };
        let (last, folders) = comps.split_last().ok_or_else(missing)?;
        let dir = self
            .parent_mut(repo, folders, false, whole)?
            .ok_or_else(missing)?;
        let name = BString::from(*last);
        let matches = match dir.entries.get(&name) {
            Some(Node::Leaf { .. }) => !folder,
            Some(Node::Tree(_) | Node::Edited(_)) => folder,
            None => false,
        };
        if !matches {
            return Err(missing());
        }
        dir.entries.remove(&name);
        Ok(())
    }

    /// Writes every folder an operation entered, depth first, and returns
    /// this one's tree; `None` if it ended up empty, as Git stores no empty
    /// folder.
    fn write(self, objects: &mut Objects) -> Result<Option<ObjectId>, GitError> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for (filename, node) in self.entries {
            let (mode, oid) = match node {
                Node::Leaf { mode, id } => (mode, id),
                Node::Tree(id) => (EntryMode::from(GitKind::Tree), id),
                Node::Edited(dir) => match dir.write(objects)? {
                    Some(id) => (EntryMode::from(GitKind::Tree), id),
                    None => continue,
                },
            };
            entries.push(gix::objs::tree::Entry {
                mode,
                filename,
                oid,
            });
        }
        if entries.is_empty() {
            return Ok(None);
        }
        // Git's tree order sorts a folder as if its name ended in `/`.
        entries.sort();
        objects.write(&gix::objs::Tree { entries }).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::components;

    #[test]
    fn write_paths() {
        assert_eq!(components("a/b.txt").ok(), Some(vec!["a", "b.txt"]));
        assert!(components("a b/ü.txt").is_ok());
        for bad in [
            "", "/a", "a/", "a//b", ".", "a/..", ".git", "x/.Git/y", "a\0b",
        ] {
            assert!(components(bad).is_err(), "{bad:?}");
        }
        assert!(components(&"a/".repeat(2049)).is_err());
    }
}
