//! Global deduplication: <https://huggingface.co/docs/xet/deduplication> and
//! <https://huggingface.co/docs/xet/shard#global-deduplication>.

use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Duration;

use xet_core_structures::metadata_shard::shard_in_memory::MDBInMemoryShard;
use xet_core_structures::metadata_shard::xorb_structs::{
    XorbChunkSequenceEntry, XorbChunkSequenceHeader,
};
use xet_core_structures::metadata_shard::{MDBShardInfo, hash_is_global_dedup_eligible};

use crate::shard::xorb_info;
use crate::{MerkleHash, ProtocolError};

/// Whether clients may query the global dedup endpoint for `chunk` by its
/// hash alone: the last 8 bytes as a little-endian u64 are a multiple of 1024
/// (<https://huggingface.co/docs/xet/deduplication>). The first chunk of a
/// file is queried regardless; that rule is the caller's.
///
/// Upstream's `hash_is_global_dedup_eligible`; debug builds of upstream let
/// `HF_XET_MDB_SHARD_GLOBAL_DEDUP_CHUNK_MODULUS` replace 1024 for its tests.
#[must_use]
pub fn is_dedup_eligible(chunk: &MerkleHash) -> bool {
    hash_is_global_dedup_eligible(chunk)
}

/// The largest dedup answer built, in bytes: 16 MiB (ruling M1-R32).
///
/// The basis: a xorb costs 48 B per chunk in an answer
/// ([`dedup_answer_xorb_bytes`]), and stock clients' xorbs average about
/// 47 KB of answer each (about 980 chunks of 64 KiB; S11), so 16 MiB offers
/// about 350 xorbs, about 22 GB of file. That keeps design §1a's
/// fresh-cache result for files well past 10 GB, while bounding what one
/// cheap query can make the server build and hold: the spec's 64 MiB shard
/// maximum would cost four times as much per answer for no measured gain.
pub const DEDUP_ANSWER_BUDGET: u64 = 16 * 1024 * 1024;

/// What offering a xorb of `chunks` chunks adds to a dedup answer, in
/// bytes: its xorb-section block, upstream's `MDBXorbInfo::num_bytes`. A
/// caller can bound what it loads before calling [`dedup_shard`], which
/// counts the same sizes.
#[must_use]
pub fn dedup_answer_xorb_bytes(chunks: usize) -> u64 {
    (size_of::<XorbChunkSequenceHeader>() + chunks * size_of::<XorbChunkSequenceEntry>()) as u64
}

/// A global dedup answer (`GET /v1/chunks/{prefix}/{chunk}`) serialized
/// but not yet keyed: [`dedup_shard`] builds it, [`Self::keyed`] keys it.
/// The two steps let the caller free the chunk tables before keying, so
/// the keyed export's peak is this serialization plus its output.
#[derive(Debug)]
pub struct UnkeyedDedupShard {
    plain: Vec<u8>,
    covered: usize,
    len: u64,
}

impl UnkeyedDedupShard {
    /// How many leading entries of [`dedup_shard`]'s input the answer
    /// covers.
    #[must_use]
    pub fn covered(&self) -> usize {
        self.covered
    }

