//! Golden tests against Hugging Face's Xet reference files
//! (`datasets/xet-team/xet-spec-reference-files`, fetched and checksum-verified
//! by `scripts/fetch-reference-files.sh`).

// clippy.toml allows unwrap only inside `#[test]` functions; the fixture
// helpers here are test code too, and a panic is the intended failure.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use openxet_protocol::{
    MAX_REFERENCED_CHUNKS, MAX_REFERENCED_XORBS, MAX_UPLOAD_SHARD_BYTES, MerkleHash, ProtocolError,
    Segment, ShardFileEntry, UploadShard, VerifyError, VerifyLimits, XorbChunkLookup, file_hash,
    hash_from_hex, hash_to_hex, range_hash, read_upload_shard, read_xorb_chunks, verify_segment,
    verify_shard, xorb_hash, xorb_with_footer,
};
// Only to build client-serialized xorbs and shards as test input; production
// code reaches upstream through openxet_protocol alone.
use xet_core_structures::metadata_shard::shard_in_memory::MDBInMemoryShard;
use xet_core_structures::xorb_object::{Chunk, RawXorbData, SerializedXorbObject};

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
/// footer-less form clients upload nor a full footer. Upstream rejects such
/// trailing bytes on both the strict and the footer-tolerant path (tested
/// below), so the golden hashes are checked over the chunk section returned
/// here.
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
    assert!(hash_from_hex(&"g".repeat(64)).is_err());
    let err = hash_from_hex("not a hash").unwrap_err();
    // The spec's string form lives on its api page.
    assert!(
        err.to_string()
            .contains("https://huggingface.co/docs/xet/api#converting-hashes-to-strings"),
        "{err}"
    );
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

/// The reference CSV serialized by upstream's client serializer
/// (`SerializedXorbObject::from_xorb`, the call `xet-data` makes on upload),
/// cut at the chunk boundaries of the `.chunks` listing.
fn client_serialized_xorb(serialize_footer: bool) -> SerializedXorbObject {
    let csv = bytes::Bytes::from(fs::read(fixture(CSV)).unwrap());
    let mut offset = 0;
    let chunks: Vec<Chunk> = chunks_listing(&format!("{CSV}.chunks"))
        .into_iter()
        .map(|(_, len)| {
            let len = usize::try_from(len).unwrap();
            let chunk = Chunk::new(csv.slice(offset..offset + len));
            offset += len;
            chunk
        })
        .collect();
    assert_eq!(offset, csv.len());
    let raw = RawXorbData::from_chunks(&chunks, vec![0]);
    SerializedXorbObject::from_xorb(raw, serialize_footer, "auto", 0).unwrap()
}

#[test]
fn xorb_upload_without_footer_gets_footer_and_reference_hash() {
    let stored = xorb_with_footer(&reference_xorb_bytes()).unwrap();
    assert_eq!(hash_to_hex(&stored.hash), XORB);
    assert_eq!(
        stored.chunks.hashes,
        chunks_listing(&format!("{CSV}.chunks"))
    );
    // The stored form is the chunk section followed by a footer, which the
    // footer-tolerant path accepts again unchanged.
    assert!(stored.bytes.starts_with(&reference_xorb_bytes()));
    let again = xorb_with_footer(&stored.bytes).unwrap();
    assert_eq!(again.hash, stored.hash);
    assert_eq!(again.bytes, stored.bytes);
}

#[test]
fn xorb_upload_with_and_without_client_footer_yield_reference_hash() {
    let without = client_serialized_xorb(false);
    let with = client_serialized_xorb(true);
    assert!(without.footer_start.is_none());
    assert!(with.footer_start.is_some());
    for body in [&without.serialized_data, &with.serialized_data] {
        let stored = xorb_with_footer(body).unwrap();
        assert_eq!(hash_to_hex(&stored.hash), XORB);
        assert_eq!(stored.chunks.hashes, reference_xorb_chunks());
    }
}

#[test]
fn xorb_upload_with_stray_trailing_bytes_is_rejected() {
    let file = fs::read(fixture(&format!("{XORB}.xorb"))).unwrap();
    assert!(matches!(
        xorb_with_footer(&file),
        Err(ProtocolError::MalformedXorb(_))
    ));
}

