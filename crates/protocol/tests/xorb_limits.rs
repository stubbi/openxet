//! Xorb upload validation: decompressed lengths, chunk limits and framing
//! (<https://huggingface.co/docs/xet/xorb>). Bodies are built from
//! upstream's chunk serializers, then altered where a test needs a header
//! that disagrees with its data.

#![allow(clippy::unwrap_used)]

use openxet_protocol::{
    MAX_CHUNK_BYTES, MAX_XORB_BODY_BYTES, MAX_XORB_BYTES, MAX_XORB_CHUNKS, ProtocolError,
    chunk_hash, xorb_with_footer,
};
// Only to build test bodies; production code reaches upstream through
// openxet_protocol alone.
use xet_core_structures::xorb_object::{
    CompressionScheme, XorbChunkHeader, bg4_lz4_compress_from_slice, lz4_compress_from_slice,
    reconstruct_xorb_with_footer, serialize_chunk, write_chunk_header,
};

/// A chunk whose header declares `declared` uncompressed bytes, followed by
/// `compressed` as stored.
fn raw_chunk(scheme: CompressionScheme, compressed: &[u8], declared: u32) -> Vec<u8> {
    let header = XorbChunkHeader::new(scheme, u32::try_from(compressed.len()).unwrap(), declared);
    let mut out = Vec::new();
    write_chunk_header(&mut out, &header).unwrap();
    out.extend_from_slice(compressed);
    out
}

fn chunk(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_chunk(data, &mut out, CompressionScheme::LZ4).unwrap();
    out
}

#[test]
fn valid_chunks_yield_their_table_and_hash() {
    let (a, b) = (vec![1u8; 1000], vec![2u8; 3000]);
    let body = [chunk(&a), chunk(&b)].concat();
    let stored = xorb_with_footer(&body).unwrap();
    assert_eq!(
        stored.chunks.hashes,
        [(chunk_hash(&a), 1000), (chunk_hash(&b), 3000)]
    );
    assert!(stored.bytes.starts_with(&body));
    assert!(stored.bytes.len() > body.len());
}

#[test]
fn chunk_shorter_than_its_header_is_rejected() {
    let body = raw_chunk(CompressionScheme::None, &[7; 100], 200);
    let err = xorb_with_footer(&body).unwrap_err();
    assert!(
        matches!(
            err,
            ProtocolError::XorbChunkTooShort {
                chunk: 0,
                declared: 200,
                actual: 100
            }
        ),
        "{err}"
    );
}

#[test]
fn chunk_longer_than_its_header_is_rejected() {
    let body = [
        chunk(&[1; 10]),
        raw_chunk(CompressionScheme::None, &[7; 100], 50),
    ]
    .concat();
    let err = xorb_with_footer(&body).unwrap_err();
    assert!(
        matches!(
            err,
            ProtocolError::XorbChunkTooLong {
                chunk: 1,
                limit: 50
            }
        ),
        "{err}"
    );
}

#[test]
fn chunk_over_the_maximum_chunk_size_is_rejected() {
    // 200 KiB stored uncompressed, with a header claiming the 128 KiB maximum.
    let data = vec![7u8; 200 * 1024];
    let body = raw_chunk(
        CompressionScheme::None,
        &data,
        u32::try_from(MAX_CHUNK_BYTES).unwrap(),
    );
    let err = xorb_with_footer(&body).unwrap_err();
    assert!(
        matches!(err, ProtocolError::XorbChunkTooLong { chunk: 0, limit } if limit == MAX_CHUNK_BYTES),
        "{err}"
    );
    assert_eq!(MAX_CHUNK_BYTES, 128 * 1024);
}