    /// The exact length of the keyed answer, in bytes: at most the budget,
    /// unless the first xorb alone exceeds it.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the answer offers no xorb.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.covered == 0
    }

    /// The answer's body: a footered shard whose file-info section is empty
    /// and whose chunk hashes are keyed with `key`, as the spec requires
    /// (<https://huggingface.co/docs/xet/shard#global-deduplication>). Raw
    /// chunk hashes never appear in it.
    ///
    /// Upstream keys it (`MDBShardInfo::export_as_keyed_shard_streaming`),
    /// without file info and without the xorb and chunk lookup tables, as
    /// the spec's reference answer has none: the client rebuilds its chunk
    /// index from the xorb section (`MDBShardInfo::read_all_truncated_hashes`,
    /// `xet-core-structures` 1.7.0). Upstream stamps the key expiry from its
    /// own clock; openxet's caller decides it, so the footer is
    /// re-serialized with `shard_key_expiry = expiry_unix` (seconds since
    /// the Unix epoch).
    ///
    /// # Errors
    ///
    /// [`ProtocolError::DedupKeyZero`] for the all-zero key, which upstream
    /// treats as "unkeyed".
    pub fn keyed(self, key: MerkleHash, expiry_unix: u64) -> Result<Vec<u8>, ProtocolError> {
        if key == MerkleHash::default() {
            return Err(ProtocolError::DedupKeyZero);
        }
        let capacity = usize::try_from(self.len)
            .unwrap_or_else(|_| unreachable!("the length of an in-memory buffer"));
        let mut keyed = Vec::with_capacity(capacity);
        MDBShardInfo::export_as_keyed_shard_streaming(
            &mut Cursor::new(&self.plain),
            &mut keyed,
            key,
            // Replaced by `expiry_unix` below.
            Duration::ZERO,
            /* include_file_info */ false,
            /* include_xorb_lookup_table */ false,
            /* include_chunk_lookup_table */ false,
        )
        .map_err(ProtocolError::ShardSerialization)?;
        drop(self.plain);

        let mut info = MDBShardInfo::load_from_reader(&mut Cursor::new(&keyed))
            .map_err(ProtocolError::ShardSerialization)?;
        info.metadata.shard_key_expiry = expiry_unix;
        let footer_start = usize::try_from(info.metadata.footer_offset)
            .unwrap_or_else(|_| unreachable!("offset into an in-memory buffer"));
        keyed.truncate(footer_start);
        info.metadata
            .serialize(&mut keyed)
            .map_err(ProtocolError::ShardSerialization)?;
        debug_assert_eq!(
            keyed.len() as u64,
            self.len,
            "the answer's size was miscounted"
        );
        Ok(keyed)
    }
}

/// Serializes a dedup answer offering `xorbs`, each a xorb with its stored
/// chunk table, for [`UnkeyedDedupShard::keyed`].
///
/// Entries are taken in order while the keyed answer stays within `budget`
/// bytes, counting its exact serialized size; the first is always taken, so
/// the queried chunk's xorb is offered however small the budget (one xorb
/// is at most about 400 KB). A xorb listed twice is offered once and counts
/// as covered. Each table is dropped as soon as it is copied, and nothing
/// past the budget is read. The xorb section is in upstream's order (by
/// xorb hash), not the order given; the client indexes every offered chunk
/// whatever the order (S11).
///
/// Upstream builds the shard (`MDBShardInfo::serialize_from`); its lookup
/// tables are cut off, as the keyed export never reads them.
///
/// # Errors
///
/// [`ProtocolError::XorbTableTooLarge`] if a chunk table doesn't fit the
/// shard format's 32-bit counts.
pub fn dedup_shard<T: AsRef<[(MerkleHash, u64)]>>(
    xorbs: impl IntoIterator<Item = (MerkleHash, T)>,
    budget: u64,
) -> Result<UnkeyedDedupShard, ProtocolError> {
    let mut mdb = MDBInMemoryShard::default();
    let mut len = answer_overhead()?;
    let mut covered = 0;
    for (xorb, chunks) in xorbs {
        if !mdb.xorb_content.contains_key(&xorb) {
            let info = xorb_info(&xorb, chunks.as_ref())?;
            drop(chunks);
            let grown = len + info.num_bytes();
            if covered > 0 && grown > budget {
                break;
            }
            len = grown;
            mdb.add_xorb_block(info)
                .map_err(ProtocolError::ShardSerialization)?;
        }
        covered += 1;
    }
    let plain = plain_sections(&mdb)?;
    Ok(UnkeyedDedupShard {
        plain,
        covered,
        len,
    })
}

/// `mdb` serialized by upstream, up to the end of its xorb section: all the
/// keyed export reads.
fn plain_sections(mdb: &MDBInMemoryShard) -> Result<Vec<u8>, ProtocolError> {
    let capacity = usize::try_from(mdb.shard_file_size())
        .unwrap_or_else(|_| unreachable!("the size of an in-memory shard"));
    let mut plain = Vec::with_capacity(capacity);
    let info = MDBShardInfo::serialize_from(&mut plain, mdb, None)
        .map_err(ProtocolError::ShardSerialization)?;
    let sections_end = usize::try_from(info.metadata.file_lookup_offset)
        .unwrap_or_else(|_| unreachable!("offset into an in-memory buffer"));
    plain.truncate(sections_end);
    Ok(plain)
}

