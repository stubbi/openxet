//! Global deduplication: <https://huggingface.co/docs/xet/deduplication> and
//! <https://huggingface.co/docs/xet/shard#global-deduplication>.

use std::io::Cursor;
use std::time::Duration;

use xet_core_structures::metadata_shard::shard_in_memory::MDBInMemoryShard;
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

/// The body of a global dedup answer (`GET /v1/chunks/{prefix}/{chunk}`)
/// offering one xorb: a footered shard whose file-info section is empty and
/// whose chunk hashes are keyed with `key`, as the spec requires
/// (<https://huggingface.co/docs/xet/shard#global-deduplication>). Raw chunk
/// hashes never appear in it.
///
/// `chunks` is the xorb's stored chunk table. Upstream builds the shard
/// (`MDBShardInfo::serialize_from`) and keys it
/// (`MDBShardInfo::export_as_keyed_shard_streaming`, without file info, with
/// the xorb and chunk lookup tables). Upstream stamps the key expiry from its
/// own clock; openxet's caller decides it, so the footer is re-serialized
/// with `shard_key_expiry = expiry_unix` (seconds since the Unix epoch).
///
/// # Errors
///
/// [`ProtocolError::DedupKeyZero`] for the all-zero key, which upstream
/// treats as "unkeyed"; [`ProtocolError::XorbTableTooLarge`] if `chunks`
/// doesn't fit the shard format's 32-bit counts.
pub fn dedup_shard(
    xorb: &MerkleHash,
    chunks: &[(MerkleHash, u64)],
    key: MerkleHash,
    expiry_unix: u64,
) -> Result<Vec<u8>, ProtocolError> {
    if key == MerkleHash::default() {
        return Err(ProtocolError::DedupKeyZero);
    }
    let mut mdb = MDBInMemoryShard::default();
    mdb.add_xorb_block(xorb_info(xorb, chunks)?)
        .map_err(ProtocolError::ShardSerialization)?;
    let mut plain = Vec::new();
    MDBShardInfo::serialize_from(&mut plain, &mdb, None)
        .map_err(ProtocolError::ShardSerialization)?;

    let mut keyed = Vec::new();
    MDBShardInfo::export_as_keyed_shard_streaming(
        &mut Cursor::new(&plain),
        &mut keyed,
        key,
        // Replaced by `expiry_unix` below.
        Duration::ZERO,
        /* include_file_info */ false,
        /* include_xorb_lookup_table */ true,
        /* include_chunk_lookup_table */ true,
    )
    .map_err(ProtocolError::ShardSerialization)?;

    let mut info = MDBShardInfo::load_from_reader(&mut Cursor::new(&keyed))
        .map_err(ProtocolError::ShardSerialization)?;
    info.metadata.shard_key_expiry = expiry_unix;
    let footer_start = usize::try_from(info.metadata.footer_offset)
        .unwrap_or_else(|_| unreachable!("offset into an in-memory buffer"));
    keyed.truncate(footer_start);
    info.metadata
        .serialize(&mut keyed)
        .map_err(ProtocolError::ShardSerialization)?;
    Ok(keyed)
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
