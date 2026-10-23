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

    /// The object does not exist.
    #[error(
        "object {key} not found in the bucket; if it should exist, the bucket may be incomplete or the index stale (rebuild the index from the bucket)"
    )]
    NotFound {
        /// The missing key.
        key: Key,
    },

    /// The key exists with a different size than the bytes being stored.
    /// Content addressing makes identical keys identical in size, so this
    /// means corruption or a bug; the existing object is left untouched.
    #[error(
        "object {key} already exists with {existing} bytes but {new} bytes were supplied; the existing object was not modified. Content-addressed keys must have identical bytes, so verify the bucket's integrity and report this as a bug"
    )]
    SizeConflict {
        /// The conflicting key.
        key: Key,
        /// Size of the stored object.
        existing: u64,
        /// Size of the rejected bytes.
        new: u64,
    },

    /// The requested range is empty or extends past the end of the object.
    #[error(
        "range {start}..={end_inclusive} is outside object {key} of {size} bytes; request a non-empty range within 0..={last}",
        last = size.saturating_sub(1)
    )]
    RangeOutOfBounds {
        /// The key read.
        key: Key,
        /// First requested byte.
        start: u64,
        /// Last requested byte (inclusive).
        end_inclusive: u64,
        /// The object's size.
        size: u64,
    },
}
