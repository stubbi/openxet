//! Xorbs: <https://huggingface.co/docs/xet/xorb>.

use std::io::Read;

use xet_core_structures::merklehash;
use xet_core_structures::xorb_object::deserialize_chunks;

use crate::{MerkleHash, ProtocolError, chunk_hash};

/// The chunks of a xorb, in xorb order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XorbChunks {
    /// `(chunk hash, uncompressed length)` per chunk.
    pub hashes: Vec<(MerkleHash, u64)>,
}

/// Deserializes a xorb and hashes every chunk.
///
/// # Errors
///
/// [`ProtocolError::MalformedXorb`] if a chunk header or chunk body is invalid
/// or truncated.
pub fn read_xorb_chunks(mut r: impl Read) -> Result<XorbChunks, ProtocolError> {
    let (data, chunk_byte_indices) =
        deserialize_chunks(&mut r).map_err(ProtocolError::MalformedXorb)?;
    // `chunk_byte_indices` holds every chunk's start offset in `data` plus the
    // total length, starting with a leading 0, so each window is one chunk.
    let hashes = chunk_byte_indices
        .windows(2)
        .map(|w| {
            let chunk = &data[w[0] as usize..w[1] as usize];
            (chunk_hash(chunk), u64::from(w[1] - w[0]))
        })
        .collect();
    Ok(XorbChunks { hashes })
}

/// The xorb hash over a xorb's chunks.
#[must_use]
pub fn xorb_hash(chunks: &XorbChunks) -> MerkleHash {
    merklehash::xorb_hash(&chunks.hashes)
}
