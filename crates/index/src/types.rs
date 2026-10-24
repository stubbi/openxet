//! The index's records and their stored blob encodings.

use std::sync::Arc;

use openxet_protocol::MerkleHash;

use crate::IndexError;

/// An access scope: a repo (`{type}/{namespace}/{name}`) in the Hub layer,
/// or an opaque string the platform chooses in storage-only mode (design
/// §5.4). The index compares scopes byte for byte and never parses them.
pub type Scope = String;

/// The stored chunk table of one xorb.
///
/// Invariant: `offsets.len() == hashes.len() + 1`. Entry `i` of `offsets` is
/// where chunk `i`'s header starts in the stored xorb and the last entry is
/// where its footer starts, as in `StoredXorb::chunk_offsets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkTable {
    /// `(chunk hash, uncompressed length)` per chunk, in xorb order: the
    /// element type `verify_shard`'s `XorbChunkLookup` takes.
    pub hashes: Arc<[(MerkleHash, u64)]>,
    /// The `n + 1` chunk offsets in the stored xorb.
    pub offsets: Arc<[u32]>,
}

/// Where one xorb's chunks are stored and how long each unpacks: a
/// [`ChunkTable`] without the chunk hashes, which is what reconstruction
/// planning reads.
///
/// Invariant: `offsets.len() == lengths.len() + 1`, as in [`ChunkTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkLayout {
    /// Each chunk's uncompressed length, in xorb order.
    pub lengths: Arc<[u32]>,
    /// The `n + 1` chunk offsets in the stored xorb.
    pub offsets: Arc<[u32]>,
}

/// One term of a file reconstruction: chunks `[chunk_start, chunk_end)` of
/// `xorb`, which unpack to `unpacked_bytes` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    /// The xorb holding the chunks.
    pub xorb: MerkleHash,
    /// The first chunk index, inclusive.
    pub chunk_start: u32,
    /// The last chunk index, exclusive.
    pub chunk_end: u32,
    /// The uncompressed bytes of the chunk range.
    pub unpacked_bytes: u32,
}

/// A file's reconstruction, content-addressed by its file hash: two records
/// with the same hash describe the same bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    /// The Xet file hash.
    pub hash: MerkleHash,
    /// The file's length in bytes.
    pub size: u64,
    /// The terms that reconstruct the file, in order.
    pub terms: Vec<Term>,
}

/// A file from a verified shard, to be registered to a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    /// The file's reconstruction.
    pub record: FileRecord,
    /// The client-asserted SHA-256 of the file. It is stored per scope and
    /// never trusted outside the scope that registered it (design §5.4).
    pub sha256: Option<[u8; 32]>,
    /// The hash of the file's first chunk, recorded as a dedup candidate
    /// pointing at the first term's xorb. `None` for the empty file.
    pub first_chunk: Option<MerkleHash>,
}

/// Which xorbs an existence check may see (design §5.4, §5.9 req 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Every stored xorb is visible to every scope: cross-tenant dedup.
    Global,
    /// A scope sees only xorbs uploaded to it; any other answers as absent.
    PerScope,
}

const HASH_LEN: usize = 32;
const CHUNK_ENTRY_LEN: usize = HASH_LEN + 4 + 4;
const TERM_LEN: usize = HASH_LEN + 4 + 4 + 4;
const LAYOUT_ENTRY_LEN: usize = 4 + 4;

pub(crate) fn hash_from(bytes: &[u8]) -> Result<MerkleHash, IndexError> {
    let raw: [u8; HASH_LEN] = bytes
        .try_into()
        .map_err(|_| IndexError::Corrupt(format!("a hash is {} bytes, not 32", bytes.len())))?;
    Ok(MerkleHash::from(raw))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut le = [0; 4];
    le.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(le)
}

