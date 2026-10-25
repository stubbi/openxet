//! The Hub object layer (ADR 0004 item 6): gix in-process, off the wire
//! path, and only ever on a request's [`SnapshotRepo`], never on the node
//! cache, whose superseded packs can still hold objects no committed pack
//! has.
//!
//! Reads ([`read`]) take a revision: a ref of the snapshot, or an object id
//! that passes R22 from the snapshot's refs exactly as a fetch want does.
//! Writes ([`write`]) build their objects in-process ([`tree`]), pack them,
//! index the pack with `git index-pack --strict`, check connectivity
//! against the snapshot and apply the writer reachability rule, then publish
//! through the fenced commit transaction like a push.
//!
//! [`SnapshotRepo`]: crate::SnapshotRepo

mod pointer;
mod read;
mod tree;
mod write;

use bytes::Bytes;
use openxet_auth::UserPrincipal;
use openxet_index::Oid;

pub use pointer::{Pointer, canonical_pointer, parse_pointer};
pub(crate) use write::HubWrites;
#[cfg(feature = "fault-injection")]
pub use write::{FAULT_OMIT_BLOBS_ACTOR, HubWriteHold};

/// What a Hub read is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision<'a> {
    /// A ref name: a branch or tag name (`main`, `v1.0`), a full ref name
    /// (`refs/heads/main`, `refs/pr/1`), or `HEAD`.
    Ref(&'a str),
    /// A commit or annotated tag id. Tree and blob ids are never revisions.
    Oid(Oid),
}

impl std::fmt::Display for Revision<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ref(name) => f.write_str(name),
            Self::Oid(oid) => write!(f, "{oid}"),
        }
    }
}

/// A revision resolved to its commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// The commit.
    pub commit: Oid,
    /// Its root tree.
    pub tree: Oid,
}

/// One entry of a tree listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path from the root of the commit's tree, `/`-separated.
    pub path: String,
    /// What it is.
    pub kind: EntryKind,
}

/// A file or a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A blob (a regular or executable file, or a symlink).
    File {
        /// The blob.
        oid: Oid,
        /// The blob's size: the pointer's own size for an LFS file.
        size: u64,
        /// The LFS pointer the blob holds, if it is one.
        lfs: Option<Pointer>,
    },
    /// A tree.
    Dir {
        /// The tree.
        oid: Oid,
    },
}

/// A file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    /// A file stored in Git.
    Inline {
        /// The blob.
        oid: Oid,
        /// Its bytes.
        bytes: Bytes,
    },
    /// A file stored with Git LFS (and Xet): the blob is its pointer.
    Lfs {
        /// The pointer blob.
        oid: Oid,
        /// What it points at.
        pointer: Pointer,
    },
}

/// A person in a commit or tag: who wrote it, or who committed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The name.
    pub name: String,
    /// The email address.
    pub email: String,
    /// When, in Unix seconds.
    pub time: i64,
}

/// A commit, as Hub listings show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// The commit.
    pub oid: Oid,
    /// Its root tree.
    pub tree: Oid,
    /// Its parents, first parent first.
    pub parents: Vec<Oid>,
    /// Its author.
    pub author: Signature,
    /// Its committer.
    pub committer: Signature,
    /// Its whole message.
    pub message: String,
}

impl CommitInfo {
    /// The message's first line: the Hub's `title` (and a Hub commit's
    /// `summary`).
    #[must_use]
    pub fn title(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }

    /// The message after the first line and the blank lines that follow it,
    /// without trailing newlines: a Hub commit's `description`.
    #[must_use]
    pub fn body(&self) -> &str {
        self.message
            .split_once('\n')
            .map_or("", |(_, rest)| rest)
            .trim_start_matches('\n')
            .trim_end_matches('\n')
    }
}

/// A name and address for the commits and tags openxet writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    /// The name.
    pub name: String,
    /// The email address.
    pub email: String,
}

