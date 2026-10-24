//! Xorbs: <https://huggingface.co/docs/xet/xorb>.

use std::io::{self, Read, Write};

use xet_core_structures::CoreError;
use xet_core_structures::merklehash;
use xet_core_structures::serialization_utils::read_u32;
use xet_core_structures::xorb_object::{
    CompressionScheme, XORB_CHUNK_HEADER_LENGTH, XorbObjectInfoV1, deserialize_chunk_header,
    reconstruct_xorb_with_footer,
};

use crate::{MerkleHash, ProtocolError, chunk_hash};

/// The largest uncompressed chunk, in bytes
/// (<https://huggingface.co/docs/xet/chunking>, `MAX_CHUNK_SIZE`).
pub const MAX_CHUNK_BYTES: u64 = 128 * 1024;

/// The most chunks one uploaded xorb may hold. The spec sets no count
/// (<https://huggingface.co/docs/xet/xorb#collecting-chunks>); this is the
/// count `xet-core` 1.7.0 clients stop at (`MAX_XORB_CHUNKS`), so stock
/// uploads always fit, and it bounds the per-xorb chunk table at 8192 entries.
pub const MAX_XORB_CHUNKS: usize = 8192;

/// The spec's 64 MiB xorb limit
/// (<https://huggingface.co/docs/xet/xorb#collecting-chunks>), on serialized
/// size. It sets [`MAX_XORB_BODY_BYTES`]; no separate total is enforced on
/// decompressed bytes, which [`MAX_XORB_CHUNKS`] × [`MAX_CHUNK_BYTES`] bound
/// at 1 GiB.
pub const MAX_XORB_BYTES: u64 = 64 * 1024 * 1024;

/// The largest xorb upload body accepted, in bytes: [`MAX_XORB_BYTES`] plus
/// framing. Clients that store chunks uncompressed add an 8-byte header per
/// chunk and may append a footer of about 40 bytes per chunk; for
/// [`MAX_XORB_CHUNKS`] chunks that is under 400 KiB, so 512 KiB of framing
/// admits every stock upload.
pub const MAX_XORB_BODY_BYTES: u64 = MAX_XORB_BYTES + 512 * 1024;

/// The chunks of a xorb, in xorb order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XorbChunks {
    /// `(chunk hash, uncompressed length)` per chunk.
    pub hashes: Vec<(MerkleHash, u64)>,
}

/// Reads a footer-less xorb (the spec's chunk sequence) and hashes every
/// chunk, under the same limits as [`xorb_with_footer`].
///
/// Input must end exactly after the last chunk: a xorb footer is rejected
/// here. Uploads, which may carry a footer, go through [`xorb_with_footer`].
///
/// # Errors
///
/// The [`ProtocolError`] `Xorb*` variant naming the rule broken
/// ([`ProtocolError::XorbTrailingBytes`] for a footer or any other bytes
/// after the last chunk), or [`ProtocolError::MalformedXorb`] if a chunk
/// header or a chunk's compressed data is invalid.
pub fn read_xorb_chunks(r: impl Read) -> Result<XorbChunks, ProtocolError> {
    let mut body = Vec::new();
    r.take(MAX_XORB_BODY_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|err| ProtocolError::MalformedXorb(CoreError::Io(err)))?;
    let len = body.len() as u64;
    if len > MAX_XORB_BODY_BYTES {
        return Err(ProtocolError::XorbTooLarge {
            len,
            limit: MAX_XORB_BODY_BYTES,
        });
    }
    let ValidChunks {
        hashes, chunks_end, ..
    } = validate_chunks(&body)?;
    if chunks_end < body.len() {
        return Err(ProtocolError::XorbTrailingBytes {
            offset: chunks_end as u64,
            len: (body.len() - chunks_end) as u64,
        });
    }
    if hashes.is_empty() {
        return Err(ProtocolError::XorbEmpty);
    }
    Ok(XorbChunks { hashes })
}

/// A validated xorb upload in its stored form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredXorb {
    /// The xorb hash computed from the chunks.
    pub hash: MerkleHash,
    /// The chunk table the server records for shard verification.
    pub chunks: XorbChunks,
    /// The serialized chunks followed by a footer computed from them.
    pub bytes: Vec<u8>,
    /// Where each chunk starts in [`StoredXorb::bytes`]: `n + 1` entries for
    /// `n` chunks, entry `i` the offset of chunk `i`'s header and the last the
    /// end of the chunk section, where the footer starts. Chunks `[a, b)` are
    /// `bytes[chunk_offsets[a]..chunk_offsets[b]]`, the end-inclusive HTTP
    /// range `chunk_offsets[a]..=chunk_offsets[b] - 1`.
    pub chunk_offsets: Vec<u32>,
}

