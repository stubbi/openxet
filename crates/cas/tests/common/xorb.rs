//! Xorb bodies for upload tests: the `hf_xet` 1.7.0 capture, and small
//! synthetic xorbs of uncompressed chunks.
//!
//! Chunks are framed by hand per the spec's chunk header
//! (<https://huggingface.co/docs/xet/xorb#chunk-header-structure>): version
//! `0`, compressed length (3 bytes LE), compression scheme, uncompressed
//! length (3 bytes LE). Scheme `0` stores the bytes as they are, so a test
//! can make a header disagree with its data. Hashes come from
//! `openxet_protocol`, which validates them against the reference files.

use std::path::PathBuf;

use openxet_protocol::{MerkleHash, StoredXorb, chunk_hash, hash_to_hex, xorb_with_footer};

/// The xorb hash of the `hf_xet` 1.7.0 capture.
pub const HF_XET_XORB: &str = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";

/// The compression scheme byte for chunks stored uncompressed.
pub const SCHEME_NONE: u8 = 0;

/// The footer-less xorb body stock `hf_xet` 1.7.0 uploaded, captured by
/// `scripts/capture-hf-xet-upload.py`.
pub fn hf_xet_xorb() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/hf-xet-1.7.0-upload")
        .join(format!("{HF_XET_XORB}.xorb"));
    std::fs::read(path).unwrap()
}

/// A chunk with `scheme`, whose header declares `declared` uncompressed
/// bytes, followed by `stored` as its data.
pub fn raw_chunk(scheme: u8, stored: &[u8], declared: u32) -> Vec<u8> {
    let stored_len = u32::try_from(stored.len()).unwrap();
    let mut out = vec![0];
    out.extend_from_slice(&stored_len.to_le_bytes()[..3]);
    out.push(scheme);
    out.extend_from_slice(&declared.to_le_bytes()[..3]);
    out.extend_from_slice(stored);
    out
}

/// A valid uncompressed chunk holding `data`.
pub fn chunk(data: &[u8]) -> Vec<u8> {
    raw_chunk(SCHEME_NONE, data, u32::try_from(data.len()).unwrap())
}

/// A valid footer-less xorb body of one uncompressed chunk per entry.
pub fn xorb_of(chunks: &[&[u8]]) -> Vec<u8> {
    chunks.iter().flat_map(|data| chunk(data)).collect()
}

/// What the server stores for `body`, which must be valid.
pub fn stored(body: &[u8]) -> StoredXorb {
    xorb_with_footer(body).unwrap()
}

/// `POST` and `HEAD` path of the xorb `hash`.
pub fn xorb_path(hash: &MerkleHash) -> String {
    format!("/v1/xorbs/default/{}", hash_to_hex(hash))
}

/// The first 8-byte counter value whose chunk hash passes
/// `is_dedup_eligible` exactly when `eligible`. About 1 in 1024 hashes pass,
/// so a million candidates always hold both kinds.
pub fn chunk_data(eligible: bool) -> Vec<u8> {
    (0..1_000_000u64)
        .map(|n| n.to_le_bytes().to_vec())
        .find(|data| openxet_protocol::is_dedup_eligible(&chunk_hash(data)) == eligible)
        .unwrap()
}
