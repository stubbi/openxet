//! Upload shards: <https://huggingface.co/docs/xet/shard#shard-upload>.
//!
//! Parsing goes through `xet-core-structures`' section parsers; the checks
//! here are the server-side rules openxet applies before registering files.
//! `docs/research/S6-shard-verification.md` lists each rule, its source and
//! its test.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::io::{self, Read};
use std::sync::Arc;

use xet_core_structures::CoreError;
use xet_core_structures::metadata_shard::file_structs::MDBFileInfoView;
use xet_core_structures::metadata_shard::streaming_shard::{
    process_shard_file_info_section, process_shard_xorb_info_section,
};
use xet_core_structures::metadata_shard::{MDBShardFileFooter, MDBShardFileHeader};

use xet_core_structures::merklehash::MerkleHashSubtree;

use crate::{MerkleHash, ProtocolError, range_hash};

/// The largest shard upload accepted, in bytes. Clients must split larger
/// shards (<https://huggingface.co/docs/xet/upload-protocol#6-shard-serialization-and-upload>).
pub const MAX_UPLOAD_SHARD_BYTES: u64 = 64 * 1024 * 1024;

/// The default cap on chunks one shard may reference, summed over all its
/// segments, for [`verify_shard`]: 2^24 chunks, about 1 TiB of logical file
/// data at the ~64 KiB average chunk size.
///
/// The parse budget alone doesn't bound verification: a 64 MiB shard holds
/// ~700k segments, and each may span a whole xorb, so the referenced count
/// could reach billions. Verification hashes each referenced chunk hash once
/// (range hash and file hash), so this cap bounds CPU. Memory is bounded by
/// [`MAX_REFERENCED_XORBS`] instead.
pub const MAX_REFERENCED_CHUNKS: u64 = 1 << 24;

/// The default cap on distinct xorbs one shard may reference, for
/// [`verify_shard`].
///
/// The caller holds the chunk table of every distinct referenced xorb while
/// verifying: at most 8192 entries of 40 bytes (320 KiB) per xorb, so
/// 4096 xorbs is at most 1.25 GiB, and about 160 MiB at the typical 1024
/// chunks per xorb. Stock clients stay well below it: they flush a shard
/// at 64 MiB, and each xorb they upload adds a CAS info entry of 48 bytes per
/// chunk to it, at least 512 chunks for a full xorb, so a shard lists at most
/// about 2,700 new xorbs; the rest of the cap covers references to xorbs
/// already stored.
pub const MAX_REFERENCED_XORBS: usize = 4096;

/// SHA-256 of zero bytes, one of the values accepted for an empty file.
const EMPTY_SHA256: [u8; 32] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
    0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
];

/// A parsed upload shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadShard {
    /// The file info section, in shard order.
    pub files: Vec<ShardFileEntry>,
    /// The xorb hashes listed in the CAS info section, in shard order. The
    /// client's per-chunk listings are dropped: the server's own chunk hashes,
    /// recorded when it accepted each xorb, are the ones it trusts.
    /// Verification uses only the xorbs segments reference
    /// ([`UploadShard::referenced_xorbs`]); a listed xorb no segment
    /// references is ignored and never registered.
    pub xorbs: Vec<MerkleHash>,
}

impl UploadShard {
    /// The distinct xorbs the file segments reference, in order of first
    /// reference: the chunk tables [`verify_shard`] will look up.
    pub fn referenced_xorbs(&self) -> impl Iterator<Item = &MerkleHash> {
        let mut seen = HashSet::new();
        self.files
            .iter()
            .flat_map(|file| &file.segments)
            .map(|seg| &seg.xorb)
            .filter(move |xorb| seen.insert(*xorb))
    }
}