#[test]
fn read_xorb_chunks_rejects_a_footer() {
    let with = client_serialized_xorb(true);
    assert!(matches!(
        read_xorb_chunks(with.serialized_data.as_slice()),
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

const UPLOAD_SHARD: &str =
    "Electric_Vehicle_Population_Data_20250917.csv.shard.verification-no-footer";
/// SHA-256 of the reference CSV (`shasum -a 256`), as hex of the digest bytes.
const CSV_SHA256: &str = "f41255b252f776125f2b657136654e4d4d5d2ccf8ef4db0ec186bd1981b69734";

fn reference_upload_shard() -> UploadShard {
    read_upload_shard(fs::File::open(fixture(UPLOAD_SHARD)).unwrap()).unwrap()
}

fn reference_segments() -> Vec<Segment> {
    reference_upload_shard().files.remove(0).segments
}

fn digest_bytes(hex: &str) -> [u8; 32] {
    core::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
}

fn flip_first_byte(h: MerkleHash) -> MerkleHash {
    let mut bytes: [u8; 32] = h.as_bytes().try_into().unwrap();
    bytes[0] ^= 1;
    MerkleHash::from(bytes)
}

#[test]
fn verification_shard_without_footer_parses_and_names_the_reference_file() {
    let shard = reference_upload_shard();
    assert_eq!(shard.files.len(), 1);
    let file = &shard.files[0];
    assert_eq!(
        hash_to_hex(&file.file_hash),
        read_trimmed(&format!("{CSV}.xet-file-hash"))
    );
    assert!(!file.segments.is_empty());
    for seg in &file.segments {
        assert_eq!(hash_to_hex(&seg.xorb), XORB);
        assert!(seg.verification.is_some());
    }
    assert_eq!(file.sha256, Some(digest_bytes(CSV_SHA256)));
    assert_eq!(
        shard.xorbs.iter().map(hash_to_hex).collect::<Vec<_>>(),
        [XORB]
    );
}

#[test]
fn every_reference_segment_verifies_against_the_reference_xorb() {
    let chunks = reference_xorb_chunks();
    for seg in reference_segments() {
        verify_segment(&seg, &chunks).unwrap();
    }
}

#[test]
fn tampered_verification_hash_is_rejected() {
    let chunks = reference_xorb_chunks();
    let mut seg = reference_segments().remove(0);
    seg.verification = seg.verification.map(flip_first_byte);
    let err = verify_segment(&seg, &chunks).unwrap_err();
    assert!(
        matches!(err, VerifyError::RangeHashMismatch { .. }),
        "{err}"
    );
    let msg = err.to_string();
    assert!(msg.contains(XORB) && msg.contains("0..796"), "{msg}");
}

#[test]
fn tampered_unpacked_bytes_is_rejected() {
    let chunks = reference_xorb_chunks();
    let mut seg = reference_segments().remove(0);
    seg.unpacked_bytes += 1;
    let err = verify_segment(&seg, &chunks).unwrap_err();
    assert!(matches!(err, VerifyError::LengthMismatch { .. }), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains("63527245") && msg.contains("63527244"),
        "{msg}"
    );
}

#[test]
fn segment_out_of_xorb_bounds_is_rejected() {
    let chunks = reference_xorb_chunks();
    let mut past_end = reference_segments().remove(0);
    past_end.chunk_end = u32::try_from(chunks.len()).unwrap() + 1;
    let mut empty = reference_segments().remove(0);
    empty.chunk_end = empty.chunk_start;
    for seg in [past_end, empty] {
        let err = verify_segment(&seg, &chunks).unwrap_err();
        assert!(matches!(err, VerifyError::OutOfBounds { .. }), "{err}");
    }
}

#[test]
fn file_hash_recomputes_from_segments() {
    let chunks = reference_xorb_chunks();
    let entry = reference_upload_shard().files.remove(0);
    let referenced: Vec<(MerkleHash, u64)> = entry
        .segments
        .iter()
        .flat_map(|s| chunks[s.chunk_start as usize..s.chunk_end as usize].to_vec())
        .collect();
    assert_eq!(file_hash(&referenced), entry.file_hash);

    let lookup = reference_lookup(&chunks);
    verify_one(&entry, &lookup).unwrap();

    let mut wrong = entry.clone();
    wrong.file_hash = flip_first_byte(wrong.file_hash);
    let err = verify_one(&wrong, &lookup).unwrap_err();
    assert!(matches!(err, VerifyError::FileHashMismatch { .. }), "{err}");
}

#[test]
fn file_referencing_an_unknown_xorb_is_rejected() {
    let entry = reference_upload_shard().files.remove(0);
    let err = verify_one(&entry, &no_xorbs).unwrap_err();
    assert!(matches!(err, VerifyError::UnknownXorb { .. }), "{err}");
    assert!(err.to_string().contains(XORB), "{err}");
}

#[test]
fn shard_without_verification_entries_is_rejected_for_upload() {
    let shard =
        read_upload_shard(fs::File::open(fixture(&format!("{CSV}.shard"))).unwrap()).unwrap();
    let chunks = reference_xorb_chunks();
    let seg = &shard.files[0].segments[0];
    assert!(seg.verification.is_none());
    assert!(matches!(
        verify_segment(seg, &chunks),
        Err(VerifyError::MissingVerification { .. })
    ));
}

#[test]
fn footered_shard_parses_like_the_footerless_one() {
    // The spec says uploads omit the footer
    // (https://huggingface.co/docs/xet/shard#shard-upload), but upstream's
    // in-process test backend accepts one, so a footer that matches the
    // sections is tolerated and discarded.
    let footered =
        read_upload_shard(fs::File::open(fixture(&format!("{CSV}.shard.verification"))).unwrap())
            .unwrap();
    assert_eq!(footered, reference_upload_shard());
}

#[test]
fn shard_serialized_with_lookup_tables_and_footer_parses() {
    // The wasm build of `xet-data` 1.7.0 uploads `MDBInMemoryShard::to_bytes`
    // (`processing/shard_interface/wasm.rs`): sections, lookup tables, footer.
    let bytes = fs::read(fixture(UPLOAD_SHARD)).unwrap();
    let in_memory = MDBInMemoryShard::from_reader(&mut bytes.as_slice()).unwrap();
    let wasm_form = in_memory.to_bytes().unwrap();
    assert!(wasm_form.len() > bytes.len() + 200);
    assert_eq!(
        read_upload_shard(wasm_form.as_slice()).unwrap(),
        reference_upload_shard()
    );
}

#[test]
fn footerless_shard_with_trailing_bytes_is_rejected() {
    let mut bytes = fs::read(fixture(UPLOAD_SHARD)).unwrap();
    bytes.push(0);
    assert!(matches!(
        read_upload_shard(bytes.as_slice()),
        Err(ProtocolError::MalformedShard(_))
    ));
}

#[test]
fn shard_with_inconsistent_footer_is_rejected() {
    let footered = fs::read(fixture(&format!("{CSV}.shard.verification"))).unwrap();
    // Footer cut short, and an extra byte between the sections and the footer
    // so its offsets no longer match.
    let truncated = &footered[..footered.len() - 1];
    let mut shifted = footered[..footered.len() - 200].to_vec();
    shifted.push(0);
    shifted.extend_from_slice(&footered[footered.len() - 200..]);
    for bytes in [truncated, shifted.as_slice()] {
        assert!(matches!(
            read_upload_shard(bytes),
            Err(ProtocolError::MalformedShard(_))
        ));
    }
}

#[test]
fn truncated_shard_is_rejected() {
    let bytes = fs::read(fixture(UPLOAD_SHARD)).unwrap();
    for len in [0, 47, 48 + 100, bytes.len() - 1] {
        assert!(
            matches!(
                read_upload_shard(&bytes[..len]),
                Err(ProtocolError::MalformedShard(_))
            ),
            "length {len}"
        );
    }
}

#[test]
fn wrong_header_version_is_rejected() {
    let mut bytes = fs::read(fixture(UPLOAD_SHARD)).unwrap();
    bytes[32] = 3;
    assert!(matches!(
        read_upload_shard(bytes.as_slice()),
        Err(ProtocolError::MalformedShard(_))
    ));
}

#[test]
fn oversized_section_claim_is_rejected_before_allocation() {
    // 1,000,000 segments with verification and metadata entries claim
    // (2 * 1_000_000 + 1) * 48 bytes, about 96 MB: no arithmetic overflow,
    // just over the 64 MiB budget, which must refuse it before allocating.
    let mut bytes = fs::read(fixture(UPLOAD_SHARD)).unwrap();
    bytes[48 + 36..48 + 40].copy_from_slice(&1_000_000u32.to_le_bytes());
    let err = read_upload_shard(bytes.as_slice()).unwrap_err();
    assert!(matches!(err, ProtocolError::MalformedShard(_)), "{err}");
    let source = std::error::Error::source(&err).unwrap().to_string();
    assert!(source.contains("exceeds limit"), "{source}");
    assert_eq!(MAX_UPLOAD_SHARD_BYTES, 64 * 1024 * 1024);
}

/// The reference xorb's stored chunk table, behind the lookup trait.
fn reference_lookup(chunks: &[(MerkleHash, u64)]) -> impl XorbChunkLookup + use<> {
    let xorb = hash_from_hex(XORB).unwrap();
    let chunks: Arc<[(MerkleHash, u64)]> = chunks.into();
    move |x: &MerkleHash| (*x == xorb).then(|| Arc::clone(&chunks))
}

/// A lookup that knows no xorbs.
fn no_xorbs(_: &MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>> {
    None
}

fn limits(max_referenced_chunks: u64, max_referenced_xorbs: usize) -> VerifyLimits {
    VerifyLimits {
        max_referenced_chunks,
        max_referenced_xorbs,
    }
}

#[test]
fn shard_work_at_the_cap_passes_and_over_it_is_rejected() {
    // The reference shard references all 796 chunks once.
    let chunks = reference_xorb_chunks();
    let shard = reference_upload_shard();
    let lookup = reference_lookup(&chunks);
    verify_shard(&shard, &lookup, limits(796, 1)).unwrap();
    let err = verify_shard(&shard, &lookup, limits(795, 1)).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::TooMuchWork {
                referenced_chunks: 796,
                limit: 795
            }
        ),
        "{err}"
    );
    assert_eq!(MAX_REFERENCED_CHUNKS, 1 << 24);
}

#[test]
fn shard_work_count_cannot_overflow() {
    // About as many maximal segments as fit in 64 MiB, each spanning u32::MAX
    // chunks: 1_400_000 * (2^32 - 1) chunks. The count must be exact (no u32
    // wrap, no panic), and the cap rejects it before any xorb is looked up.
    let seg = Segment {
        xorb: MerkleHash::default(),
        chunk_start: 0,
        chunk_end: u32::MAX,
        unpacked_bytes: 0,
        verification: None,
    };
    let entry = ShardFileEntry {
        file_hash: MerkleHash::default(),
        segments: vec![seg; 1_400_000],
        sha256: None,
    };
    let shard = UploadShard {
        files: vec![entry],
        xorbs: Vec::new(),
    };
    let unreachable = |_: &MerkleHash| -> Option<Arc<[(MerkleHash, u64)]>> { unreachable!() };
    let err = verify_shard(&shard, &unreachable, VerifyLimits::default()).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::TooMuchWork {
                referenced_chunks: 6_012_954_213_000_000,
                limit: MAX_REFERENCED_CHUNKS,
            }
        ),
        "{err}"
    );
}

