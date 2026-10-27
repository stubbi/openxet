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

/// Where each term of a file is stored: `(start, end)` per term, the bytes
/// `start..end` of its xorb holding its chunks' headers and payloads, that
/// is `offsets[chunk_start]..offsets[chunk_end]` of the xorb's
/// [`ChunkLayout`]. Recorded at registration, so reconstruction needs no
/// layout for a term it serves whole.
pub type TermBytes = Vec<(u32, u32)>;

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
    /// Every stored xorb is visible to every tenant: cross-tenant dedup.
    Global,
    /// A tenant sees only the xorbs it owns: those uploaded under it and
    /// those its registered files reference. Any other answers as absent.
    PerTenant,
}

/// Who registers a shard (ruling M1-R38), and the registration's number
/// (ruling M1-R37): the same three facts its bucket record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registrant<'a> {
    /// The scope the files are registered to.
    pub scope: &'a str,
    /// The tenant the scope belongs to, which comes to own the files'
    /// xorbs.
    pub tenant: &'a str,
    /// The registration's sequence number, from
    /// [`Index::next_registration_seq`](crate::Index::next_registration_seq)
    /// or the bucket record of an earlier attempt.
    pub seq: u64,
}

/// A repository's immutable id: a `UUIDv7`, shown as 32 lowercase hex
/// characters (ruling M2-R2). Its CAS scope is `repo:{id}`, and its bucket
/// keys carry the id, so a rename rewrites one record and orphans nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoId(pub [u8; 16]);

/// A pack row's id: a `UUIDv7`, shown as 32 lowercase hex characters. Each
/// attempt to write a pack takes a new one, so a row is never reused across
/// attempts (ADR 0004 item 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackId(pub [u8; 16]);

/// A Git object id: SHA-1, shown as 40 lowercase hex characters. SHA-256
/// repositories are out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(pub [u8; 20]);

/// Why a string isn't an id: the expected length in hex characters, which
/// the message names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected {0} hex characters")]
pub struct ParseIdError(usize);

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex, written in one call: a `tree` page formats thousands of
/// ids, and a `write!` per byte cost more than reading the tree did.
fn write_hex(f: &mut std::fmt::Formatter<'_>, bytes: &[u8]) -> std::fmt::Result {
    let mut buf = [0u8; 64];
    let out = buf.get_mut(..bytes.len() * 2).ok_or(std::fmt::Error)?;
    for (pair, b) in out.chunks_exact_mut(2).zip(bytes) {
        pair[0] = HEX_DIGITS[usize::from(b >> 4)];
        pair[1] = HEX_DIGITS[usize::from(b & 0xf)];
    }
    f.write_str(std::str::from_utf8(out).map_err(|_| std::fmt::Error)?)
}

/// `bytes` as lowercase hex.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| {
            [
                HEX_DIGITS[usize::from(b >> 4)],
                HEX_DIGITS[usize::from(b & 0xf)],
            ]
        })
        .map(char::from)
        .collect()
}

pub(crate) fn parse_hex<const N: usize>(s: &str) -> Result<[u8; N], ParseIdError> {
    // Checked up front: `from_str_radix` alone would take a `+` sign.
    if s.len() != N * 2 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ParseIdError(N * 2));
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => b - b'A' + 10,
    };
    let mut out = [0; N];
    for (byte, pair) in out.iter_mut().zip(s.as_bytes().chunks_exact(2)) {
        *byte = digit(pair[0]) << 4 | digit(pair[1]);
    }
    Ok(out)
}

macro_rules! hex_id {
    ($ty:ident, $len:literal) => {
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write_hex(f, &self.0)
            }
        }

        impl std::str::FromStr for $ty {
            type Err = ParseIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                parse_hex::<$len>(s).map(Self)
            }
        }

        impl $ty {
            pub(crate) fn from_slice(bytes: &[u8], what: &str) -> Result<Self, IndexError> {
                bytes.try_into().map(Self).map_err(|_| {
                    IndexError::Corrupt(format!("{what} is {} bytes, not {}", bytes.len(), $len))
                })
            }
        }
    };
}

hex_id!(RepoId, 16);
hex_id!(PackId, 16);
hex_id!(Oid, 20);