/// Where [`verify_shard`] finds the chunk table the server stored for a
/// xorb when it accepted it: `(chunk hash, uncompressed length)` per chunk,
/// in xorb order, or `None` if the xorb isn't stored.
///
/// Lookups are synchronous. A caller whose index is async loads the tables
/// of [`UploadShard::referenced_xorbs`] first, for example into a
/// `HashMap<MerkleHash, Arc<[(MerkleHash, u64)]>>`, which implements this
/// trait, as does any `Fn(&MerkleHash) -> Option<Arc<..>>`.
pub trait XorbChunkLookup {
    /// The stored chunk table of `xorb`, if it is stored.
    fn chunks(&self, xorb: &MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>>;
}

impl<S: BuildHasher> XorbChunkLookup for HashMap<MerkleHash, Arc<[(MerkleHash, u64)]>, S> {
    fn chunks(&self, xorb: &MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>> {
        self.get(xorb).cloned()
    }
}

impl<F> XorbChunkLookup for F
where
    F: Fn(&MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>>,
{
    fn chunks(&self, xorb: &MerkleHash) -> Option<Arc<[(MerkleHash, u64)]>> {
        self(xorb)
    }
}

/// The work bounds [`verify_shard`] enforces before any lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyLimits {
    /// Chunks referenced, summed over every segment; bounds CPU.
    pub max_referenced_chunks: u64,
    /// Distinct xorbs referenced; bounds the chunk tables the caller holds.
    pub max_referenced_xorbs: usize,
}

impl Default for VerifyLimits {
    /// [`MAX_REFERENCED_CHUNKS`] and [`MAX_REFERENCED_XORBS`].
    fn default() -> Self {
        Self {
            max_referenced_chunks: MAX_REFERENCED_CHUNKS,
            max_referenced_xorbs: MAX_REFERENCED_XORBS,
        }
    }
}

/// One file's reconstruction as uploaded in a shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardFileEntry {
    /// The file hash the client claims; [`verify_shard`] recomputes it.
    pub file_hash: MerkleHash,
    /// The reconstruction terms, in file order.
    pub segments: Vec<Segment>,
    /// The SHA-256 digest of the file contents, from the optional metadata
    /// extension. Client-asserted: the server can't check it without the
    /// file's bytes.
    pub sha256: Option<[u8; 32]>,
}

/// One reconstruction term: a chunk range of one xorb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// The xorb holding the chunks.
    pub xorb: MerkleHash,
    /// First chunk index, inclusive.
    pub chunk_start: u32,
    /// Last chunk index, exclusive.
    pub chunk_end: u32,
    /// The claimed uncompressed length of the range.
    pub unpacked_bytes: u32,
    /// The claimed range verification hash, if the shard has verification
    /// entries.
    pub verification: Option<MerkleHash>,
}

