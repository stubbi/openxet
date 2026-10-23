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
    MAX_REFERENCED_CHUNKS, MerkleHash, ShardFileEntry, UploadShard, VerifyError, XorbChunks,
    hash_to_hex, read_upload_shard, read_xorb_chunks, verify_segment, verify_shard, xorb_hash,
    xorb_with_footer,
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
fn hf_xet_upload_xorb_is_accepted_as_uploaded() {
    let body = fs::read(capture(&format!("{XORB}.xorb"))).unwrap();
    let stored = xorb_with_footer(&body).unwrap();
    assert_eq!(hash_to_hex(&stored.hash), XORB);
    assert_eq!(stored.chunks, xorb_chunks());
    assert!(stored.bytes.starts_with(&body));
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
        verify_one(&file, lookup).unwrap();
    }
}

/// SHA-256 of zero bytes.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn empty_file_sha256_must_be_absent_zero_or_the_empty_digest() {
    let empty = shard().files.remove(0);
    assert!(empty.segments.is_empty());
    // hf_xet 1.7.0 sends all zeros.
    assert_eq!(empty.sha256, Some([0; 32]));
    let no_xorbs = |_: &MerkleHash| None;
    for sha256 in [None, Some([0; 32]), Some(digest_bytes(EMPTY_SHA256))] {
        let entry = ShardFileEntry {
            sha256,
            ..empty.clone()
        };
        verify_one(&entry, no_xorbs).unwrap();
    }
    let wrong = ShardFileEntry {
        sha256: Some(digest_bytes(FILES[1].1)),
        ..empty
    };
    let err = verify_one(&wrong, no_xorbs).unwrap_err();
    assert!(matches!(err, VerifyError::EmptyFileSha256 { .. }), "{err}");
}

/// Verifies one file through the public entry point, under the default cap.
fn verify_one<'a>(
    entry: &ShardFileEntry,
    xorb_chunks: impl Fn(&MerkleHash) -> Option<&'a [(MerkleHash, u64)]>,
) -> Result<(), VerifyError> {
    let shard = UploadShard {
        files: vec![entry.clone()],
        xorbs: Vec::new(),
    };
    verify_shard(&shard, xorb_chunks, MAX_REFERENCED_CHUNKS)
}
