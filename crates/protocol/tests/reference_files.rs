//! Golden tests against Hugging Face's Xet reference files
//! (`datasets/xet-team/xet-spec-reference-files`, fetched and checksum-verified
//! by `scripts/fetch-reference-files.sh`).

// clippy.toml allows unwrap only inside `#[test]` functions; the fixture
// helpers here are test code too, and a panic is the intended failure.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use openxet_protocol::{
    MerkleHash, ProtocolError, file_hash, hash_from_hex, hash_to_hex, range_hash, read_xorb_chunks,
    xorb_hash,
};

const CSV: &str = "Electric_Vehicle_Population_Data_20250917.csv";
const XORB: &str = "eea25d6ee393ccae385820daed127b96ef0ea034dfb7cf6da3a950ce334b7632";
const SINGLE_CHUNKS: [&str; 3] = [
    "b10aa1dc71c61661de92280c41a188aabc47981739b785724a099945d8dc5ce4",
    "26255591fa803b6baf25d88c315b8a6f5153d5bcfdf18ec5ef526264e0ccc907",
    "099cb228194fe640e36a6c7d274ee5ed3a714ccd557a0951d9b6b43a7292b5d1",
];

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/xet-spec-reference")
        .join(name);
    assert!(
        path.is_file(),
        "missing reference file {name}: run scripts/fetch-reference-files.sh"
    );
    path
}

fn read_trimmed(name: &str) -> String {
    fs::read_to_string(fixture(name)).unwrap().trim().to_owned()
}

/// Parses a `.chunks` listing. Each line is `<chunk hash> <length>`: the
/// chunk hash in the spec's string form
/// (<https://huggingface.co/docs/xet/api#converting-hashes-to-strings>), one
/// ASCII space, and the chunk's uncompressed length in decimal bytes. Lines
/// are newline-terminated and in chunk order; the dataset README describes
/// the files.
fn chunks_listing(name: &str) -> Vec<(MerkleHash, u64)> {
    fs::read_to_string(fixture(name))
        .unwrap()
        .lines()
        .map(|line| {
            let (hash, len) = line.split_once(' ').unwrap();
            (hash_from_hex(hash).unwrap(), len.parse().unwrap())
        })
        .collect()
}

/// The reference xorb is its 796 serialized chunks followed by the 4 bytes
/// `XETB` (the start of a footer ident, `XETBLOB`), which is neither the
/// footer-less form clients upload nor a full footer. Upstream's
/// `deserialize_chunks` rejects such trailing bytes (tested below), so the
/// golden hashes are checked over the chunk section returned here.
fn reference_xorb_bytes() -> Vec<u8> {
    let mut bytes = fs::read(fixture(&format!("{XORB}.xorb"))).unwrap();
    let trailer = bytes.split_off(bytes.len() - 4);
    assert_eq!(trailer, b"XETB");
    bytes
}

fn reference_xorb_chunks() -> Vec<(MerkleHash, u64)> {
    read_xorb_chunks(reference_xorb_bytes().as_slice())
        .unwrap()
        .hashes
}

#[test]
fn hex_round_trip_matches_spec_example() {
    // https://huggingface.co/docs/xet/api#converting-hashes-to-strings:
    // bytes 0..32 render as this string. The spec's printed string drops the
    // `10` of the third group (62 characters); its own reordered byte list
    // (23, 22, ..., 16) gives the 64 characters below.
    let bytes: [u8; 32] = core::array::from_fn(|i| u8::try_from(i).unwrap());
    let h = MerkleHash::from(bytes);
    assert_eq!(
        hash_to_hex(&h),
        "07060504030201000f0e0d0c0b0a090817161514131211101f1e1d1c1b1a1918"
    );
    assert_eq!(hash_from_hex(&hash_to_hex(&h)).unwrap(), h);
}

#[test]
fn invalid_hex_is_rejected() {
    assert!(hash_from_hex("not a hash").is_err());
    assert!(hash_from_hex(&"g".repeat(64)).is_err());
}

#[test]
fn reference_xorb_hash_matches_its_name() {
    let chunks = read_xorb_chunks(reference_xorb_bytes().as_slice()).unwrap();
    assert_eq!(hash_to_hex(&xorb_hash(&chunks)), XORB);
    assert_eq!(read_trimmed(&format!("{CSV}.xet-xorb-hash")), XORB);
}

#[test]
fn reference_xorb_with_trailing_bytes_is_rejected() {
    let file = fs::File::open(fixture(&format!("{XORB}.xorb"))).unwrap();
    assert!(matches!(
        read_xorb_chunks(file),
        Err(ProtocolError::MalformedXorb(_))
    ));
}

#[test]
fn reference_xorb_chunks_match_chunks_listing() {
    let listing = chunks_listing(&format!("{XORB}.xorb.chunks"));
    assert_eq!(listing.len(), 796);
    assert_eq!(reference_xorb_chunks(), listing);
    assert_eq!(chunks_listing(&format!("{CSV}.chunks")), listing);
}

#[test]
fn reference_file_hash_matches() {
    let expected = read_trimmed(&format!("{CSV}.xet-file-hash"));
    assert_eq!(
        expected,
        "118a53328412787fee04011dcf82fdc4acf3a4a1eddec341c910d30a306aaf97"
    );
    assert_eq!(hash_to_hex(&file_hash(&reference_xorb_chunks())), expected);
}

#[test]
fn reference_range_hash_matches() {
    let hashes: Vec<MerkleHash> = reference_xorb_chunks()
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    assert_eq!(
        hash_to_hex(&range_hash(&hashes)),
        read_trimmed(&format!("{XORB}.xorb.range-hash"))
    );
}

#[test]
fn single_chunk_files_hash_to_their_names() {
    for name in SINGLE_CHUNKS {
        let data = fs::read(fixture(&format!("{name}.chunk"))).unwrap();
        assert_eq!(
            hash_to_hex(&openxet_protocol::chunk_hash(&data)),
            name,
            "{name}.chunk"
        );
    }
}

#[test]
fn missing_fixture_fails_loudly() {
    let err = std::panic::catch_unwind(|| fixture("does-not-exist")).unwrap_err();
    assert_eq!(
        err.downcast_ref::<String>().map(String::as_str),
        Some("missing reference file does-not-exist: run scripts/fetch-reference-files.sh")
    );
}