impl Ident {
    /// The author of a Hub commit by `who` (ruling M2-R7): the subject, at
    /// the credential's `email` claim or else
    /// `{subject}@users.noreply.openxet.com`. Characters git refuses in an
    /// identity (`<`, `>`, line breaks, NUL) are dropped.
    #[must_use]
    pub fn author(who: &UserPrincipal) -> Self {
        let email = who
            .email
            .clone()
            .unwrap_or_else(|| format!("{}@users.noreply.openxet.com", who.subject));
        Self {
            name: clean(&who.subject),
            email: clean(&email),
        }
    }

    /// openxet itself, the committer of every Hub commit (rulings M2-R5 and
    /// M2-R7).
    #[must_use]
    pub fn openxet() -> Self {
        Self {
            name: "openxet".to_owned(),
            email: "noreply@openxet.com".to_owned(),
        }
    }
}

/// `s` without the characters that would end a git identity field early or
/// break its line (git `ident.c`, `strbuf_addstr_without_crud`; fsck's
/// `badName` and `badEmail`).
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '<' | '>' | '\n' | '\r' | '\0'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A Hub commit (ruling M2-R7).
#[derive(Debug, Clone)]
pub struct HubCommit<'a> {
    /// The branch: a name (`main`) or a full ref name (`refs/heads/main`,
    /// `refs/pr/1`). It must exist, unless the repository has no refs at
    /// all, when the commit is its first.
    pub branch: &'a str,
    /// The client's `parentCommit`: the branch tip it expects.
    pub parent: Option<Oid>,
    /// The message's first line.
    pub summary: &'a str,
    /// The rest of the message, after a blank line; may be empty.
    pub description: &'a str,
    /// What changes, applied in order on the parent's tree.
    pub ops: Vec<Op>,
}

/// One change of a Hub commit. Paths are `/`-separated, relative to the
/// root, with no empty, `.`, `..` or `.git` component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A file stored in Git with this content.
    File {
        /// Its path.
        path: String,
        /// Its content.
        content: Bytes,
    },
    /// A file stored with LFS: its blob is the canonical pointer
    /// (ADR 0005 item 9). The caller checks that the sha256 is registered
    /// to the repository's scope.
    LfsFile {
        /// Its path.
        path: String,
        /// The file's SHA-256.
        sha256: [u8; 32],
        /// The file's size.
        size: u64,
    },
    /// Deletes the file at this path, which must be one.
    DeleteFile(String),
    /// Deletes the folder at this path, which must be one, and everything
    /// under it.
    DeleteFolder(String),
}

/// An annotated tag to create with a Hub ref write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    /// The tag's message.
    pub message: String,
    /// Who tags.
    pub tagger: Ident,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use openxet_auth::UserPrincipal;
    use openxet_index::Oid;

    use super::{CommitInfo, Ident, Signature};

    #[test]
    fn identities_drop_what_git_refuses() {
        let who = UserPrincipal {
            subject: " eve<x>\n ".to_owned(),
            name: "eve".to_owned(),
            email: None,
            claims: BTreeMap::new(),
        };
        let ident = Ident::author(&who);
        assert_eq!(ident.name, "evex");
        assert_eq!(ident.email, "evex @users.noreply.openxet.com");
        let mut with_email = who;
        with_email.email = Some("e@example.org>".to_owned());
        assert_eq!(Ident::author(&with_email).email, "e@example.org");
    }

    #[test]
    fn title_and_body() {
        let sig = Signature {
            name: String::new(),
            email: String::new(),
            time: 0,
        };
        let info = |message: &str| CommitInfo {
            oid: Oid([0; 20]),
            tree: Oid([0; 20]),
            parents: Vec::new(),
            author: sig.clone(),
            committer: sig.clone(),
            message: message.to_owned(),
        };
        assert_eq!(info("t\n\nb\nc\n").title(), "t");
        assert_eq!(info("t\n\nb\nc\n").body(), "b\nc");
        assert_eq!(info("t\n").body(), "");
        assert_eq!(info("").title(), "");
    }
}
