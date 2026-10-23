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
        false,
        true,
        true,
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