#[test]
fn file_hash_streams_across_many_segments() {
    // The same 796 chunks split into segments of varying size, each with a
    // correct verification entry, still recompute to the reference file hash.
    let chunks = reference_xorb_chunks();
    let xorb = hash_from_hex(XORB).unwrap();
    let mut segments = Vec::new();
    let mut start = 0;
    for size in (1..).cycle().take(796) {
        if start == chunks.len() {
            break;
        }
        let end = (start + size).min(chunks.len());
        let range = &chunks[start..end];
        segments.push(Segment {
            xorb,
            chunk_start: u32::try_from(start).unwrap(),
            chunk_end: u32::try_from(end).unwrap(),
            unpacked_bytes: u32::try_from(range.iter().map(|(_, l)| l).sum::<u64>()).unwrap(),
            verification: Some(range_hash(
                &range.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            )),
        });
        start = end;
    }
    assert_eq!(segments.len(), 40);
    let entry = ShardFileEntry {
        file_hash: hash_from_hex(&read_trimmed(&format!("{CSV}.xet-file-hash"))).unwrap(),
        segments,
        sha256: None,
    };
    verify_one(&entry, &reference_lookup(&chunks)).unwrap();
}

#[test]
fn reference_dedupe_shard_has_no_files() {
    // A global-dedup response is a shard too; it parses, lists its xorb, and
    // has nothing to register.
    let shard = read_upload_shard(fs::File::open(fixture(&format!("{CSV}.shard.dedupe"))).unwrap())
        .unwrap();
    assert!(shard.files.is_empty());
    assert_eq!(
        shard.xorbs.iter().map(hash_to_hex).collect::<Vec<_>>(),
        [XORB]
    );
}

