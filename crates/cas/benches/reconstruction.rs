//! Reconstruction benchmarks, for humans: the latency gate
//! (`tests/latency_gate.rs`) is what CI enforces. Results and the machine
//! they were taken on are in `docs/reference/benchmarks.md`.
//!
//! - `reconstruction/{shape}/{terms}`: one full-file
//!   `GET /v2/reconstructions` in process, through the router and a SQLite
//!   index, for files of 1k, 10k and 100k terms in both shapes of
//!   `tests/fixture/large_files.rs`. It covers loading the record and
//!   chunk layouts, planning, signing and writing the answer.
//! - `sign/100k_urls`: signing 100k transfer URLs over 1,000 xorbs, the
//!   largest part of the unmerged 100k-term answer.
//!
//! Run with `cargo bench -p openxet-cas --bench reconstruction`.

#![allow(clippy::unwrap_used, missing_docs)]

#[allow(dead_code)]
#[path = "../tests/fixture/large_files.rs"]
mod large_files;

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use openxet_auth::UrlSigner;
use openxet_protocol::chunk_hash;
use url::Url;

fn reconstruction(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("reconstruction");
    group.sample_size(20);
    for terms in [1_000, 10_000, 100_000] {
        let fixture = runtime.block_on(large_files::fixture(terms));
        for (shape, record) in &fixture.files {
            group.throughput(Throughput::Elements(u64::from(terms)));
            group.bench_with_input(
                BenchmarkId::new(shape.name(), terms),
                record,
                |b, record| {
                    b.to_async(&runtime).iter(|| async {
                        black_box(large_files::get(&fixture.state, &record.hash, None).await)
                    });
                },
            );
        }
    }
    group.finish();
}

fn sign(c: &mut Criterion) {
    let signer = UrlSigner::new([7; 32], UrlSigner::DEFAULT_LIFETIME).unwrap();
    let base = Url::parse("https://cas.example.com").unwrap();
    let xorbs: Vec<_> = (0..1000u32).map(|i| chunk_hash(&i.to_le_bytes())).collect();
    let mut group = c.benchmark_group("sign");
    group.throughput(Throughput::Elements(100_000));
    group.bench_function("100k_urls", |b| {
        b.iter(|| {
            let urls = signer.transfer_urls(&base, 1_800_000_000);
            let mut out = String::with_capacity(100_000 * 160);
            for (x, xorb) in xorbs.iter().enumerate() {
                let xorb = urls.xorb(xorb);
                for k in 0..100u64 {
                    let start = (k * 1000 + x as u64) * 65_536;
                    xorb.push_signed(start, start + 65_535, &mut out).unwrap();
                }
            }
            black_box(out)
        });
    });
    group.finish();
}

criterion_group!(benches, reconstruction, sign);
criterion_main!(benches);