/// Validates an uploaded xorb body (`POST /v1/xorbs`) and builds its stored
/// form.
///
/// The body is a chunk sequence, optionally followed by one footer, the form
/// upstream's in-process test backend also accepts (`xet-client` 1.7.0,
/// `cas_client/simulation/local_client.rs`). Each chunk is checked against
/// the spec (<https://huggingface.co/docs/xet/xorb#chunk-header-structure>)
/// and openxet's limits before anything is kept:
/// - the body is at most [`MAX_XORB_BODY_BYTES`] and holds 1 to
///   [`MAX_XORB_CHUNKS`] chunks
/// - every chunk decompresses to exactly its header's uncompressed length,
///   at most [`MAX_CHUNK_BYTES`]; decompression stops at that length, so a
///   chunk can't make the server inflate more than it declares
/// - after the last chunk comes nothing, or exactly one footer, which must
///   parse and is then discarded
///
/// The stored bytes are the chunk section with a footer recomputed by
/// upstream's serializer. The caller compares [`StoredXorb::hash`] with the
/// hash in the request path.
///
/// # Errors
///
/// The [`ProtocolError`] `Xorb*` variant naming the rule broken, or
/// [`ProtocolError::MalformedXorb`] if a chunk header, a chunk's compressed
/// data or the footer is invalid.
pub fn xorb_with_footer(body: &[u8]) -> Result<StoredXorb, ProtocolError> {
    let len = body.len() as u64;
    if len > MAX_XORB_BODY_BYTES {
        return Err(ProtocolError::XorbTooLarge {
            len,
            limit: MAX_XORB_BODY_BYTES,
        });
    }
    let ValidChunks {
        hashes,
        offsets: chunk_offsets,
        chunks_end,
    } = validate_chunks(body)?;
    if hashes.is_empty() {
        return Err(ProtocolError::XorbEmpty);
    }
    let mut bytes = Vec::with_capacity(body.len());
    // Only reached with chunks that passed every check above, so upstream
    // decompresses exactly what was validated.
    let (_, hash) = reconstruct_xorb_with_footer(&mut bytes, &body[..chunks_end])
        .map_err(ProtocolError::MalformedXorb)?;
    debug_assert_eq!(hash, merklehash::xorb_hash(&hashes));
    Ok(StoredXorb {
        hash,
        chunks: XorbChunks { hashes },
        bytes,
        chunk_offsets,
    })
}

/// The result of walking a xorb body's chunk section.
struct ValidChunks {
    /// `(chunk hash, uncompressed length)` per chunk.
    hashes: Vec<(MerkleHash, u64)>,
    /// The offset of each chunk's header, then `chunks_end`.
    offsets: Vec<u32>,
    /// Where the chunk section ends; a footer, if any, starts there.
    chunks_end: usize,
}

/// Walks and validates the chunks of `body`.
fn validate_chunks(body: &[u8]) -> Result<ValidChunks, ProtocolError> {
    let mut hashes = Vec::new();
    let mut offsets = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        let mut rest = &body[pos..];
        let header = match deserialize_chunk_header(&mut rest) {
            Ok(header) => header,
            // The footer ident where a header would be: the chunk section ends.
            Err(CoreError::ChunkHeaderParse) => {
                check_footer(body, pos)?;
                break;
            }
            Err(err) => return Err(ProtocolError::MalformedXorb(err)),
        };
        let chunk = hashes.len();
        if chunk == MAX_XORB_CHUNKS {
            return Err(ProtocolError::XorbTooManyChunks {
                limit: MAX_XORB_CHUNKS,
            });
        }
        if header.get_uncompressed_length() == 0 {
            return Err(ProtocolError::XorbChunkEmpty { chunk });
        }
        let compressed_len = header.get_compressed_length() as usize;
        let compressed = rest
            .get(..compressed_len)
            .ok_or(ProtocolError::XorbTruncated { chunk })?;
        let data = decompress_chunk(
            chunk,
            header
                .get_compression_scheme()
                .map_err(ProtocolError::MalformedXorb)?,
            compressed,
            header.get_uncompressed_length(),
        )?;
        hashes.push((chunk_hash(&data), data.len() as u64));
        offsets.push(offset(pos));
        pos += XORB_CHUNK_HEADER_LENGTH + compressed_len;
    }
    offsets.push(offset(pos));
    Ok(ValidChunks {
        hashes,
        offsets,
        chunks_end: pos,
    })
}

const _: () = assert!(MAX_XORB_BODY_BYTES <= u32::MAX as u64);