#[test]
fn decompression_bomb_stops_at_the_declared_length() {
    // 48 MiB of zeros compresses to about 200 KiB; the header claims 64 KiB.
    // Both LZ4 schemes must stop at the declared length rather than inflate
    // the whole stream.
    let zeros = vec![0u8; 48 * 1024 * 1024];
    for (scheme, compressed) in [
        (
            CompressionScheme::LZ4,
            lz4_compress_from_slice(&zeros).unwrap(),
        ),
        (
            CompressionScheme::ByteGrouping4LZ4,
            bg4_lz4_compress_from_slice(&zeros).unwrap(),
        ),
    ] {
        assert!(compressed.len() < 2 * 128 * 1024, "{scheme}");
        let body = raw_chunk(scheme, &compressed, 64 * 1024);
        let err = xorb_with_footer(&body).unwrap_err();
        assert!(
            matches!(
                err,
                ProtocolError::XorbChunkTooLong {
                    chunk: 0,
                    limit: 65_536
                }
            ),
            "{scheme}: {err}"
        );
    }
}

#[test]
fn empty_body_is_rejected() {
    let err = xorb_with_footer(&[]).unwrap_err();
    assert!(matches!(err, ProtocolError::XorbEmpty), "{err}");
}

#[test]
fn footer_without_chunks_is_rejected() {
    let mut footer_only = Vec::new();
    reconstruct_xorb_with_footer(&mut footer_only, &[]).unwrap();
    let err = xorb_with_footer(&footer_only).unwrap_err();
    assert!(matches!(err, ProtocolError::XorbEmpty), "{err}");
}

#[test]
fn chunk_count_at_the_limit_passes_and_over_it_is_rejected() {
    let one = chunk(&[1]);
    let at_limit = one.repeat(MAX_XORB_CHUNKS);
    assert_eq!(
        xorb_with_footer(&at_limit).unwrap().chunks.hashes.len(),
        MAX_XORB_CHUNKS
    );
    let over = one.repeat(MAX_XORB_CHUNKS + 1);
    let err = xorb_with_footer(&over).unwrap_err();
    assert!(
        matches!(err, ProtocolError::XorbTooManyChunks { limit: 8192 }),
        "{err}"
    );
}

#[test]
fn body_over_the_size_limit_is_rejected_before_parsing() {
    let body = vec![0u8; usize::try_from(MAX_XORB_BODY_BYTES).unwrap() + 1];
    let err = xorb_with_footer(&body).unwrap_err();
    assert!(
        matches!(
            err,
            ProtocolError::XorbTooLarge { len, limit }
                if len == MAX_XORB_BODY_BYTES + 1 && limit == MAX_XORB_BODY_BYTES
        ),
        "{err}"
    );
}

#[test]
fn body_limit_covers_a_full_xorb_and_its_framing() {
    // The most framing a valid upload carries: a header per chunk for the
    // most chunks, and the footer upstream writes for them.
    let chunks = chunk(&[1]).repeat(MAX_XORB_CHUNKS);
    let mut stored = Vec::new();
    reconstruct_xorb_with_footer(&mut stored, &chunks).unwrap();
    let footer = (stored.len() - chunks.len()) as u64;
    let headers = MAX_XORB_CHUNKS as u64 * 8;
    assert_eq!(MAX_XORB_BYTES, 64 * 1024 * 1024);
    assert!(MAX_XORB_BODY_BYTES >= MAX_XORB_BYTES + headers + footer);
}

#[test]
fn bytes_after_a_footer_are_rejected() {
    let body = [chunk(&[1; 10]), chunk(&[2; 20])].concat();
    let mut stored = xorb_with_footer(&body).unwrap().bytes;
    let footer_end = stored.len() as u64;
    stored.extend_from_slice(&[0, 0]);
    let err = xorb_with_footer(&stored).unwrap_err();
    assert!(
        matches!(err, ProtocolError::XorbTrailingBytes { offset, len: 2 } if offset == footer_end),
        "{err}"
    );
}

#[test]
fn malformed_footer_is_rejected() {
    let body = [chunk(&[1; 10]), chunk(&[2; 20])].concat();
    let stored = xorb_with_footer(&body).unwrap().bytes;
    // The footer ident followed by too few bytes for a footer.
    let cut = [&body[..], &stored[body.len()..body.len() + 16]].concat();
    let err = xorb_with_footer(&cut).unwrap_err();
    assert!(matches!(err, ProtocolError::MalformedXorb(_)), "{err}");
}