impl RepoId {
    /// A new id: a `UUIDv7`, so ids sort by creation time.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().into_bytes())
    }
}

impl PackId {
    /// A new id: a `UUIDv7`, so ids sort by creation time.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().into_bytes())
    }
}

const HASH_LEN: usize = 32;
const CHUNK_ENTRY_LEN: usize = HASH_LEN + 4 + 4;
pub(crate) const TERM_LEN: usize = HASH_LEN + 4 + 4 + 4;
const LAYOUT_ENTRY_LEN: usize = 4 + 4;
const TERM_BYTES_LEN: usize = 4 + 4;

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

/// Encodes [`TermBytes`] as `(start u32 LE, end u32 LE)` per term.
pub(crate) fn encode_term_bytes(bytes: &[(u32, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * TERM_BYTES_LEN);
    for (start, end) in bytes {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&end.to_le_bytes());
    }
    out
}

/// Decodes [`encode_term_bytes`]'s output for a file of `terms` terms.
///
/// # Errors
///
/// [`IndexError::Corrupt`] if the length doesn't match, or a range is
/// empty or reversed.
pub(crate) fn decode_term_bytes(bytes: &[u8], terms: usize) -> Result<TermBytes, IndexError> {
    if terms.checked_mul(TERM_BYTES_LEN) != Some(bytes.len()) {
        return Err(IndexError::Corrupt(format!(
            "the byte ranges of {terms} terms are {} bytes",
            bytes.len()
        )));
    }
    let mut out = Vec::with_capacity(terms);
    for e in bytes.chunks_exact(TERM_BYTES_LEN) {
        let (start, end) = (u32_at(e, 0), u32_at(e, 4));
        if start >= end {
            return Err(IndexError::Corrupt(format!(
                "a term's stored bytes are {start}..{end}"
            )));
        }
        out.push((start, end));
    }
    Ok(out)
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
    // Sized up front: collecting through `Result` can't, and a 100k-term
    // list would grow by copying.
    let mut terms = Vec::with_capacity(bytes.len() / TERM_LEN);
    for t in bytes.chunks_exact(TERM_LEN) {
        terms.push(Term {
            xorb: hash_from(&t[..HASH_LEN])?,
            chunk_start: u32_at(t, HASH_LEN),
            chunk_end: u32_at(t, HASH_LEN + 4),
            unpacked_bytes: u32_at(t, HASH_LEN + 8),
        });
    }
    Ok(terms)
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
    fn term_bytes_round_trip_and_reject_bad_entries() -> Result<(), IndexError> {
        let bytes = vec![(0, 17), (17, 70_000), (5, u32::MAX)];
        assert_eq!(decode_term_bytes(&encode_term_bytes(&bytes), 3)?, bytes);
        assert_eq!(decode_term_bytes(&[], 0)?, Vec::new());
        let encoded = encode_term_bytes(&bytes);
        for (bad, terms) in [
            (encoded.clone(), 2),
            (encoded[..encoded.len() - 1].to_vec(), 3),
            (encode_term_bytes(&[(9, 9)]), 1),
            (encode_term_bytes(&[(9, 3)]), 1),
        ] {
            assert!(matches!(
                decode_term_bytes(&bad, terms),
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
    fn ids_round_trip_through_hex() -> Result<(), ParseIdError> {
        let repo = RepoId::generate();
        let shown = repo.to_string();
        assert_eq!(shown.len(), 32);
        assert_eq!(shown, shown.to_ascii_lowercase());
        assert_eq!(shown.parse::<RepoId>()?, repo);
        let oid: Oid = "0123456789abcdefABCDEF0123456789abcdef01".parse()?;
        assert_eq!(oid.to_string(), "0123456789abcdefabcdef0123456789abcdef01");
        assert_ne!(PackId::generate(), PackId::generate());
        for bad in [
            "",
            "0123",
            "zz23456789abcdef0123456789abcdef",
            "+f23456789abcdef0123456789abcdef",
            "é123456789abcdef0123456789abcde",
        ] {
            assert_eq!(bad.parse::<RepoId>(), Err(ParseIdError(32)), "{bad}");
        }
        assert_eq!("00".repeat(16).parse::<Oid>(), Err(ParseIdError(40)));
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