/// A position inside a body of at most [`MAX_XORB_BODY_BYTES`], which fits
/// in `u32`.
fn offset(pos: usize) -> u32 {
    u32::try_from(pos).unwrap_or_else(|_| unreachable!("xorb bodies are under 4 GiB"))
}

/// Decompresses one chunk into a buffer bounded by its declared length (and
/// [`MAX_CHUNK_BYTES`]), and requires the declared length exactly.
fn decompress_chunk(
    chunk: usize,
    scheme: CompressionScheme,
    compressed: &[u8],
    declared: u32,
) -> Result<Vec<u8>, ProtocolError> {
    let limit = u64::from(declared).min(MAX_CHUNK_BYTES);
    let bounded = |scheme: CompressionScheme| {
        let mut out = BoundedWriter::new(limit);
        let result = scheme.decompress_from_reader(&mut &compressed[..], &mut out);
        if out.overflowed {
            return Err(ProtocolError::XorbChunkTooLong { chunk, limit });
        }
        result.map_err(ProtocolError::MalformedXorb)?;
        Ok(out.buf)
    };
    let data = match scheme {
        // A BG4 chunk is an LZ4 stream of its regrouped bytes, which have the
        // chunk's length (xorb#byte-grouping-lz4-compression). The BG4 decode
        // regroups only a complete stream, so a bounded LZ4 pass over the
        // same bytes enforces the limit first.
        CompressionScheme::ByteGrouping4LZ4 => {
            let lz4_len = bounded(CompressionScheme::LZ4)?.len();
            if lz4_len as u64 != u64::from(declared) {
                return Err(ProtocolError::XorbChunkTooShort {
                    chunk,
                    declared,
                    actual: lz4_len as u64,
                });
            }
            bounded(scheme)?
        }
        _ => bounded(scheme)?,
    };
    if data.len() as u64 != u64::from(declared) {
        return Err(ProtocolError::XorbChunkTooShort {
            chunk,
            declared,
            actual: data.len() as u64,
        });
    }
    Ok(data)
}

/// Requires `body[start..]` to be exactly one xorb footer: the info block,
/// parsed forward by upstream, then its length as a little-endian u32.
fn check_footer(body: &[u8], start: usize) -> Result<(), ProtocolError> {
    let mut rest = &body[start..];
    let (_, info_len) =
        XorbObjectInfoV1::deserialize(&mut rest).map_err(ProtocolError::MalformedXorb)?;
    let stored_len =
        read_u32(&mut rest).map_err(|err| ProtocolError::MalformedXorb(CoreError::Io(err)))?;
    if stored_len != info_len {
        return Err(ProtocolError::MalformedXorb(CoreError::MalformedData(
            format!(
                "xorb footer records an info length of {stored_len} bytes, but its info block is \
             {info_len}"
            ),
        )));
    }
    if !rest.is_empty() {
        return Err(ProtocolError::XorbTrailingBytes {
            offset: (body.len() - rest.len()) as u64,
            len: rest.len() as u64,
        });
    }
    Ok(())
}

/// A writer that keeps at most `limit` bytes and fails on the first byte
/// past it, so a decompressor writing into it stops there.
struct BoundedWriter {
    buf: Vec<u8>,
    limit: usize,
    overflowed: bool,
}

impl BoundedWriter {
    fn new(limit: u64) -> Self {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        Self {
            buf: Vec::with_capacity(limit),
            limit,
            overflowed: false,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let room = self.limit - self.buf.len();
        if room == 0 && !data.is_empty() {
            self.overflowed = true;
            return Err(io::Error::other("chunk exceeds its declared length"));
        }
        let n = data.len().min(room);
        self.buf.extend_from_slice(&data[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The xorb hash over a xorb's chunks.
#[must_use]
pub fn xorb_hash(chunks: &XorbChunks) -> MerkleHash {
    merklehash::xorb_hash(&chunks.hashes)
}

/// Decodes `bytes`, one fetched xorb range, as `hf_xet` 1.7.0 decodes a
/// data URL's body: upstream's `deserialize_chunks` over the chunk headers
/// and payloads (`xet-client` 1.7.0, `remote_client.rs`). Returns the
/// chunks' uncompressed bytes, concatenated, and where each chunk starts in
/// them: `n + 1` offsets for `n` chunks, starting at `0`.
///
/// # Errors
///
/// [`ProtocolError::MalformedXorb`] if upstream refuses `bytes`.
#[cfg(feature = "test-support")]
pub fn decode_fetched_chunks(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u32>), ProtocolError> {
    xet_core_structures::xorb_object::deserialize_chunks(&mut &bytes[..])
        .map_err(ProtocolError::MalformedXorb)
}
