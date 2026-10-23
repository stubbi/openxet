//! Shard checks against the exact bytes stock `hf_xet` 1.7.0 uploads.
//!
//! `testdata/hf-xet-1.7.0-upload/` holds the xorb and shard bodies captured
//! from a real `hf_xet.upload_files` run by `scripts/capture-hf-xet-upload.py`
//! (three seeded files: `a.bin`, `b.bin` and an empty file). The expected
//! values below are what that run printed.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use openxet_protocol::{
    MerkleHash, UploadShard, XorbChunks, hash_to_hex, read_upload_shard, read_xorb_chunks,
    verify_file, verify_segment, xorb_hash,
};

const XORB: &str = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";
/// `(file hash, SHA-256)` per uploaded file, as `hf_xet` reported them.
const FILES: [(&str, &str); 3] = [
    // empty.bin: hf_xet reports, and the shard carries, all-zero values.
    (
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ),
    // a.bin
    (
        "65fef04da9e2708da8ca1acc831962d0686875eff24218d24e108362eeb420df",
        "7d4ca26bb83e603ddef686c5256eb39c7c2180008da16337a38cc21cd017127b",
    ),
    // b.bin
    (
        "79359c577af7d8f7e7796f289dac3525f2f7ce75d6408a4ed2ff8ec56b316dc0",
        "5cfbf99f098dd461881f858aee23696a61ad036a17ab241f9faa2e85313e237b",
    ),
];

fn capture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/hf-xet-1.7.0-upload")
        .join(name)
}

fn shard() -> UploadShard {
    read_upload_shard(fs::File::open(capture("upload.shard")).unwrap()).unwrap()
}

/// `hf_xet` uploads xorbs without a footer, so the strict chunk parser reads
/// the captured body as is.
fn xorb_chunks() -> XorbChunks {
    read_xorb_chunks(fs::File::open(capture(&format!("{XORB}.xorb"))).unwrap()).unwrap()
}

fn digest_bytes(hex: &str) -> [u8; 32] {
    core::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
}

#[test]
fn hf_xet_upload_shard_names_its_files_and_xorb() {
    let shard = shard();
    let names: Vec<(String, Option<[u8; 32]>)> = shard
        .files
        .iter()
        .map(|f| (hash_to_hex(&f.file_hash), f.sha256))
        .collect();
    let expected: Vec<(String, Option<[u8; 32]>)> = FILES
        .iter()
        .map(|(h, s)| ((*h).to_owned(), Some(digest_bytes(s))))
        .collect();
    assert_eq!(names, expected);
    assert_eq!(
        shard.xorbs.iter().map(hash_to_hex).collect::<Vec<_>>(),
        [XORB]
    );
    assert!(shard.files[0].segments.is_empty());
}

#[test]
fn hf_xet_upload_shard_verifies_against_its_xorb() {
    let xorb = xorb_chunks();
    assert_eq!(hash_to_hex(&xorb_hash(&xorb)), XORB);
    let chunks = xorb.hashes;
    let lookup = |x: &MerkleHash| (hash_to_hex(x) == XORB).then_some(chunks.as_slice());
    for file in shard().files {
        for seg in &file.segments {
            verify_segment(seg, &chunks).unwrap();
        }
        verify_file(&file, lookup).unwrap();
    }
}
