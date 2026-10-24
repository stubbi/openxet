# Benchmarks

What openxet measures about its own speed, how to rerun it, and the last
results. AGENTS.md's rule holds here: a performance claim comes with a
benchmark, and a speed regression fails CI.

| What | Kind | Command |
|---|---|---|
| Reconstruction latency, design §5.8 | CI gate (Linux runner); locally, needs a quiet machine | `cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)' --no-capture` |
| Reconstruction and URL signing | criterion, for humans | `cargo bench -p openxet-cas --bench reconstruction` |
| Xorb ingest | criterion, for humans | `cargo bench -p openxet-cas --bench xorb_ingest` |
| Shard and index commit throughput | a plain run, for humans | `cargo bench -p openxet-cas --bench commit_throughput` |

Criterion keeps its last run under `target/criterion` and reports the
change on the next one. The benches are not CI gates: shared runners vary
too much for criterion's statistics to fail a build on.

## Machines

- **Local:** Apple M3 (4 performance and 4 efficiency cores), 16 GB,
  macOS 15.6.1, rustc 1.97.1, release profile. The host was shared with
  other builds during every run here (load average 4 to 27 on 8 cores), so
  local numbers are noisy, tails most of all. Where a number moved only
  because of load, the text says so.
- **CI:** GitHub's `ubuntu-latest` (4 vCPU, an AMD EPYC 7763 or 9V45)
  and `macos-latest` (3 vCPU Apple M1 VM) runners, release profile, from
  the `rust` job's log.

## Reconstruction latency gate

`crates/cas/tests/latency_gate.rs` asks for p99 < 50 ms (design §5.8) and
pins the file shape, so it can't pass on an easy file. Both files have
100,000 terms over 1,000 xorbs of 1,024 chunks
(`crates/cas/tests/fixture/large_files.rs`):

- **unmerged**, the worst case: consecutive terms name different xorbs, so
  every term has its own fetch descriptor and a full answer signs 100,000
  URLs in a 42.3 MB body
- **merged**: each xorb's 100 terms are adjacent runs, so a full answer
  has 1,000 descriptors in a 13.6 MB body

Each shape gets 200 full-file and 200 random-range
`GET /v2/reconstructions`, one at a time, in process through the router
and a SQLite index on disk, timed to the body's last byte. The index uses
per-scope visibility, the costlier check. The gate runs on the Linux
runner only, as the other release-build gate does.

p50 / p99 in ms:

<!-- table-caption: Reconstruction latency by implementation stage (p50 / p99, ms) -->

| Shape | Start (local) | Before threads (CI Linux) | All writers (CI Linux) | All writers (CI macOS) | Half the free permits (CI Linux, slow runner) | Now (local) | Now (CI Linux, 10 runs) |
|---|---|---|---|---|---|---|---|
| unmerged full | 246 / 679 | 66.2 / 73.6 | 24.8 / 28.2 | 57.5 / 90.3 | 43.7 / 53.1 | 17.4 / 20.4 | **14.4–26.8 / 16.4–29.6** |
| unmerged range | 74 / 486 | 25.3 / 59.8 | 14.7 / 23.8 | 31.1 / 70.3 | 21.5 / 40.3 | 7.6 / 15.4 | **5.4–9.2 / 12.3–22.6** |
| merged full | 50 / 93 | 19.7 / 25.9 | 13.2 / 15.7 | 23.7 / 32.8 | 19.3 / 26.2 | 6.5 / 8.6 | **4.0–7.8 / 4.3–8.8** |
| merged range | 14 / 42 | 7.7 / 18.7 | 5.2 / 12.6 | 10.1 / 23.0 | 7.9 / 19.6 | 4.2 / 7.0 | **2.6–4.4 / 3.9–7.0** |

- "Start" is the gate against the code before this work.
- "Before threads" is CI with every change up to the writer threads, so
  one writer.
- "All writers" let an answer take every free `validate` permit.
- "Half the free permits" is the first form of ruling M1-R34, 2 writers on
  CI's 4-vCPU runner, on the slower of its two machine types, where it
  failed the gate in 2 of 4 runs.
- "Now" is the code at the end of this work: M1-R34 as amended (3 writers
  on 4 vCPUs), each term's stored bytes recorded at registration, and the
  sequential path trimmed. Locally at load average 12.

**The ten CI runs.** Ten parallel jobs of the gate on `ubuntu-latest`
(4 vCPUs), all passing, p50 / p99 in ms for the unmerged full shape:

<!-- table-caption: Reconstruction latency in ten Linux CI runs (p50 / p99, ms) -->