#[test]
fn distinct_xorb_count_at_the_cap_passes_and_over_it_is_rejected() {
    // The reference file twice: two segments, one distinct xorb.
    let chunks = reference_xorb_chunks();
    let mut shard = reference_upload_shard();
    shard.files.push(shard.files[0].clone());
    let lookup = reference_lookup(&chunks);
    verify_shard(&shard, &lookup, limits(MAX_REFERENCED_CHUNKS, 1)).unwrap();
    let err = verify_shard(&shard, &lookup, limits(MAX_REFERENCED_CHUNKS, 0)).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::TooManyXorbs {
                referenced_xorbs: 1,
                limit: 0
            }
        ),
        "{err}"
    );
}

#[test]
fn distinct_xorb_cap_is_checked_before_any_lookup() {
    // Two distinct xorbs, one referenced twice: two count against the cap.
    let mut entry = reference_upload_shard().files.remove(0);
    let seg = entry.segments[0];
    let other = Segment {
        xorb: flip_first_byte(seg.xorb),
        ..seg
    };
    entry.segments = vec![seg, other, other];
    let shard = UploadShard {
        files: vec![entry],
        xorbs: Vec::new(),
    };
    assert_eq!(shard.referenced_xorbs().count(), 2);
    let unreachable = |_: &MerkleHash| -> Option<Arc<[(MerkleHash, u64)]>> { unreachable!() };
    let err = verify_shard(&shard, &unreachable, limits(MAX_REFERENCED_CHUNKS, 1)).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::TooManyXorbs {
                referenced_xorbs: 2,
                limit: 1
            }
        ),
        "{err}"
    );
    assert_eq!(
        VerifyLimits::default(),
        limits(MAX_REFERENCED_CHUNKS, MAX_REFERENCED_XORBS)
    );
}