/// Why an upload shard's file or segment fails verification against the
/// stored xorb chunk hashes. The client must rebuild the shard from the xorbs
/// it uploaded.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The segment's chunk range is empty or runs past the xorb's last chunk.
    #[error(
        "segment {xorb}[{start}..{end}] is out of bounds: the xorb has {xorb_chunks} chunks and \
         a segment must cover at least one of them"
    )]
    OutOfBounds {
        /// The segment's xorb.
        xorb: MerkleHash,
        /// The segment's first chunk index.
        start: u32,
        /// The segment's end chunk index (exclusive).
        end: u32,
        /// The number of chunks the xorb has.
        xorb_chunks: usize,
    },
    /// The shard has no verification entry for the segment.
    #[error(
        "segment {xorb}[{start}..{end}] has no verification entry; upload shards must carry one \
         per segment (https://huggingface.co/docs/xet/shard#fileverificationentry-optional)"
    )]
    MissingVerification {
        /// The segment's xorb.
        xorb: MerkleHash,
        /// The segment's first chunk index.
        start: u32,
        /// The segment's end chunk index (exclusive).
        end: u32,
    },
    /// The segment's verification hash doesn't match the stored chunks.
    #[error(
        "segment {xorb}[{start}..{end}] verification hash is {actual}, expected {expected} from \
         the xorb's chunks (https://huggingface.co/docs/xet/hashing#term-verification-hashes)"
    )]
    RangeHashMismatch {
        /// The segment's xorb.
        xorb: MerkleHash,
        /// The segment's first chunk index.
        start: u32,
        /// The segment's end chunk index (exclusive).
        end: u32,
        /// The range hash computed from the stored chunk hashes.
        expected: MerkleHash,
        /// The range hash in the shard.
        actual: MerkleHash,
    },
    /// The segment's `unpacked_segment_bytes` doesn't match the stored chunks.
    #[error(
        "segment {xorb}[{start}..{end}] claims {actual} unpacked bytes, but its chunks hold \
         {expected}"
    )]
    LengthMismatch {
        /// The segment's xorb.
        xorb: MerkleHash,
        /// The segment's first chunk index.
        start: u32,
        /// The segment's end chunk index (exclusive).
        end: u32,
        /// The sum of the stored chunk lengths.
        expected: u64,
        /// The length in the shard.
        actual: u32,
    },
    /// A segment names a xorb the server doesn't have.
    #[error(
        "file {file} references xorb {xorb}, which is not stored; upload every xorb before the \
         shard that references it (https://huggingface.co/docs/xet/upload-protocol#ordering-and-concurrency)"
    )]
    UnknownXorb {
        /// The file whose segment names the xorb.
        file: MerkleHash,
        /// The missing xorb.
        xorb: MerkleHash,
    },
    /// The file hash doesn't match the chunks its segments reference.
    #[error(
        "file hash is {actual}, expected {expected} from the chunks its segments reference \
         (https://huggingface.co/docs/xet/hashing#file-hashes)"
    )]
    FileHashMismatch {
        /// The file hash computed from the referenced chunks.
        expected: MerkleHash,
        /// The file hash in the shard.
        actual: MerkleHash,
    },
    /// An empty file (no segments) claims the SHA-256 of non-empty content.
    #[error(
        "empty file {file} claims SHA-256 {actual}; an empty file's SHA-256 must be absent, all \
         zeros, or the digest of zero bytes"
    )]
    EmptyFileSha256 {
        /// The empty file's hash.
        file: MerkleHash,
        /// The claimed SHA-256, as hex of the digest bytes.
        actual: String,
    },
    /// The shard references more chunks than the server verifies per shard.
    #[error(
        "shard references {referenced_chunks} chunks in total, over the limit of {limit}; split \
         the upload into smaller shards"
    )]
    TooMuchWork {
        /// The chunks referenced, summed over every segment of every file.
        referenced_chunks: u64,
        /// The limit applied.
        limit: u64,
    },
    /// The shard references more distinct xorbs than the server verifies per
    /// shard.
    #[error(
        "shard references {referenced_xorbs} distinct xorbs, over the limit of {limit}; split \
         the upload into smaller shards"
    )]
    TooManyXorbs {
        /// The distinct xorbs referenced by the segments.
        referenced_xorbs: usize,
        /// The limit applied.
        limit: usize,
    },
}

/// Parses a shard upload body (`POST /v1/shards`, `/v2/shards`).
///
/// The body must be within [`MAX_UPLOAD_SHARD_BYTES`]; every section is
/// charged against that budget before upstream allocates for it. The spec's
/// upload form ends at the CAS info bookend with `footer_size` 0
/// (<https://huggingface.co/docs/xet/shard#shard-upload>), which is what
/// native clients send. A body that declares a footer, as upstream's full
/// serializer writes it (lookup tables, then the footer), is accepted when the
/// footer's offsets match the sections; the tail is discarded. The wasm build
/// of `xet-data` 1.7.0 uploads that form, and upstream's reference server
/// accepts both.
///
/// # Errors
///
/// [`ProtocolError::MalformedShard`] if the header, a section or the declared
/// footer is invalid, truncated, over the budget, or followed by other bytes.
pub fn read_upload_shard(r: impl Read) -> Result<UploadShard, ProtocolError> {
    parse(&mut CountingReader { inner: r, read: 0 }).map_err(ProtocolError::MalformedShard)
}