/// Encodes a chunk table as `(hash, uncompressed length u32 LE, offset u32
/// LE)` per chunk, then the final offset.
///
/// # Errors
///
/// [`IndexError::InvalidInput`] if `offsets` doesn't have one entry more
/// than `hashes`, or a chunk length doesn't fit in `u32`.
pub(crate) fn encode_chunk_table(table: &ChunkTable) -> Result<Vec<u8>, IndexError> {
    let n = table.hashes.len();
    if table.offsets.len() != n + 1 {
        return Err(IndexError::InvalidInput(format!(
            "a chunk table of {n} chunks needs {} offsets, got {}",
            n + 1,
            table.offsets.len()
        )));
    }
    let mut out = Vec::with_capacity(n * CHUNK_ENTRY_LEN + 4);
    for ((hash, len), offset) in table.hashes.iter().zip(table.offsets.iter()) {
        let len = u32::try_from(*len).map_err(|_| {
            IndexError::InvalidInput(format!("chunk {hash} is {len} bytes, over u32::MAX"))
        })?;
        out.extend_from_slice(hash.as_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out.extend_from_slice(&table.offsets[n].to_le_bytes());
    Ok(out)
}

/// Decodes [`encode_chunk_table`]'s output, checking it holds exactly
/// `chunk_count` chunks.
///
/// # Errors
///
/// [`IndexError::Corrupt`] if the length doesn't match.
pub(crate) fn decode_chunk_table(
    bytes: &[u8],
    chunk_count: usize,
) -> Result<ChunkTable, IndexError> {
    let expected = chunk_count
        .checked_mul(CHUNK_ENTRY_LEN)
        .and_then(|l| l.checked_add(4));
    if expected != Some(bytes.len()) {
        return Err(IndexError::Corrupt(format!(
            "a chunk table of {chunk_count} chunks is {} bytes",
            bytes.len()
        )));
    }
    let mut hashes = Vec::with_capacity(chunk_count);
    let mut offsets = Vec::with_capacity(chunk_count + 1);
    for entry in bytes[..bytes.len() - 4].chunks_exact(CHUNK_ENTRY_LEN) {
        hashes.push((
            hash_from(&entry[..HASH_LEN])?,
            u64::from(u32_at(entry, HASH_LEN)),
        ));
        offsets.push(u32_at(entry, HASH_LEN + 4));
    }
    offsets.push(u32_at(bytes, bytes.len() - 4));
    Ok(ChunkTable {
        hashes: hashes.into(),
        offsets: offsets.into(),
    })
}

/// Encodes a chunk table's [`ChunkLayout`] as `(uncompressed length u32 LE,
/// offset u32 LE)` per chunk, then the final offset. Called after
/// [`encode_chunk_table`] accepted the table, so every length fits `u32`.
pub(crate) fn encode_chunk_layout(table: &ChunkTable) -> Vec<u8> {
    let n = table.hashes.len();
    let mut out = Vec::with_capacity(n * LAYOUT_ENTRY_LEN + 4);
    for ((_, len), offset) in table.hashes.iter().zip(table.offsets.iter()) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "encode_chunk_table rejected lengths over u32::MAX"
        )]
        out.extend_from_slice(&(*len as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out.extend_from_slice(&table.offsets[n].to_le_bytes());
    out
}

/// Decodes [`encode_chunk_layout`]'s output, checking it holds exactly
/// `chunk_count` chunks.
///
/// # Errors
///
/// [`IndexError::Corrupt`] if the length doesn't match.
pub(crate) fn decode_chunk_layout(
    bytes: &[u8],
    chunk_count: usize,
) -> Result<ChunkLayout, IndexError> {
    let expected = chunk_count
        .checked_mul(LAYOUT_ENTRY_LEN)
        .and_then(|l| l.checked_add(4));
    if expected != Some(bytes.len()) {
        return Err(IndexError::Corrupt(format!(
            "a chunk layout of {chunk_count} chunks is {} bytes",
            bytes.len()
        )));
    }
    let entries = bytes[..bytes.len() - 4].chunks_exact(LAYOUT_ENTRY_LEN);
    let lengths = entries.clone().map(|e| u32_at(e, 0)).collect();
    let offsets = entries
        .map(|e| u32_at(e, 4))
        .chain([u32_at(bytes, bytes.len() - 4)])
        .collect();
    Ok(ChunkLayout { lengths, offsets })
}

/// The [`ChunkLayout`] of an [`encode_chunk_table`] output, for xorbs
/// stored before layouts were kept on their own.
///
/// # Errors
///
/// [`IndexError::Corrupt`] if the length doesn't match `chunk_count`.
pub(crate) fn decode_layout_of_chunk_table(
    bytes: &[u8],
    chunk_count: usize,
) -> Result<ChunkLayout, IndexError> {
    let table = decode_chunk_table(bytes, chunk_count)?;
    #[allow(clippy::cast_possible_truncation, reason = "decoded from u32 fields")]
    let lengths = table.hashes.iter().map(|(_, len)| *len as u32).collect();
    Ok(ChunkLayout {
        lengths,
        offsets: table.offsets,
    })
}

/// Encodes terms as `(xorb, chunk start u32 LE, chunk end u32 LE, unpacked
/// bytes u32 LE)` each.
pub(crate) fn encode_terms(terms: &[Term]) -> Vec<u8> {
    let mut out = Vec::with_capacity(terms.len() * TERM_LEN);
    for t in terms {
        out.extend_from_slice(t.xorb.as_bytes());
        out.extend_from_slice(&t.chunk_start.to_le_bytes());
        out.extend_from_slice(&t.chunk_end.to_le_bytes());
        out.extend_from_slice(&t.unpacked_bytes.to_le_bytes());
    }
    out
}

/// Decodes [`encode_terms`]'s output.
///
/// # Errors
///
/// [`IndexError::Corrupt`] if the length isn't a whole number of terms.
pub(crate) fn decode_terms(bytes: &[u8]) -> Result<Vec<Term>, IndexError> {
    if !bytes.len().is_multiple_of(TERM_LEN) {
        return Err(IndexError::Corrupt(format!(
            "a term list is {} bytes, not a multiple of {TERM_LEN}",
            bytes.len()
        )));
    }
    bytes
        .chunks_exact(TERM_LEN)
        .map(|t| {
            Ok(Term {
                xorb: hash_from(&t[..HASH_LEN])?,
                chunk_start: u32_at(t, HASH_LEN),
                chunk_end: u32_at(t, HASH_LEN + 4),
                unpacked_bytes: u32_at(t, HASH_LEN + 8),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use openxet_protocol::chunk_hash;

    use super::*;

    fn table() -> ChunkTable {
        ChunkTable {
            hashes: vec![(chunk_hash(b"a"), 1), (chunk_hash(b"b"), 131_072)].into(),
            offsets: vec![0, 17, 70_000].into(),
        }
    }

    #[test]
    fn chunk_table_round_trips() -> Result<(), IndexError> {
        let t = table();
        let bytes = encode_chunk_table(&t)?;
        assert_eq!(bytes.len(), 2 * CHUNK_ENTRY_LEN + 4);
        assert_eq!(decode_chunk_table(&bytes, 2)?, t);
        Ok(())
    }

    #[test]
    fn chunk_table_decode_rejects_wrong_length() -> Result<(), IndexError> {
        let bytes = encode_chunk_table(&table())?;
        assert!(matches!(
            decode_chunk_table(&bytes, 3),
            Err(IndexError::Corrupt(_))
        ));
        assert!(matches!(
            decode_chunk_table(&bytes[..bytes.len() - 1], 2),
            Err(IndexError::Corrupt(_))
        ));
        assert!(matches!(
            decode_chunk_table(&[], 0),
            Err(IndexError::Corrupt(_))
        ));
        assert!(matches!(
            decode_chunk_table(&bytes, usize::MAX),
            Err(IndexError::Corrupt(_))
        ));
        Ok(())
    }

    #[test]
    fn chunk_layout_round_trips_and_matches_the_table() -> Result<(), IndexError> {
        let t = table();
        let bytes = encode_chunk_layout(&t);
        assert_eq!(bytes.len(), 2 * LAYOUT_ENTRY_LEN + 4);
        let expected = ChunkLayout {
            lengths: vec![1, 131_072].into(),
            offsets: vec![0, 17, 70_000].into(),
        };
        assert_eq!(decode_chunk_layout(&bytes, 2)?, expected);
        assert_eq!(
            decode_layout_of_chunk_table(&encode_chunk_table(&t)?, 2)?,
            expected
        );
        for (bytes, count) in [
            (&bytes[..], 3),
            (&bytes[..bytes.len() - 1], 2),
            (&[][..], 0),
            (&bytes[..], usize::MAX),
        ] {
            assert!(matches!(
                decode_chunk_layout(bytes, count),
                Err(IndexError::Corrupt(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn chunk_table_encode_rejects_bad_shape() {
        let mut t = table();
        t.offsets = vec![0, 17].into();
        assert!(matches!(
            encode_chunk_table(&t),
            Err(IndexError::InvalidInput(_))
        ));
        let mut t = table();
        t.hashes = vec![
            (chunk_hash(b"a"), u64::from(u32::MAX) + 1),
            (chunk_hash(b"b"), 1),
        ]
        .into();
        assert!(matches!(
            encode_chunk_table(&t),
            Err(IndexError::InvalidInput(_))
        ));
    }

    #[test]
    fn terms_round_trip() -> Result<(), IndexError> {
        let terms = vec![
            Term {
                xorb: chunk_hash(b"x"),
                chunk_start: 0,
                chunk_end: 3,
                unpacked_bytes: 9000,
            },
            Term {
                xorb: chunk_hash(b"y"),
                chunk_start: 5,
                chunk_end: u32::MAX,
                unpacked_bytes: u32::MAX,
            },
        ];
        let bytes = encode_terms(&terms);
        assert_eq!(bytes.len(), 2 * TERM_LEN);
        assert_eq!(decode_terms(&bytes)?, terms);
        assert_eq!(decode_terms(&[])?, Vec::new());
        Ok(())
    }

    #[test]
    fn terms_decode_rejects_partial_term() {
        let bytes = encode_terms(&[Term {
            xorb: chunk_hash(b"x"),
            chunk_start: 0,
            chunk_end: 1,
            unpacked_bytes: 1,
        }]);
        assert!(matches!(
            decode_terms(&bytes[..TERM_LEN - 1]),
            Err(IndexError::Corrupt(_))
        ));
    }
}
