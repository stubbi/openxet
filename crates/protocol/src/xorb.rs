//! Xorbs: <https://huggingface.co/docs/xet/xorb>.

use std::io::Read;

use xet_core_structures::merklehash;
use xet_core_structures::xorb_object::{deserialize_chunks, reconstruct_xorb_with_footer};

use crate::{MerkleHash, ProtocolError, chunk_hash};

/// The chunks of a xorb, in xorb order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XorbChunks {
    /// `(chunk hash, uncompressed length)` per chunk.
    pub hashes: Vec<(MerkleHash, u64)>,
}

/// Deserializes a footer-less xorb (the spec's chunk sequence) and hashes
/// every chunk.
///
/// Input must end exactly after the last chunk: a xorb footer is rejected
/// here. Uploads, which may carry a footer, go through [`xorb_with_footer`].
///
/// # Errors
///
/// [`ProtocolError::MalformedXorb`] if a chunk header or chunk body is invalid
/// or truncated, or bytes follow the last chunk.
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

/// A xorb in its stored form: the chunk sequence followed by a footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredXorb {
    /// The xorb hash computed from the chunks.
    pub hash: MerkleHash,
    /// The serialized chunks followed by a footer computed from them.
    pub bytes: Vec<u8>,
}

/// Validates an uploaded xorb body and builds its stored form, as upstream's
/// reference server does for `POST /v1/xorbs` (`xet-client` 1.7.0,
/// `cas_client/simulation/local_client.rs`, via
/// `reconstruct_xorb_with_footer`).
///
/// The body is a chunk sequence, optionally followed by a footer: chunk
/// parsing stops at the footer ident `XETBLOB`, the old footer is discarded
/// unread, and a new one is computed from the chunks. The caller compares
/// [`StoredXorb::hash`] with the hash in the request path.
///
/// # Errors
///
/// [`ProtocolError::MalformedXorb`] if a chunk is invalid or truncated, or
/// bytes after the last chunk neither form a chunk nor start with the footer
/// ident.
pub fn xorb_with_footer(body: &[u8]) -> Result<StoredXorb, ProtocolError> {
    let mut bytes = Vec::with_capacity(body.len());
    let (_, hash) =
        reconstruct_xorb_with_footer(&mut bytes, body).map_err(ProtocolError::MalformedXorb)?;
    Ok(StoredXorb { hash, bytes })
}

/// The xorb hash over a xorb's chunks.
#[must_use]
pub fn xorb_hash(chunks: &XorbChunks) -> MerkleHash {
    merklehash::xorb_hash(&chunks.hashes)
}