fn parse<R: Read>(r: &mut CountingReader<R>) -> Result<UploadShard, CoreError> {
    let expected_header = MDBShardFileHeader::default();
    let header = MDBShardFileHeader::deserialize(r)?;
    if header.version != expected_header.version {
        return Err(CoreError::invalid_shard(format!(
            "shard header version {} is not supported; expected {} \
             (https://huggingface.co/docs/xet/shard#version-compatibility)",
            header.version, expected_header.version
        )));
    }
    let file_info_offset = r.read;
    let mut budget = MAX_UPLOAD_SHARD_BYTES - file_info_offset;

    let mut files = Vec::new();
    budget = process_shard_file_info_section(
        r,
        |view| {
            files.push(file_entry(&view));
            Ok(())
        },
        Some(budget),
    )?
    .unwrap_or(0);

    let xorb_info_offset = r.read;
    let mut xorbs = Vec::new();
    budget = process_shard_xorb_info_section(
        r,
        |view| {
            xorbs.push(view.xorb_hash());
            Ok(())
        },
        Some(budget),
    )?
    .unwrap_or(0);

    let sections_end = r.read;
    let mut tail = Vec::new();
    r.take(budget + 1).read_to_end(&mut tail)?;
    if tail.len() as u64 > budget {
        return Err(CoreError::invalid_shard(format!(
            "shard exceeds {MAX_UPLOAD_SHARD_BYTES} bytes; split it into smaller shards"
        )));
    }
    check_tail(
        &header,
        &tail,
        file_info_offset,
        xorb_info_offset,
        sections_end,
    )?;

    Ok(UploadShard { files, xorbs })
}

/// Checks what follows the CAS info bookend: nothing, or exactly the footer
/// the header declares, with offsets matching the sections just read.
fn check_tail(
    header: &MDBShardFileHeader,
    tail: &[u8],
    file_info_offset: u64,
    xorb_info_offset: u64,
    sections_end: u64,
) -> Result<(), CoreError> {
    if header.footer_size == 0 {
        if tail.is_empty() {
            return Ok(());
        }
        return Err(CoreError::invalid_shard(format!(
            "{} bytes follow the CAS info bookend, but the header declares no footer",
            tail.len()
        )));
    }
    let footer_size = MDBShardFileHeader::default().footer_size;
    let mismatch = |what: &str| {
        CoreError::invalid_shard(format!(
            "the shard footer does not match its sections ({what}); upload shards should omit the \
             footer (https://huggingface.co/docs/xet/shard#shard-upload)"
        ))
    };
    if header.footer_size != footer_size {
        return Err(mismatch("footer size"));
    }
    let footer_start = usize::try_from(footer_size)
        .ok()
        .and_then(|size| tail.len().checked_sub(size))
        .ok_or_else(|| mismatch("footer truncated"))?;
    let footer = MDBShardFileFooter::deserialize(&mut &tail[footer_start..])?;
    if footer.file_info_offset != file_info_offset
        || footer.xorb_info_offset != xorb_info_offset
        || footer.footer_offset != sections_end + footer_start as u64
    {
        return Err(mismatch("offsets"));
    }
    Ok(())
}

fn file_entry(view: &MDBFileInfoView) -> ShardFileEntry {
    let segments = (0..view.num_entries())
        .map(|i| {
            let entry = view.entry(i);
            Segment {
                xorb: entry.xorb_hash,
                chunk_start: entry.chunk_index_start,
                chunk_end: entry.chunk_index_end,
                unpacked_bytes: entry.unpacked_segment_bytes,
                verification: view
                    .contains_verification()
                    .then(|| view.verification(i).range_hash),
            }
        })
        .collect();
    ShardFileEntry {
        file_hash: view.file_hash(),
        segments,
        sha256: view.metadata_ext().map(|ext| sha256_digest(&ext.sha256)),
    }
}

