//! `verify_shard` recomputes file hashes incrementally (upstream's
//! `MerkleHashSubtree`, merged segment by segment). This pins that path to
//! upstream's one-shot `file_hash` over a large file split into many
//! segments, including the boundary cases of the aggregation.

#![allow(clippy::unwrap_used)]

use openxet_protocol::{
    MAX_REFERENCED_CHUNKS, MerkleHash, Segment, ShardFileEntry, UploadShard, VerifyError,
    chunk_hash, file_hash, range_hash, verify_shard,
};

const CHUNKS: usize = 100_000;
/// The spec's mean branching factor; a level-0 aggregation group ends at the
/// first node from its third on whose hash is 0 mod 4, or after 9 nodes
/// (<https://huggingface.co/docs/xet/hashing#xorb-hashes>, upstream
/// `next_merge_cut`). Used only to place segment ends on group cuts.
const BRANCHING: u64 = 4;
const MAX_GROUP: usize = 9;

/// xorshift64*, fixed seed: the segmentation is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// End positions (exclusive) of the level-0 aggregation groups.
fn group_cuts(nodes: &[(MerkleHash, u64)]) -> Vec<usize> {
    let mut cuts = Vec::new();
    let mut start = 0;
    while start < nodes.len() {
        let rest = &nodes[start..];
        let len = if rest.len() <= 2 {
            rest.len()
        } else {
            let end = MAX_GROUP.min(rest.len());
            (2..end)
                .find(|&i| rest[i].0 % BRANCHING == 0)
                .map_or(end, |i| i + 1)
        };
        start += len;
        cuts.push(start);
    }
    cuts
}

#[test]
fn incremental_file_hash_matches_one_shot_over_many_random_segments() {
    let mut rng = Rng(0x5eed_0f0e_7ae7);
    let nodes: Vec<(MerkleHash, u64)> = (0..CHUNKS as u64)
        .map(|i| (chunk_hash(&i.to_le_bytes()), 1 + rng.below(128 * 1024)))
        .collect();
    let cuts = group_cuts(&nodes);
    let xorb = chunk_hash(b"synthetic xorb");

    let (mut segments, mut single, mut on_cut) = (Vec::new(), 0, 0);
    let mut start = 0;
    while start < CHUNKS {
        let end = match rng.below(4) {
            0 => start + 1,
            1 => cuts[cuts.partition_point(|&c| c <= start)],
            _ => start + 1 + usize::try_from(rng.below(300)).unwrap(),
        }
        .min(CHUNKS);
        single += usize::from(end - start == 1);
        on_cut += usize::from(cuts.binary_search(&end).is_ok());
        let range = &nodes[start..end];
        let hashes: Vec<MerkleHash> = range.iter().map(|(h, _)| *h).collect();
        segments.push(Segment {
            xorb,
            chunk_start: u32::try_from(start).unwrap(),
            chunk_end: u32::try_from(end).unwrap(),
            unpacked_bytes: u32::try_from(range.iter().map(|(_, l)| l).sum::<u64>()).unwrap(),
            verification: Some(range_hash(&hashes)),
        });
        start = end;
    }
    assert!(segments.len() > 1000, "{} segments", segments.len());
    assert!(
        single > 200 && on_cut > 200,
        "{single} single, {on_cut} on cuts"
    );

    let lookup = |x: &MerkleHash| (*x == xorb).then_some(nodes.as_slice());
    let mut shard = UploadShard {
        files: vec![ShardFileEntry {
            file_hash: file_hash(&nodes),
            segments,
            sha256: None,
        }],
        xorbs: vec![xorb],
    };
    verify_shard(&shard, lookup, MAX_REFERENCED_CHUNKS).unwrap();

    let mut bytes: [u8; 32] = shard.files[0].file_hash.as_bytes().try_into().unwrap();
    bytes[31] ^= 1;
    shard.files[0].file_hash = MerkleHash::from(bytes);
    let err = verify_shard(&shard, lookup, MAX_REFERENCED_CHUNKS).unwrap_err();
    assert!(matches!(err, VerifyError::FileHashMismatch { .. }), "{err}");
}