| Run | CPU | unmerged full | unmerged range | merged full | merged range |
|---|---|---|---|---|---|
| 1 | EPYC 7763 | 26.2 / 29.6 | 9.0 / 21.8 | 7.5 / 8.2 | 4.2 / 6.8 |
| 2 | EPYC 7763 | 26.1 / 28.2 | 9.0 / 22.0 | 7.4 / 8.8 | 4.2 / 6.8 |
| 3 | EPYC 7763 | 26.5 / 29.5 | 9.2 / 22.6 | 7.7 / 8.8 | 4.3 / 6.9 |
| 4 | EPYC 7763 | 26.0 / 28.2 | 9.0 / 21.8 | 7.3 / 7.9 | 4.1 / 6.6 |
| 5 | EPYC 7763 | 26.8 / 29.2 | 9.2 / 22.5 | 7.8 / 8.2 | 4.4 / 7.0 |
| 6 | EPYC 9V45 | 14.4 / 28.0 | 5.4 / 12.5 | 4.0 / 4.3 | 2.6 / 3.9 |
| 7 | EPYC 7763 | 25.2 / 28.0 | 8.8 / 21.3 | 7.1 / 7.5 | 4.0 / 6.3 |
| 8 | EPYC 7763 | 25.8 / 28.0 | 8.9 / 21.5 | 7.5 / 7.8 | 4.2 / 6.8 |
| 9 | EPYC 9V45 | 14.5 / 16.4 | 5.4 / 12.3 | 4.2 / 5.1 | 2.6 / 4.1 |
| 10 | EPYC 7763 | 25.8 / 28.1 | 9.0 / 21.8 | 7.4 / 7.8 | 4.1 / 6.7 |

The runner came as two machine types. The EPYC 7763 is the slower one,
and 8 of the 10 jobs ran on it; it passes with about 20 ms of p99 margin.
The earlier failing runs didn't log their CPU, but were slower on every
shape in the same way. The macOS runner, a 3-vCPU VM, missed the p99 for the
unmerged shape even with every writer, with p50s within it. Its numbers
are recorded here, not gated. The CI step prints the runner's CPU model
before the gate, so a log tells the two machine types apart.

**Run it on a quiet machine.** With 200 samples, the p99 is the third
slowest request, so three scheduler stalls fail it. On a busy host the
p50s stay steady while the p99 fails: that is load, not a regression.

### What the gate proves, and what it doesn't

The gate proves that one reconstruction at a time, on an otherwise idle
node, against an index of 1,000 xorbs, answers the worst-case 100k-term
file within 50 ms at p99. On that idle node the answer is written by
several threads, each past the first under a `validate` permit free at
that moment: at most half, rounded down, of the permits free counting the
request's own (ruling M1-R34), so 3 threads on CI's 4-vCPU runner and 5 on
the 8-core M3.

It doesn't prove design §5.8's target: p99 < 50 ms **at 1,000 requests/s
per node, against an index of 10⁷ files and 10⁷ xorbs**, which §16 assigns
to the release rig. Under load the free permits run out and an answer is
written on one thread. Single-threaded, the unmerged 100k-term full answer
took 66.2 ms p50 and 73.6 ms p99 on CI Linux before the sequential path
was trimmed ("Before threads" above), and takes 31.0 ms p50 and 39.0 ms
p99 locally now. Measuring §5.8 under load on the release rig is tracked
in <https://github.com/stubbi/openxet/issues/43>.

### What each change bought

In order of commit. Stage times are for the unmerged 100k-term full
answer.

| Change | Measured |
|---|---|
| Sign from a pre-serialized base, MAC state kept per xorb (`UrlSigner::transfer_urls`) | 100k URLs: 134.5 ms → 24.1 ms (16.3 ms into one buffer); the HMAC alone is 8.4 ms |
| Load chunk layouts, not whole chunk tables (`Index::chunk_layouts`, migration 0003) | 1,000 xorbs of 1,024 chunks: 24.5 ms → 6.2 ms, 40 MB → 8 MB |
| Write the answer by hand into one buffer | gate p50, unmerged full: 80.2 → 58.7 ms; merged full: 25.4 → 17.2 ms |
| foldhash for the per-request maps | plan 3.9 → 2.6 ms, window 1.1 → 0.4 ms, answer 29 → 24 ms; gate p50, unmerged full: 58.7 → 39.9 ms |
| Batch reconstruction looks files up in one query per 500 (`Index::files`) | 1,000 files: 17.99 ms → 1.62 ms |
| Write a large answer on several threads, each past the first under a free `validate` permit | gate p50 / p99 on CI Linux, unmerged full: 66.2 / 73.6 → 24.8 / 28.2 ms |
| Cap the writers at half the free permits, counting the request's own (M1-R34 as amended) | 3 writers on CI's 4 vCPUs (2 under the first form) |
| Record each term's stored bytes at registration (`Index::file_with_bytes`, migration 0004), so only the terms trimmed to a range need layouts | layouts 5.6 → 0.02 ms; gate p50 / p99, unmerged full: 29.4 / 83.9 → 21.3 / 35.7 ms, merged full 15.6 / 27.3 → 8.5 / 24.0 ms |
| List a xorb's descriptors without a set while they arrive in increasing order | catalog 3.01 → 2.38 ms |
| Size decoded term lists up front | record decode 1.60 → 0.68 ms |
| Size the plan's and catalog's descriptor lists up front | plan 1.75 → 1.09 ms, catalog 2.42 → 1.77 ms; gate p50 / p99, unmerged full: 18.9 / 25.0 → 17.4 / 20.4 ms |