/// The metadata extension stores the SHA-256 digest in the hash layout, so
/// its string form (<https://huggingface.co/docs/xet/api#converting-hashes-to-strings>)
/// is the digest's hex. Verified against the reference CSV's `shasum -a 256`.
fn sha256_digest(stored: &MerkleHash) -> [u8; 32] {
    let hex = stored.hex();
    core::array::from_fn(|i| {
        u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .unwrap_or_else(|_| unreachable!("DataHash::hex is 64 hex digits"))
    })
}

/// Verifies one segment against the chunk hashes and lengths the server
/// stored for its xorb when it accepted the xorb. Never re-reads the xorb.
///
/// Rules, from the spec:
/// - the chunk range `[chunk_start, chunk_end)` is non-empty and inside the
///   xorb (<https://huggingface.co/docs/xet/shard#filedatasequenceentry>)
/// - a verification entry is present
///   (<https://huggingface.co/docs/xet/shard#fileverificationentry-optional>)
///   and equals the range hash of the stored chunk hashes
///   (<https://huggingface.co/docs/xet/hashing#term-verification-hashes>)
/// - `unpacked_segment_bytes` equals the sum of the stored chunk lengths
///   (<https://huggingface.co/docs/xet/shard#filedatasequenceentry>, "term
///   size when unpacked")
///
/// # Errors
///
/// The first rule the segment breaks, as a [`VerifyError`].
pub fn verify_segment(seg: &Segment, xorb_chunks: &[(MerkleHash, u64)]) -> Result<(), VerifyError> {
    verify_segment_with(seg, xorb_chunks, &mut Vec::new()).map(|_| ())
}

/// [`verify_segment`] with a reusable buffer for the range's chunk hashes;
/// returns the segment's chunks.
fn verify_segment_with<'a>(
    seg: &Segment,
    xorb_chunks: &'a [(MerkleHash, u64)],
    hashes: &mut Vec<MerkleHash>,
) -> Result<&'a [(MerkleHash, u64)], VerifyError> {
    let (xorb, start, end) = (seg.xorb, seg.chunk_start, seg.chunk_end);
    let chunks = xorb_chunks
        .get(start as usize..end as usize)
        .filter(|chunks| !chunks.is_empty())
        .ok_or(VerifyError::OutOfBounds {
            xorb,
            start,
            end,
            xorb_chunks: xorb_chunks.len(),
        })?;
    let actual = seg
        .verification
        .ok_or(VerifyError::MissingVerification { xorb, start, end })?;
    hashes.clear();
    hashes.extend(chunks.iter().map(|(hash, _)| *hash));
    let expected = range_hash(hashes);
    if actual != expected {
        return Err(VerifyError::RangeHashMismatch {
            xorb,
            start,
            end,
            expected,
            actual,
        });
    }
    let expected: u64 = chunks.iter().map(|(_, len)| len).sum();
    if expected != u64::from(seg.unpacked_bytes) {
        return Err(VerifyError::LengthMismatch {
            xorb,
            start,
            end,
            expected,
            actual: seg.unpacked_bytes,
        });
    }
    Ok(chunks)
}