/// The size of an answer offering no xorb: header, empty sections and
/// footer. Every answer is this plus its xorbs' blocks.
fn answer_overhead() -> Result<u64, ProtocolError> {
    static OVERHEAD: OnceLock<u64> = OnceLock::new();
    if let Some(size) = OVERHEAD.get() {
        return Ok(*size);
    }
    let plain = plain_sections(&MDBInMemoryShard::default())?;
    let mut keyed = Vec::new();
    MDBShardInfo::export_as_keyed_shard_streaming(
        &mut Cursor::new(&plain),
        &mut keyed,
        MerkleHash::from([1; 32]),
        Duration::ZERO,
        false,
        false,
        false,
    )
    .map_err(ProtocolError::ShardSerialization)?;
    Ok(*OVERHEAD.get_or_init(|| keyed.len() as u64))
}

/// A global dedup answer as a stock client reads it, for tests of the
/// servers that build one. Only with the `test-support` feature.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupAnswer {
    /// The footer's `chunk_hash_hmac_key`.
    pub key: MerkleHash,
    /// The footer's `shard_key_expiry`, in Unix seconds.
    pub expiry_unix: u64,
    /// The entries of the file-info section.
    pub files: usize,
    /// The offered xorbs, in shard order.
    pub xorbs: Vec<MerkleHash>,
    /// Where the client's lookup finds the queried chunk: the xorb and the
    /// chunk's index in it. `None` if the answer doesn't offer it.
    pub matched: Option<(MerkleHash, u32)>,
}

/// Reads `body`, a global dedup answer, with upstream's readers, and looks
/// `chunk` (raw, unkeyed) up in it as `hf_xet` does: by the truncated keyed
/// hash in the shard's chunk index, then chunk by chunk from that position
/// (`ShardFileManager::chunk_hash_dedup_query`, `xet-core-structures`
/// 1.7.0). The footer is read as the client reads it, so a body without one
/// fails.
///
/// # Errors
///
/// [`ProtocolError::MalformedShard`] if upstream's readers refuse `body`.
#[cfg(feature = "test-support")]
pub fn read_dedup_answer(body: &[u8], chunk: &MerkleHash) -> Result<DedupAnswer, ProtocolError> {
    use xet_core_structures::metadata_shard::streaming_shard::MDBMinimalShard;
    use xet_core_structures::metadata_shard::utils::truncate_hash;
    use xet_core_structures::metadata_shard::xorb_structs::MDBXorbInfoView;

    let mut reader = Cursor::new(body);
    let info =
        MDBShardInfo::load_from_reader(&mut reader).map_err(ProtocolError::MalformedShard)?;
    let shard = MDBMinimalShard::from_reader(&mut &body[..], true, true)
        .map_err(ProtocolError::MalformedShard)?;
    let xorbs = (0..shard.num_xorb())
        .filter_map(|i| shard.xorb(i).map(MDBXorbInfoView::xorb_hash))
        .collect();

    let wanted = truncate_hash(&info.keyed_chunk_hash(chunk));
    let mut matched = None;
    for (truncated, (xorb_index, chunk_offset)) in info
        .read_all_truncated_hashes(&mut reader)
        .map_err(ProtocolError::MalformedShard)?
    {
        if truncated != wanted {
            continue;
        }
        if let Some((_, entry)) = info
            .chunk_hash_dedup_query_direct(&mut reader, &[*chunk], xorb_index, chunk_offset)
            .map_err(ProtocolError::MalformedShard)?
        {
            matched = Some((entry.xorb_hash, entry.chunk_index_start));
            break;
        }
    }
    Ok(DedupAnswer {
        key: info.metadata.chunk_hash_hmac_key,
        expiry_unix: info.metadata.shard_key_expiry,
        files: shard.num_files(),
        xorbs,
        matched,
    })
}
