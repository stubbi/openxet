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
    DEDUP_ANSWER_BUDGET, MerkleHash, ProtocolError, Segment, ShardFileEntry, StoredXorb,
    UploadShard, VerifyError, VerifyLimits, dedup_shard, hash_to_hex, is_dedup_eligible,
    read_dedup_answer, read_upload_shard, read_xorb_chunks, shard_hash, stored_shard, verify_shard,
    xorb_with_footer,
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
    let (bytes, offered) = build_answer(
        &[(xorb.hash, &xorb.chunks.hashes[..])],
        DEDUP_ANSWER_BUDGET,
        key,
        expiry,
    )
    .unwrap();
    assert_eq!(offered, 1);

    for (chunk, _) in &xorb.chunks.hashes {
        assert!(
            !bytes.windows(32).any(|window| window == chunk.as_bytes()),
            "raw chunk hash {chunk} appears in the dedup shard"
        );
        // Upstream's lookup tables would hold hashes truncated to their
        // first 8 bytes; the answer has none, but no window may hold one.
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
    // Ruling M1-R30: no lookup tables, as in the spec's reference answer;
    // the client rebuilds its index from the xorb section.
    assert_eq!(info.metadata.xorb_lookup_num_entry, 0);
    assert_eq!(info.metadata.chunk_lookup_num_entry, 0);
    assert_eq!(info.metadata.file_lookup_num_entry, 0);

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
fn read_dedup_answer_finds_every_offered_chunk_as_the_client_does() {
    let xorb = capture_xorb();
    let key = MerkleHash::random_from_seed(7);
    let (bytes, _) = build_answer(
        &[(xorb.hash, &xorb.chunks.hashes[..])],
        DEDUP_ANSWER_BUDGET,
        key,
        1_900_000_000,
    )
    .unwrap();

    for (chunk, _) in &xorb.chunks.hashes {
        let answer = read_dedup_answer(&bytes, chunk).unwrap();
        assert_eq!(answer.key, key);
        assert_eq!(answer.expiry_unix, 1_900_000_000);
        assert_eq!(answer.files, 0);
        assert_eq!(answer.xorbs, vec![xorb.hash]);
        // The capture repeats chunks; any position holding the chunk will do.
        let (matched, index) = answer.matched.unwrap();
        assert_eq!(matched, xorb.hash);
        assert_eq!(xorb.chunks.hashes[index as usize].0, *chunk);
    }
    let absent = read_dedup_answer(&bytes, &MerkleHash::random_from_seed(8)).unwrap();
    assert_eq!(absent.matched, None);
    // The client reads the footer first (S10 §2): a body without one fails.
    let footer_less = &bytes[..bytes.len() - 200];
    assert!(matches!(
        read_dedup_answer(footer_less, &xorb.chunks.hashes[0].0),
        Err(ProtocolError::MalformedShard(_))
    ));
}

#[test]
fn dedup_shard_rejects_the_zero_key() {
    let xorb = capture_xorb();
    let result = build_answer(
        &[(xorb.hash, &xorb.chunks.hashes[..])],
        DEDUP_ANSWER_BUDGET,
        MerkleHash::default(),
        1_900_000_000,
    );
    assert!(matches!(result, Err(ProtocolError::DedupKeyZero)));
}

/// The keyed answer `dedup_shard` builds, and how many entries it covers.
fn build_answer(
    xorbs: &[(MerkleHash, &[(MerkleHash, u64)])],
    budget: u64,
    key: MerkleHash,
    expiry: u64,
) -> Result<(Vec<u8>, usize), ProtocolError> {
    let unkeyed = dedup_shard(xorbs.iter().copied(), budget)?;
    let (covered, len) = (unkeyed.covered(), unkeyed.len());
    let body = unkeyed.keyed(key, expiry)?;
    assert_eq!(u64::try_from(body.len()).unwrap(), len);
    Ok((body, covered))
}

/// `count` synthetic chunk tables of `chunks` chunks each, distinct per
/// `seed`, as the index stores them.
fn synthetic_xorbs(
    seed: u64,
    count: usize,
    chunks: usize,
) -> Vec<(MerkleHash, Vec<(MerkleHash, u64)>)> {
    (0..count as u64)
        .map(|x| {
            let base = seed * 1_000_000 + x * 10_000;
            let table = (0..chunks as u64)
                .map(|c| (MerkleHash::random_from_seed(base + c + 1), 60_000 + c))
                .collect();
            (MerkleHash::random_from_seed(base), table)
        })
        .collect()
}

fn as_refs(
    xorbs: &[(MerkleHash, Vec<(MerkleHash, u64)>)],
) -> Vec<(MerkleHash, &[(MerkleHash, u64)])> {
    xorbs.iter().map(|(x, t)| (*x, t.as_slice())).collect()
}

#[test]
fn dedup_answer_offers_every_xorb_given_and_finds_each_chunk() {
    // Ruling M1-R30: the answer offers several xorbs; the client finds every
    // chunk of every one of them through the keyed index it rebuilds from
    // the xorb section.
    let xorbs = synthetic_xorbs(1, 4, 50);
    let key = MerkleHash::random_from_seed(77);
    let (bytes, offered) =
        build_answer(&as_refs(&xorbs), DEDUP_ANSWER_BUDGET, key, 1_900_000_000).unwrap();
    assert_eq!(offered, 4);

    let mut expected: Vec<MerkleHash> = xorbs.iter().map(|(x, _)| *x).collect();
    for (xorb, table) in &xorbs {
        for (index, (chunk, _)) in table.iter().enumerate() {
            let answer = read_dedup_answer(&bytes, chunk).unwrap();
            assert_eq!(answer.files, 0);
            let mut got = answer.xorbs.clone();
            got.sort();
            expected.sort();
            assert_eq!(got, expected);
            assert_eq!(answer.matched, Some((*xorb, u32::try_from(index).unwrap())));
            assert!(!bytes.windows(32).any(|w| w == chunk.as_bytes()));
            assert!(!bytes.windows(8).any(|w| w == &chunk.as_bytes()[..8]));
        }
    }
}

#[test]
fn dedup_answer_respects_the_size_budget() {
    // Ruling M1-R30: the answer stops at the budget, at a xorb boundary,
    // counting its exact serialized size, and still parses.
    let xorbs = synthetic_xorbs(2, 5, 20);
    let refs = as_refs(&xorbs);
    let key = MerkleHash::random_from_seed(78);
    let (full, all) = build_answer(&refs, DEDUP_ANSWER_BUDGET, key, 0).unwrap();
    assert_eq!(all, 5);
    let full_len = u64::try_from(full.len()).unwrap();
    let (exact, fit) = build_answer(&refs, full_len, key, 0).unwrap();
    assert_eq!((exact.len(), fit), (full.len(), 5));

    let (cut, fit) = build_answer(&refs, full_len - 1, key, 0).unwrap();
    assert_eq!(fit, 4);
    assert!(u64::try_from(cut.len()).unwrap() < full_len - 1);
    let answer = read_dedup_answer(&cut, &xorbs[0].1[0].0).unwrap();
    let mut got = answer.xorbs;
    got.sort();
    let mut want: Vec<MerkleHash> = xorbs[..4].iter().map(|(x, _)| *x).collect();
    want.sort();
    assert_eq!(got, want);
    let last = &xorbs[4].1[0].0;
    assert_eq!(read_dedup_answer(&cut, last).unwrap().matched, None);

    // The first xorb is always offered, however small the budget.
    let (tiny, fit) = build_answer(&refs, 1, key, 0).unwrap();
    assert_eq!(fit, 1);
    assert_eq!(
        read_dedup_answer(&tiny, &xorbs[0].1[0].0).unwrap().xorbs,
        vec![xorbs[0].0]
    );

    // A xorb given twice is offered once and counts as covered.
    let twice = [refs[0], refs[1], refs[0]];
    let (_, fit) = build_answer(&twice, DEDUP_ANSWER_BUDGET, key, 0).unwrap();
    assert_eq!(fit, 3);
}

#[test]
fn dedup_answer_budget_is_16_mib_and_holds_350_typical_xorbs() {
    // Ruling M1-R32: 16 MiB, which still offers about 350 xorbs of the
    // typical density (about 980 chunks each), about 22 GB of file.
    assert_eq!(DEDUP_ANSWER_BUDGET, 16 << 20);
    let xorbs = synthetic_xorbs(4, 351, 980);
    let unkeyed = dedup_shard(as_refs(&xorbs), DEDUP_ANSWER_BUDGET).unwrap();
    assert_eq!(unkeyed.covered(), 351);
    assert!(unkeyed.len() <= DEDUP_ANSWER_BUDGET);
    let more = synthetic_xorbs(5, 400, 980);
    let cut = dedup_shard(as_refs(&more), DEDUP_ANSWER_BUDGET).unwrap();
    assert!(cut.covered() < 400);
    assert!(cut.len() <= DEDUP_ANSWER_BUDGET);
}

#[test]
fn dedup_answer_xorb_bytes_matches_the_serialized_growth() {
    let key = MerkleHash::random_from_seed(79);
    let one = synthetic_xorbs(3, 1, 10);
    let two = synthetic_xorbs(3, 2, 10);
    let (a, _) = build_answer(&as_refs(&one), DEDUP_ANSWER_BUDGET, key, 0).unwrap();
    let (b, _) = build_answer(&as_refs(&two), DEDUP_ANSWER_BUDGET, key, 0).unwrap();
    assert_eq!(
        u64::try_from(b.len() - a.len()).unwrap(),
        openxet_protocol::dedup_answer_xorb_bytes(10)
    );
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
    let err = build_answer(
        &[(xorb, &table[..])],
        DEDUP_ANSWER_BUDGET,
        MerkleHash::random_from_seed(12),
        0,
    )
    .unwrap_err();
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

/// The capture shard plus a second entry for `a.bin` that references another
/// xorb, with the given SHA-256.
fn capture_with_duplicate(sha256: Option<[u8; 32]>) -> (UploadShard, MerkleHash) {
    let mut shard = capture_shard();
    let other = MerkleHash::random_from_seed(13);
    let original = &shard.files[1];
    assert!(!original.segments.is_empty());
    shard.files.push(ShardFileEntry {
        file_hash: original.file_hash,
        segments: vec![Segment {
            xorb: other,
            chunk_start: 0,
            chunk_end: 1,
            unpacked_bytes: 1,
            verification: Some(MerkleHash::random_from_seed(14)),
        }],
        sha256,
    });
    (shard, other)
}

/// The capture's lookup, which also knows the duplicate's xorb.
fn lookup_with(other: MerkleHash) -> impl Fn(&MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>> {
    let capture = capture_lookup();
    let extra = one_chunk_lookup(other, 1);
    move |x: &MerkleHash| capture(x).or_else(|| extra(x))
}

#[test]
fn stored_shard_keeps_the_first_of_duplicate_file_entries() {
    let a_sha256 = capture_shard().files[1].sha256;
    assert!(a_sha256.is_some());
    for sha256 in [None, a_sha256] {
        let (shard, other) = capture_with_duplicate(sha256);
        let stored = stored_shard(&shard, &lookup_with(other)).unwrap();
        // The second entry and the xorb only it references are left out.
        assert_eq!(stored, capture_shard_bytes());
    }
}

#[test]
fn verify_shard_rejects_duplicate_files_with_conflicting_sha256() {
    let (shard, other) = capture_with_duplicate(Some([7; 32]));
    let file = shard.files[1].file_hash;
    let err = verify_shard(&shard, &lookup_with(other), VerifyLimits::default()).unwrap_err();
    assert!(
        matches!(&err, VerifyError::ConflictingFileSha256 { file: f } if *f == file),
        "{err}"
    );
}

#[test]
fn stored_shard_rejects_duplicate_files_with_conflicting_sha256() {
    let (shard, other) = capture_with_duplicate(Some([7; 32]));
    let file = shard.files[1].file_hash;
    let err = stored_shard(&shard, &lookup_with(other)).unwrap_err();
    assert!(
        matches!(&err, ProtocolError::ShardFileSha256Conflict { file: f } if *f == file),
        "{err}"
    );
}