/// Verifies an upload shard: the only public entry point for verification,
/// so the work bounds can't be bypassed.
///
/// First the shard must stay within `limits` ([`VerifyLimits::default`] is
/// the default), before any lookup. Then, for every file:
/// - every segment's xorb is stored and the segment passes
///   [`verify_segment`] against its stored chunks
/// - the file hash recomputed from the referenced chunks, in order, equals
///   the claimed one (<https://huggingface.co/docs/xet/hashing#file-hashes>)
/// - a file with no segments is the empty file: its hash must be all zeros
///   (as upstream's `file_hash` of no chunks is), and its SHA-256 absent, all
///   zeros (what `hf_xet` 1.7.0 sends), or the digest of zero bytes
///
/// The CAS info section ([`UploadShard::xorbs`]) isn't checked: only
/// referenced xorbs must be stored.
///
/// Memory: the caller holds the chunk table of every distinct referenced
/// xorb, which `max_referenced_xorbs` bounds (see [`MAX_REFERENCED_XORBS`]).
/// Verification itself adds one buffer of at most one xorb's chunk hashes
/// and, per file, an O(log n) file-hash state: the file hash is computed
/// incrementally with upstream's `MerkleHashSubtree` in release builds
/// (debug builds keep every chunk to self-check upstream's merge). CPU is
/// linear in the referenced chunks, which `max_referenced_chunks` bounds.
///
/// # Errors
///
/// [`VerifyError::TooMuchWork`] or [`VerifyError::TooManyXorbs`] if the shard
/// is over a limit, else the first rule a file breaks.
pub fn verify_shard(
    shard: &UploadShard,
    lookup: &impl XorbChunkLookup,
    limits: VerifyLimits,
) -> Result<(), VerifyError> {
    // Each term is at most u32::MAX chunks and a 64 MiB shard holds well under
    // 2^32 terms, so the sum fits in u64; saturate anyway rather than trust it.
    let referenced_chunks = shard
        .files
        .iter()
        .flat_map(|file| &file.segments)
        .map(|seg| u64::from(seg.chunk_end.saturating_sub(seg.chunk_start)))
        .fold(0u64, u64::saturating_add);
    if referenced_chunks > limits.max_referenced_chunks {
        return Err(VerifyError::TooMuchWork {
            referenced_chunks,
            limit: limits.max_referenced_chunks,
        });
    }
    let referenced_xorbs = shard.referenced_xorbs().count();
    if referenced_xorbs > limits.max_referenced_xorbs {
        return Err(VerifyError::TooManyXorbs {
            referenced_xorbs,
            limit: limits.max_referenced_xorbs,
        });
    }
    shard
        .files
        .iter()
        .try_for_each(|file| verify_file(file, lookup))
}

/// One file's rules, as listed on [`verify_shard`]; callers go through
/// that function so the work bounds apply.
fn verify_file(entry: &ShardFileEntry, lookup: &impl XorbChunkLookup) -> Result<(), VerifyError> {
    let mut hashes = Vec::new();
    let mut tree: Option<MerkleHashSubtree> = None;
    let last = entry.segments.len().saturating_sub(1);
    for (i, seg) in entry.segments.iter().enumerate() {
        let xorb = lookup.chunks(&seg.xorb).ok_or(VerifyError::UnknownXorb {
            file: entry.file_hash,
            xorb: seg.xorb,
        })?;
        let chunks = verify_segment_with(seg, &xorb, &mut hashes)?;
        let part = MerkleHashSubtree::from_chunks(i == 0, chunks, i == last);
        match &mut tree {
            None => tree = Some(part),
            Some(tree) => tree.merge_into(&part).unwrap_or_else(|_| {
                unreachable!("only the first part is at_start and only the last at_end")
            }),
        }
    }
    let expected = if let Some(tree) = tree {
        let aggregated = tree
            .final_hash()
            .unwrap_or_else(|| unreachable!("the first part is at_start and the last at_end"));
        // hashing#file-hashes: the aggregated hash, keyed with 32 zero bytes;
        // `merklehash::file_hash` does the same over a chunk list.
        aggregated.hmac([0; 32].into())
    } else {
        check_empty_file_sha256(entry)?;
        MerkleHash::default()
    };
    if expected != entry.file_hash {
        return Err(VerifyError::FileHashMismatch {
            expected,
            actual: entry.file_hash,
        });
    }
    Ok(())
}

fn check_empty_file_sha256(entry: &ShardFileEntry) -> Result<(), VerifyError> {
    match entry.sha256 {
        None => Ok(()),
        Some(sha) if sha == [0; 32] || sha == EMPTY_SHA256 => Ok(()),
        Some(sha) => Err(VerifyError::EmptyFileSha256 {
            file: entry.file_hash,
            actual: sha.iter().fold(String::with_capacity(64), |mut hex, b| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
                hex
            }),
        }),
    }
}

/// Counts bytes read, so the footer's offsets can be checked against where
/// the sections actually were.
struct CountingReader<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}
