//! Upload shards: <https://huggingface.co/docs/xet/shard#shard-upload>.
//!
//! Parsing goes through `xet-core-structures`' section parsers; the checks
//! here are the server-side rules openxet applies before registering files.
//! `docs/research/S6-shard-verification.md` lists each rule, its source and
//! its test.

use std::io::{self, Read};

use xet_core_structures::CoreError;
use xet_core_structures::metadata_shard::file_structs::MDBFileInfoView;
use xet_core_structures::metadata_shard::streaming_shard::{
    process_shard_file_info_section, process_shard_xorb_info_section,
};
use xet_core_structures::metadata_shard::{MDBShardFileFooter, MDBShardFileHeader};

use crate::{MerkleHash, ProtocolError, file_hash, range_hash};

/// The largest shard upload accepted, in bytes. Clients must split larger
/// shards (<https://huggingface.co/docs/xet/upload-protocol#6-shard-serialization-and-upload>).
pub const MAX_UPLOAD_SHARD_BYTES: u64 = 64 * 1024 * 1024;

/// A parsed upload shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadShard {
    /// The file info section, in shard order.
    pub files: Vec<ShardFileEntry>,
    /// The xorb hashes listed in the CAS info section, in shard order. The
    /// client's per-chunk listings are dropped: the server's own chunk hashes,
    /// recorded when it accepted each xorb, are the ones it trusts.
    pub xorbs: Vec<MerkleHash>,
}

/// One file's reconstruction as uploaded in a shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardFileEntry {
    /// The file hash the client claims; [`verify_file`] recomputes it.
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
    let hashes: Vec<MerkleHash> = chunks.iter().map(|(hash, _)| *hash).collect();
    let expected = range_hash(&hashes);
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
    Ok(())
}

/// Verifies a file entry: every segment passes [`verify_segment`] against its
/// xorb's stored chunks, and the file hash recomputed from the referenced
/// chunks, in order, equals the claimed one
/// (<https://huggingface.co/docs/xet/hashing#file-hashes>). A file with no
/// segments is the empty file, whose hash upstream defines as all zeros.
///
/// `xorb_chunks` returns the stored `(chunk hash, length)` list for a xorb,
/// or `None` if the xorb isn't stored.
///
/// # Errors
///
/// The first rule the file breaks, as a [`VerifyError`].
pub fn verify_file<'a>(
    entry: &ShardFileEntry,
    xorb_chunks: impl Fn(&MerkleHash) -> Option<&'a [(MerkleHash, u64)]>,
) -> Result<(), VerifyError> {
    let mut referenced = Vec::new();
    for seg in &entry.segments {
        let chunks = xorb_chunks(&seg.xorb).ok_or(VerifyError::UnknownXorb {
            file: entry.file_hash,
            xorb: seg.xorb,
        })?;
        verify_segment(seg, chunks)?;
        referenced.extend_from_slice(&chunks[seg.chunk_start as usize..seg.chunk_end as usize]);
    }
    let expected = file_hash(&referenced);
    if expected != entry.file_hash {
        return Err(VerifyError::FileHashMismatch {
            expected,
            actual: entry.file_hash,
        });
    }
    Ok(())
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
