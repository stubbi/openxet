//! The formats the CAS server writes: chunk offsets into stored xorbs, the
//! stored form of an upload shard, and global-dedup responses.
//!
//! Fixtures are the spec reference xorb (`scripts/fetch-reference-files.sh`)
//! and the `hf_xet` 1.7.0 upload capture (`testdata/hf-xet-1.7.0-upload/`).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use openxet_protocol::{
    MerkleHash, ProtocolError, Segment, ShardFileEntry, StoredXorb, UploadShard, VerifyError,
    VerifyLimits, dedup_shard, hash_to_hex, is_dedup_eligible, read_upload_shard, read_xorb_chunks,
    shard_hash, stored_shard, verify_shard, xorb_with_footer,
};
// Only to read the produced shards back with upstream's own parsers.
use xet_core_structures::metadata_shard::MDBShardInfo;
use xet_core_structures::metadata_shard::streaming_shard::MDBMinimalShard;
use xet_core_structures::xorb_object::XorbObjectInfoV1;

const REFERENCE_XORB: &str = "eea25d6ee393ccae385820daed127b96ef0ea034dfb7cf6da3a950ce334b7632";
const CAPTURE_XORB: &str = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";

fn testdata(path: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(path);
    assert!(
        path.is_file(),
        "missing fixture {}: run scripts/fetch-reference-files.sh",
        path.display()
    );
    path
}

/// The reference xorb's chunk section; the file ends in 4 stray bytes
/// (`XETB`), see `reference_files.rs`.
fn reference_xorb() -> StoredXorb {
    let mut bytes = fs::read(testdata(&format!(
        "xet-spec-reference/{REFERENCE_XORB}.xorb"
    )))
    .unwrap();
    bytes.truncate(bytes.len() - 4);
    xorb_with_footer(&bytes).unwrap()
}

fn capture_xorb() -> StoredXorb {
    let body = fs::read(testdata(&format!(
        "hf-xet-1.7.0-upload/{CAPTURE_XORB}.xorb"
    )))
    .unwrap();
    xorb_with_footer(&body).unwrap()
}

fn capture_shard_bytes() -> Vec<u8> {
    fs::read(testdata("hf-xet-1.7.0-upload/upload.shard")).unwrap()
}

fn capture_shard() -> UploadShard {
    read_upload_shard(capture_shard_bytes().as_slice()).unwrap()
}