#[test]
fn referenced_xorbs_are_distinct_in_first_reference_order() {
    let mut entry = reference_upload_shard().files.remove(0);
    let seg = entry.segments[0];
    let (first, other) = (seg.xorb, flip_first_byte(seg.xorb));
    entry.segments = vec![seg, Segment { xorb: other, ..seg }];
    let shard = UploadShard {
        files: vec![entry.clone(), entry],
        xorbs: Vec::new(),
    };
    assert_eq!(
        shard.referenced_xorbs().copied().collect::<Vec<_>>(),
        [first, other]
    );
}

#[test]
fn xorbs_listed_but_not_referenced_are_ignored() {
    // The CAS info section may list xorbs no segment uses; verification
    // neither looks them up nor requires them.
    let chunks = reference_xorb_chunks();
    let mut shard = reference_upload_shard();
    shard.xorbs.push(flip_first_byte(shard.xorbs[0]));
    assert_eq!(shard.referenced_xorbs().count(), 1);
    verify_shard(&shard, &reference_lookup(&chunks), VerifyLimits::default()).unwrap();
}

#[test]
fn a_map_of_shared_chunk_tables_is_a_lookup() {
    // M1 loads every referenced xorb's table up front, then verifies.
    let shard = reference_upload_shard();
    let table: Arc<[(MerkleHash, u64)]> = reference_xorb_chunks().into();
    let loaded: HashMap<MerkleHash, Arc<[(MerkleHash, u64)]>> = shard
        .referenced_xorbs()
        .map(|x| (*x, Arc::clone(&table)))
        .collect();
    verify_shard(&shard, &loaded, VerifyLimits::default()).unwrap();
    assert!(loaded.chunks(&flip_first_byte(shard.xorbs[0])).is_none());
    // Verification borrows the tables; it keeps no reference of its own.
    assert_eq!(Arc::strong_count(&table), 2);
}

/// Verifies one file through the public entry point, under the default limits.
fn verify_one(entry: &ShardFileEntry, lookup: &impl XorbChunkLookup) -> Result<(), VerifyError> {
    let shard = UploadShard {
        files: vec![entry.clone()],
        xorbs: Vec::new(),
    };
    verify_shard(&shard, lookup, VerifyLimits::default())
}
