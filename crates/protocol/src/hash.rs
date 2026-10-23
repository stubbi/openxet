//! Hashing methods from the spec: <https://huggingface.co/docs/xet/hashing>.

use xet_core_structures::merklehash;
use xet_core_structures::metadata_shard::chunk_verification::range_hash_from_chunks;

use crate::ProtocolError;

pub use xet_core_structures::merklehash::MerkleHash;

/// Renders a hash in the spec's string form ("Converting Hashes to Strings"),
/// as used in URLs, file names and the reference files.
#[must_use]
pub fn hash_to_hex(h: &MerkleHash) -> String {
    h.hex()
}

/// Parses a hash from the spec's string form; the inverse of [`hash_to_hex`].
///
/// # Errors
///
/// [`ProtocolError::InvalidHex`] unless `s` is exactly 64 hex characters.
pub fn hash_from_hex(s: &str) -> Result<MerkleHash, ProtocolError> {
    MerkleHash::from_hex(s).map_err(ProtocolError::InvalidHex)
}

/// The chunk hash of a chunk's uncompressed bytes.
#[must_use]
pub fn chunk_hash(data: &[u8]) -> MerkleHash {
    merklehash::compute_data_hash(data)
}

/// The file hash over a file's `(chunk hash, uncompressed length)` pairs, in
/// file order. Uses the empty salt, as the reference file hash requires.
#[must_use]
pub fn file_hash(chunks: &[(MerkleHash, u64)]) -> MerkleHash {
    merklehash::file_hash(chunks)
}

/// The range verification hash over consecutive chunk hashes of one xorb,
/// as stored in a shard's verification section.
#[must_use]
pub fn range_hash(chunk_hashes: &[MerkleHash]) -> MerkleHash {
    range_hash_from_chunks(chunk_hashes)
}