fn capture_lookup() -> impl Fn(&MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>> {
    let xorb = capture_xorb();
    let chunks: Arc<[(MerkleHash, u64)]> = xorb.chunks.hashes.into();
    let hash = xorb.hash;
    move |x: &MerkleHash| (*x == hash).then(|| Arc::clone(&chunks))
}

#[test]
fn chunk_offsets_frame_every_chunk() {
    for xorb in [reference_xorb(), capture_xorb()] {
        let n = xorb.chunks.hashes.len();
        let off = &xorb.chunk_offsets;
        assert_eq!(off.len(), n + 1);
        assert_eq!(off[0], 0);
        for (a, b) in [(0, 1), (0, n), (n - 1, n)] {
            let range = &xorb.bytes[off[a] as usize..off[b] as usize];
            let chunks = read_xorb_chunks(range).unwrap();
            assert_eq!(chunks.hashes, xorb.chunks.hashes[a..b], "chunks {a}..{b}");
        }
    }
}

#[test]
fn chunk_offsets_of_a_footered_upload_end_where_the_footer_starts() {
    // Both fixtures are footer-less uploads; their stored form is a footered
    // body, as a client serializing with a footer uploads it.
    for footerless in [reference_xorb(), capture_xorb()] {
        let footered = xorb_with_footer(&footerless.bytes).unwrap();
        assert_eq!(footered.chunk_offsets, footerless.chunk_offsets);
        let n = footered.chunks.hashes.len();
        let end = footered.chunk_offsets[n] as usize;
        assert!(end < footered.bytes.len());
        assert_eq!(
            read_xorb_chunks(&footered.bytes[..end]).unwrap().hashes,
            footered.chunks.hashes
        );
        // Upstream's footer parser reads a footer starting exactly there.
        XorbObjectInfoV1::deserialize(&mut &footered.bytes[end..]).unwrap();
    }
}

#[test]
fn stored_shard_round_trips() {
    let shard = capture_shard();
    let lookup = capture_lookup();
    let stored = stored_shard(&shard, &lookup).unwrap();
    let back = read_upload_shard(stored.as_slice()).unwrap();

    // Files come back in file-hash order and xorbs in xorb-hash order, the
    // order upstream's serializer writes; the capture is already in it.
    let mut expected = shard.clone();
    expected.files.sort_by_key(|f| f.file_hash);
    expected.xorbs = shard.referenced_xorbs().copied().collect();
    expected.xorbs.sort();
    assert_eq!(back, expected);
    // The real SHA-256 digests, not their stored hash layout, survive.
    assert_eq!(
        back.files.iter().map(|f| f.sha256).collect::<Vec<_>>(),
        shard.files.iter().map(|f| f.sha256).collect::<Vec<_>>()
    );
    assert!(back.files.iter().all(|f| f.sha256.is_some()));
    verify_shard(&back, &lookup, VerifyLimits::default()).unwrap();
}

#[test]
fn stored_shard_drops_unreferenced_xorbs() {
    let mut shard = capture_shard();
    let unreferenced = MerkleHash::random_from_seed(7);
    shard.xorbs.push(unreferenced);
    let stored = stored_shard(&shard, &capture_lookup()).unwrap();
    let back = read_upload_shard(stored.as_slice()).unwrap();
    assert_eq!(
        back.xorbs.iter().map(hash_to_hex).collect::<Vec<_>>(),
        [CAPTURE_XORB]
    );
}

#[test]
fn stored_shard_is_the_form_stock_clients_upload() {
    // hf_xet serializes the same in-memory shard and cuts the footer, so the
    // server's re-serialization of a verified upload reproduces it exactly.
    let stored = stored_shard(&capture_shard(), &capture_lookup()).unwrap();
    assert_eq!(stored, capture_shard_bytes());
}

#[test]
fn stored_shard_is_deterministic() {
    let shard = capture_shard();
    let lookup = capture_lookup();
    let first = stored_shard(&shard, &lookup).unwrap();
    let second = stored_shard(&shard, &lookup).unwrap();
    assert_eq!(first, second);
    assert_eq!(shard_hash(&first), shard_hash(&second));
    assert_ne!(shard_hash(&first), MerkleHash::default());
}

#[test]
fn stored_shard_requires_stored_xorbs() {
    let none = |_: &MerkleHash| -> Option<Arc<[(MerkleHash, u64)]>> { None };
    let err = stored_shard(&capture_shard(), &none).unwrap_err();
    assert!(
        matches!(
            &err,
            ProtocolError::ShardNotVerified(VerifyError::UnknownXorb { xorb, .. })
                if hash_to_hex(xorb) == CAPTURE_XORB
        ),
        "{err}"
    );
}

#[test]
fn dedup_shard_hides_raw_chunk_hashes() {
    let xorb = capture_xorb();
    let key = MerkleHash::random_from_seed(42);
    let expiry = 1_900_000_000;
    let bytes = dedup_shard(&xorb.hash, &xorb.chunks.hashes, key, expiry).unwrap();

    for (chunk, _) in &xorb.chunks.hashes {
        assert!(
            !bytes.windows(32).any(|window| window == chunk.as_bytes()),
            "raw chunk hash {chunk} appears in the dedup shard"
        );
        // The lookup tables hold hashes truncated to their first 8 bytes.
        assert!(
            !bytes
                .windows(8)
                .any(|window| window == &chunk.as_bytes()[..8]),
            "truncated raw chunk hash {chunk} appears in the dedup shard"
        );
    }

    let info = MDBShardInfo::load_from_reader(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(info.metadata.chunk_hash_hmac_key, key);
    assert_eq!(info.metadata.shard_key_expiry, expiry);

    let shard = MDBMinimalShard::from_reader(&mut bytes.as_slice(), true, true).unwrap();
    assert_eq!(shard.num_files(), 0);
    assert_eq!(shard.num_xorb(), 1);
    let offered = shard.xorb(0).unwrap();
    assert_eq!(offered.xorb_hash(), xorb.hash);
    let keyed: Vec<MerkleHash> = (0..offered.num_entries())
        .map(|i| offered.chunk(i).chunk_hash)
        .collect();
    let expected: Vec<MerkleHash> = xorb
        .chunks
        .hashes
        .iter()
        .map(|(chunk, _)| chunk.hmac(key))
        .collect();
    assert_eq!(keyed, expected);
}

#[test]
fn dedup_shard_rejects_the_zero_key() {
    let xorb = capture_xorb();
    let result = dedup_shard(
        &xorb.hash,
        &xorb.chunks.hashes,
        MerkleHash::default(),
        1_900_000_000,
    );
    assert!(matches!(result, Err(ProtocolError::DedupKeyZero)));
}

#[test]
fn dedup_eligibility_matches_upstream() {
    let mut eligible = 0;
    for seed in 0..10_000 {
        let hash = MerkleHash::random_from_seed(seed);
        let last: [u8; 8] = hash.as_bytes()[24..32].try_into().unwrap();
        let expected = u64::from_le_bytes(last).is_multiple_of(1024);
        assert_eq!(is_dedup_eligible(&hash), expected, "{hash}");
        eligible += usize::from(expected);
    }
    assert!(eligible > 0, "no eligible hash among the samples");
}

/// A lookup holding one xorb whose single chunk is `len` bytes long.
fn one_chunk_lookup(
    xorb: MerkleHash,
    len: u64,
) -> impl Fn(&MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>> {
    let chunks: Arc<[(MerkleHash, u64)]> = vec![(MerkleHash::random_from_seed(1), len)].into();
    move |x: &MerkleHash| (*x == xorb).then(|| Arc::clone(&chunks))
}

#[test]
fn chunk_tables_over_the_shard_formats_counts_are_rejected() {
    let xorb = MerkleHash::random_from_seed(9);
    let lookup = one_chunk_lookup(xorb, 1 << 32);
    let table = lookup(&xorb).unwrap();
    let shard = UploadShard {
        files: vec![ShardFileEntry {
            file_hash: MerkleHash::random_from_seed(10),
            segments: vec![Segment {
                xorb,
                chunk_start: 0,
                chunk_end: 1,
                unpacked_bytes: 0,
                verification: Some(MerkleHash::random_from_seed(11)),
            }],
            sha256: None,
        }],
        xorbs: vec![xorb],
    };
    let too_large = |err: &ProtocolError| matches!(err, ProtocolError::XorbTableTooLarge { xorb: x } if *x == xorb);
    let err = stored_shard(&shard, &lookup).unwrap_err();
    assert!(too_large(&err), "{err}");
    let err = dedup_shard(&xorb, &table, MerkleHash::random_from_seed(12), 0).unwrap_err();
    assert!(too_large(&err), "{err}");
}

#[test]
fn stored_shard_keeps_an_empty_file() {
    let none = |_: &MerkleHash| -> Option<Arc<[(MerkleHash, u64)]>> { None };
    for sha256 in [None, Some([0; 32])] {
        let shard = UploadShard {
            files: vec![ShardFileEntry {
                file_hash: MerkleHash::default(),
                segments: Vec::new(),
                sha256,
            }],
            xorbs: Vec::new(),
        };
        let stored = stored_shard(&shard, &none).unwrap();
        let back = read_upload_shard(stored.as_slice()).unwrap();
        assert_eq!(back, shard);
        verify_shard(&back, &none, VerifyLimits::default()).unwrap();
    }
}