Tried and dropped:
- Reading the layouts of 1,000 xorbs in concurrent queries: 6.4 ms
  against 5.1 ms, and 125-xorb queries ran no faster side by side, so the
  reads serialise inside SQLite.
- Larger SQLite pages for the layout read: 16 KiB and 64 KiB pages made
  it slower (8.5 and 10.9 ms against 5.3 ms).
- Fewer string pushes and MAC updates per signed URL: no measurable gain
  writing into one buffer.

What the layouts and term bytes cost:
- `xorb_layouts` repeats 8 of the 40 bytes per chunk that
  `xorbs.chunk_table` holds, 20% more chunk-table index. At design §5.8's
  sizing of 1 PB of unique data (about 1.6×10¹⁰ chunks) that is about
  128 GB more index. Its rows reference `xorbs` with a foreign key, so
  garbage collection must delete a xorb's layout row before its `xorbs`
  row. (Migration 0003 itself is left unedited: sqlx checksums applied
  migrations, comments included.)
- `file_scopes.term_bytes` adds 8 bytes per term, 800 kB for a 100k-term
  file, and registration reads the layouts of the xorbs a shard names.
  Shard commit throughput at 32 permits stayed at 212 to 225 shards/s.

Where the unmerged full answer's time goes now, locally (load 12, 3
writers, medians of 201): file record 2.6 ms (1.8 ms SQLite read, 0.7 ms
decode), window 0.1 ms, layouts 0.01 ms, plan 1.1 ms, catalog 1.8 ms, the
writers 7.5 ms, then handing the 42 MB body over.
## Criterion: reconstruction

`crates/cas/benches/reconstruction.rs`. One full-file
`GET /v2/reconstructions` in process for files of 1k, 10k and 100k terms in
both shapes, and signing 100k URLs over 1,000 xorbs. Local, mean of the
estimate; the first column was taken before the writer threads, at load
average 13, the second after, at load average 6 rising to 25:

| Bench | Before threads | After threads |
|---|---|---|
| `reconstruction/unmerged/1000` | 415 µs | 396 µs |
| `reconstruction/merged/1000` | 335 µs | 189 µs |
| `reconstruction/unmerged/10000` | 3.85 ms | 7.87 ms |
| `reconstruction/merged/10000` | 1.33 ms | 3.39 ms |
| `reconstruction/unmerged/100000` | 46.6 ms | 52.4 ms |
| `reconstruction/merged/100000` | 17.4 ms | 28.9 ms |
| `sign/100k_urls` | 15.5 ms | 34.7 ms |

The signing code didn't change between the two columns, so its 2.2× is
the host's load; read the second column as an upper bound until it is
rerun on a quiet machine. The gate's numbers above are the reference.

## Criterion: xorb ingest

`crates/cas/benches/xorb_ingest.rs`, on a 64 MiB xorb of 1,024 text-like
chunks of 64 KiB, LZ4-compressed by upstream's `serialize_chunk` as
`hf_xet` compresses them: 67,108,864 bytes in a 34,995,481-byte body.
Local, load average 15:

| Bench | Time | Throughput (uncompressed) |
|---|---|---|
| `xorb_ingest/xorb_with_footer`, all of `POST /v1/xorbs`'s CPU work | 123.4 ms | 519 MiB/s |
| `xorb_ingest/validation_pass`, openxet's decompress-and-hash pass alone | 77.6 ms | 825 MiB/s |

