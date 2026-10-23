//! The Xet protocol for openxet: hashes, xorbs and shards.
//!
//! This crate is openxet's only doorway to Hugging Face's `xet-core` crates
//! (ADR 0001). Parsing, serialization, compression and hashing are
//! `xet-core-structures`' own functions, so formats stay byte-exact with
//! upstream; nothing here reimplements them. What this crate adds is
//! openxet's policy on untrusted input: size and count limits, exact
//! decompressed lengths, and shard verification. Spec:
//! <https://huggingface.co/docs/xet>.

#![forbid(unsafe_code)]

mod hash;
mod shard;
mod xorb;

/// The Xet protocol spec version openxet implements (design section 1).
pub const PROTOCOL_SPEC: &str = "1.1.0";

pub use hash::{MerkleHash, chunk_hash, file_hash, hash_from_hex, hash_to_hex, range_hash};
pub use shard::{
    MAX_REFERENCED_CHUNKS, MAX_REFERENCED_XORBS, MAX_UPLOAD_SHARD_BYTES, Segment, ShardFileEntry,
    UploadShard, VerifyError, VerifyLimits, XorbChunkLookup, read_upload_shard, verify_segment,
    verify_shard,
};
pub use xorb::{
    MAX_CHUNK_BYTES, MAX_XORB_BODY_BYTES, MAX_XORB_BYTES, MAX_XORB_CHUNKS, StoredXorb, XorbChunks,
    read_xorb_chunks, xorb_hash, xorb_with_footer,
};

/// Errors from parsing protocol data.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// A string is not a hash in the spec's 64-character hex form.
    #[error(
        "invalid Xet hash: expected 64 hex characters as in \
         https://huggingface.co/docs/xet/api#converting-hashes-to-strings"
    )]
    InvalidHex(#[source] xet_core_structures::merklehash::DataHashError),
    /// Bytes are not a valid serialized xorb.
    #[error(
        "malformed xorb: expected a sequence of chunks as in \
         https://huggingface.co/docs/xet/xorb"
    )]
    MalformedXorb(#[source] xet_core_structures::CoreError),
    /// An uploaded xorb body is larger than the server accepts.
    #[error(
        "xorb body is {len} bytes, over the limit of {limit}; xorbs hold at most 64 MiB of \
         chunks (https://huggingface.co/docs/xet/xorb#xorb-format)"
    )]
    XorbTooLarge {
        /// The body length.
        len: u64,
        /// The limit applied, [`MAX_XORB_BODY_BYTES`].
        limit: u64,
    },
    /// An uploaded xorb has no chunks.
    #[error(
        "xorb has no chunks; a xorb holds at least one \
         (https://huggingface.co/docs/xet/xorb#xorb-format)"
    )]
    XorbEmpty,
    /// An uploaded xorb has more chunks than the server accepts.
    #[error("xorb has more than {limit} chunks; split it into smaller xorbs")]
    XorbTooManyChunks {
        /// The limit applied, [`MAX_XORB_CHUNKS`].
        limit: usize,
    },
    /// A chunk decompresses to more bytes than its header declares or than
    /// the largest chunk allowed. Decompression stops at the limit.
    #[error(
        "malformed xorb: chunk {chunk} decompresses to more than {limit} bytes, its declared \
         length (chunks are at most 128 KiB; \
         https://huggingface.co/docs/xet/xorb#chunk-header-structure)"
    )]
    XorbChunkTooLong {
        /// The chunk's index in the xorb.
        chunk: usize,
        /// The bytes the chunk may decompress to.
        limit: u64,
    },
    /// A chunk decompresses to fewer bytes than its header declares.
    #[error(
        "malformed xorb: chunk {chunk} decompresses to {actual} bytes, but its header declares \
         {declared} (https://huggingface.co/docs/xet/xorb#chunk-header-structure)"
    )]
    XorbChunkTooShort {
        /// The chunk's index in the xorb.
        chunk: usize,
        /// The uncompressed length in the chunk header.
        declared: u32,
        /// The bytes the chunk decompressed to.
        actual: u64,
    },
    /// A chunk's compressed data runs past the end of the body.
    #[error(
        "malformed xorb: chunk {chunk} is truncated; its header declares more compressed bytes \
         than the body holds"
    )]
    XorbTruncated {
        /// The chunk's index in the xorb.
        chunk: usize,
    },
    /// Bytes follow the xorb footer.
    #[error(
        "malformed xorb: {len} bytes follow the footer at offset {offset}; the body must end \
         after its last chunk or its footer"
    )]
    XorbTrailingBytes {
        /// Where the extra bytes start.
        offset: u64,
        /// How many extra bytes follow the footer.
        len: u64,
    },
    /// Bytes are not a valid upload shard.
    #[error(
        "malformed shard: expected an upload shard as in \
         https://huggingface.co/docs/xet/shard#shard-upload"
    )]
    MalformedShard(#[source] xet_core_structures::CoreError),
}
