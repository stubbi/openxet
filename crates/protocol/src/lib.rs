//! The Xet protocol for openxet: hashes, xorbs and shards.
//!
//! This crate is openxet's only doorway to Hugging Face's `xet-core` crates
//! (ADR 0001). Every function is a thin wrapper over `xet-core-structures`,
//! so formats and hashing stay byte-exact with upstream; nothing here
//! reimplements them. Spec: <https://huggingface.co/docs/xet>.

#![forbid(unsafe_code)]

mod hash;
mod shard;
mod xorb;

pub use hash::{MerkleHash, chunk_hash, file_hash, hash_from_hex, hash_to_hex, range_hash};
pub use shard::{
    MAX_REFERENCED_CHUNKS, MAX_UPLOAD_SHARD_BYTES, Segment, ShardFileEntry, UploadShard,
    VerifyError, read_upload_shard, verify_segment, verify_shard,
};
pub use xorb::{StoredXorb, XorbChunks, read_xorb_chunks, xorb_hash, xorb_with_footer};

/// Errors from parsing protocol data.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// A string is not a hash in the spec's 64-character hex form.
    #[error(
        "invalid Xet hash: expected 64 hex characters as in \
         https://huggingface.co/docs/xet/hashing#converting-hashes-to-strings"
    )]
    InvalidHex(#[source] xet_core_structures::merklehash::DataHashError),
    /// Bytes are not a valid serialized xorb.
    #[error(
        "malformed xorb: expected a sequence of chunks as in \
         https://huggingface.co/docs/xet/xorb"
    )]
    MalformedXorb(#[source] xet_core_structures::CoreError),
    /// Bytes are not a valid upload shard.
    #[error(
        "malformed shard: expected an upload shard as in \
         https://huggingface.co/docs/xet/shard#shard-upload"
    )]
    MalformedShard(#[source] xet_core_structures::CoreError),
}
