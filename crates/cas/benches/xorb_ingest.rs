//! Xorb ingest benchmarks, for humans. Results and the machine they were
//! taken on are in `docs/reference/benchmarks.md`.
//!
//! On a 64 MiB xorb of LZ4 chunks, as `hf_xet` uploads them:
//! - `xorb_ingest/xorb_with_footer`: what `POST /v1/xorbs` runs under a
//!   `validate` permit: openxet's validation pass, which decompresses and
//!   hashes every chunk, then upstream's `reconstruct_xorb_with_footer`,
//!   which decompresses and hashes them again to build the footer (ruling
//!   M1-R8).
//! - `xorb_ingest/validation_pass`: `read_xorb_chunks`, that first pass
//!   alone. The difference between the two is the second pass's cost.
//!
//! Run with `cargo bench -p openxet-cas --bench xorb_ingest`.

#![allow(clippy::unwrap_used, missing_docs)]

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use openxet_protocol::{MAX_XORB_BYTES, lz4_xorb_body, read_xorb_chunks, xorb_with_footer};

const CHUNK_BYTES: usize = 64 * 1024;

/// Text-like data LZ4 compresses about as it does source code: words
/// drawn from a small vocabulary by xorshift64*, fixed seed.
fn text(len: usize) -> Vec<u8> {
    const WORDS: &[&[u8]] = &[
        b"fn ", b"let ", b"mut ", b"self", b".await", b"Result", b"<u64>", b"(", b")", b"{\n",
        b"}\n", b"    ", b"xorb", b"chunk", b"hash", b"range", b"::", b"; ", b"// ", b"=> ",
        b"Some", b"None", b"0x", b"impl ", b"for ", b"in ", b"if ", b"else ", b"return ",
        b"String", b"Vec", b"&",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let r = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        out.extend_from_slice(WORDS[(r >> 59) as usize]);
        if r & 0xff < 8 {
            // A sprinkle of noise, so chunks differ and don't compress away.
            out.extend_from_slice(&r.to_le_bytes()[..4]);
        }
    }
    out.truncate(len);
    out
}

fn ingest(c: &mut Criterion) {
    let data = text(usize::try_from(MAX_XORB_BYTES).unwrap());
    let chunks: Vec<&[u8]> = data.chunks(CHUNK_BYTES).collect();
    let body = lz4_xorb_body(&chunks);
    eprintln!(
        "xorb_ingest: {} chunks, {} bytes uncompressed, {} bytes uploaded",
        chunks.len(),
        data.len(),
        body.len()
    );
    let mut group = c.benchmark_group("xorb_ingest");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("xorb_with_footer", |b| {
        b.iter(|| black_box(xorb_with_footer(&body).unwrap()));
    });
    group.bench_function("validation_pass", |b| {
        b.iter(|| black_box(read_xorb_chunks(&body[..]).unwrap()));
    });
    group.finish();
}

criterion_group!(benches, ingest);
criterion_main!(benches);
