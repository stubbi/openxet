use std::path::PathBuf;

use crate::Key;

/// A bucket failure. Messages say what happened and what to do next; they
/// carry paths and keys, never object contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// A filesystem operation failed.
    #[error(
        "failed to {op} {path}: {source}; check that the bucket directory exists, is writable and has free space"
    )]
    Io {
        /// What openxet was doing, e.g. `create temp file`.
        op: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The bucket's filesystem can't make hard links, which writing records
    /// needs ([`crate::Bucket::create`]).
    #[error(
        "the filesystem holding the bucket at {} does not support hard links ({source}), which openxet needs to write its registration records; put the data directory on a filesystem that does, such as ext4, XFS, Btrfs, ZFS, APFS or NTFS (not FAT or exFAT, and not a network mount without hard links)",
        .path.display()
    )]
    NoHardLinks {
        /// The bucket's root.
        path: PathBuf,
        /// Why the probe link failed.
        #[source]
        source: std::io::Error,
    },

    /// The object does not exist.
    #[error(
        "object {key} not found in the bucket; if it should exist, the bucket may be incomplete or the index stale (rebuild the index from the bucket)"
    )]
    NotFound {
        /// The missing key.
        key: Box<Key>,
    },

    /// The key exists with a different size than the bytes being stored.
    /// Content addressing makes identical keys identical in size, so this
    /// means a bug or a hash collision. The existing object is left untouched
    /// when this is reported; detection is best-effort under concurrent puts.
    #[error(
        "object {key} already exists with {existing} bytes but {new} bytes were supplied; the existing object was not modified. Content-addressed keys must have identical bytes, so verify the bucket's integrity and report this as a bug"
    )]
    SizeConflict {
        /// The conflicting key.
        key: Box<Key>,
        /// Size of the stored object.
        existing: u64,
        /// Size of the rejected bytes.
        new: u64,
    },

    /// A record object doesn't decode, or names a different key than the one
    /// it is stored under.
    #[error(
        "record {key} in the bucket is unreadable: {reason}{hint}",
        hint = if *.newer {
            "; it was written by a newer openxet, so run that version"
        } else {
            "; the bucket may be corrupt or written by another program. Restore the object \
             from a backup, or report this at https://github.com/stubbi/openxet/issues"
        }
    )]
    CorruptRecord {
        /// The record's key.
        key: Box<Key>,
        /// What is wrong with it.
        reason: String,
        /// Whether the record has a newer format version than this build
        /// reads.
        newer: bool,
    },

    /// The requested range is empty or extends past the end of the object.
    #[error(
        "range {start}..={end_inclusive} is outside object {key} of {size} bytes; request a non-empty range within 0..={last}",
        last = size.saturating_sub(1)
    )]
    RangeOutOfBounds {
        /// The key read.
        key: Box<Key>,
        /// First requested byte.
        start: u64,
        /// Last requested byte (inclusive).
        end_inclusive: u64,
        /// The object's size.
        size: u64,
    },

    /// A key, or a part of one, that no bucket object may have: a malformed
    /// repository id or pack name, or a kind of key the operation doesn't
    /// take.
    #[error("{key} is not valid here: {reason}")]
    InvalidKey {
        /// The key or part, escaped.
        key: String,
        /// What a valid one looks like, or which operation to use instead.
        reason: &'static str,
    },

    /// A streamed body held more or fewer bytes than its declared length.
    #[error(
        "the body sent for {key} held {received}{more} bytes, but {expected} were declared; nothing was stored. Send the whole object with its exact length",
        more = if received > expected { " or more" } else { "" }
    )]
    LengthMismatch {
        /// The key being written.
        key: Box<Key>,
        /// The declared length.
        expected: u64,
        /// The bytes read before the mismatch was found: all of them when
        /// too few, and the first ones past `expected` when too many.
        received: u64,
    },
}