**The M1-R8 double decompression costs about 46 ms per 64 MiB xorb,
37% of ingest.** `xorb_with_footer` decompresses and hashes every chunk
to validate it, then upstream's `reconstruct_xorb_with_footer`
decompresses and hashes them again to build the footer. Ruling M1-R8
keeps that, since openxet doesn't reimplement footer serialization (ADR
0001). The cost is what a change could buy back: up to 1.6× faster
ingest per core (123.4 / 77.6), for instance by handing upstream's footer
serializer the first pass's hashes and offsets.

## Shard commit throughput (M1-R24)

A shard commit is the bucket write of the stored shard and its index
registration, under the `commit` semaphore (4 × CPUs permits, 32 on the
local machine). The bench commits 400 shards of 4 files each (64 KiB
stored shard, one term per file) concurrently to a `LocalBucket` and a
SQLite index on the local SSD, with the semaphore at each size:
`cargo bench -p openxet-cas --bench commit_throughput`.

| Permits | 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 |
|---|---|---|---|---|---|---|---|---|
| Shards/s, before `fullfsync` (Task 16, quieter host) | 151 | 193 | 172 | 196 | 220 | 202 | 217 | 219 |
| Shards/s, before `fullfsync` (rerun, load 7 to 25) | 52 | 61 | 48 | 95 | 111 | 115 | 119 | 126 |
| Shards/s, with `fullfsync` (same load) | 18–57 | 51–61 | 58–66 | 59–62 | 72–73 | 67–77 | 79–85 | 78–87 |

The two rows taken under load ran back to back, minutes apart, with only
the pragma changed; the first row is the original measurement, on a less
loaded host, kept for reference. Throughput levels off past a few permits:
every registration is a durable SQLite commit, and one writer at a time
holds the database (design §5.8's index scaling is M3's Postgres), so the
plateau is the rate of durable index commits. `fullfsync` makes each of
those commits slower, which lowers the plateau on macOS (next section).
4 × CPUs is past the plateau, so it never limits throughput on local
storage; what it bounds is memory, which ruling M1-R24 sets it for. A
remote bucket's slower puts overlap under the same permits, so the number
stays right until a bucket's latency makes 32 concurrent puts the limit.

## SQLite `fullfsync` on macOS

On macOS, `fsync` returns once the bytes reach the drive's cache, so a
commit acknowledged under `synchronous=FULL` alone could be lost on power
loss. The index therefore also sets `PRAGMA fullfsync=ON`, which makes
SQLite sync with `F_FULLFSYNC`, as the bucket's `sync_all` already does.
SQLite has no `F_FULLFSYNC` to use on Linux and ignores the pragma there,
so Linux servers and the Linux CI runner are unaffected. macOS hosts pay
the cost below, and so does CI's `macos-latest` job when it runs the test
suite.

Measured on the local machine (Apple M3, internal SSD, macOS 15.6.1;
load average 7 to 25 from other builds) with the same bench's second
part: 200 index commit pairs (a `put_xorb` and a `register_shard` of 4
files, each its own durable transaction) one after another, then 200
concurrently, three rounds on a fresh index each, with and without the
pragma:

| | Sequential, per pair | Sequential pairs/s | Concurrent pairs/s |
|---|---|---|---|
| `synchronous=FULL` | 0.38–0.43 ms | 2,319–2,610 | 2,421–2,563 |
| plus `fullfsync=ON` | 8.5–15 ms | 66–118 | 71–119 |

One further round with the pragma took 73 ms per pair while another build
saturated the disk. Each durable index commit costs about 4–7 ms more
with `F_FULLFSYNC`, and whole shard commits fall from about 115–126 to
about 70–87 per second (table above). Durability of an acknowledged write
(design §5.9 req 7) comes first.

## Stock client behaviour that sizes the server

- **`hf_xet` 1.7.0 retries a `408`.** Its retry policy classes `408`, like
  `429` and `5xx`, as transient (`xet-client` 1.7.0,
  `cas_client/retry_wrapper.rs:503-521`, `default_on_request_success`), so
  the body-pace `408` costs a client one retry of that request, not the
  upload; S10's status table agrees.
- **`hf_xet` 1.7.0 uploads up to 64 xorbs at once.** Its adaptive
  concurrency starts at 2 concurrent uploads and may grow to 64
  (`xet-runtime` 1.7.0, `config/groups/client.rs:207-223`). Uploading 1 GiB
  of random data (17 xorbs) to a local `openxet serve` reached 6
  concurrent `POST /v1/xorbs` in the server's log. At the body-pace floor
  of 4 KiB/s per connection, 64 concurrent uploads need 256 KiB/s
  (about 2 Mbit/s) from one client before any is cut off, and 6 need
  24 KiB/s.
