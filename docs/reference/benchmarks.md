# Benchmarks

What openxet measures about its own speed, how to rerun it, and the last
results. AGENTS.md's rule holds here: a performance claim comes with a
benchmark, and a speed regression fails CI.

| What | Kind | Command |
|---|---|---|
| Reconstruction latency, design §5.8 | CI gate (Linux runner); locally, needs a quiet machine | `cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)' --no-capture` |
| Reconstruction and URL signing | criterion, for humans | `cargo bench -p openxet-cas --bench reconstruction` |
| Xorb ingest | criterion, for humans | `cargo bench -p openxet-cas --bench xorb_ingest` |

Criterion keeps its last run under `target/criterion` and reports the
change on the next one. The benches are not CI gates: shared runners vary
too much for criterion's statistics to fail a build on.

## Machines

- **Local:** Apple M3 (4 performance and 4 efficiency cores), 16 GB,
  macOS 15.6.1, rustc 1.97.1, release profile. The host was shared with
  other builds during every run here (load average 4 to 27 on 8 cores), so
  local numbers are noisy, tails most of all. Where a number moved only
  because of load, the text says so.
- **CI:** GitHub's `ubuntu-latest` (4 vCPU) and `macos-latest` (3 vCPU
  Apple M1 VM) runners, release profile, from the `rust` job's log.

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

| Shape | Start (local) | Before threads (CI Linux) | All writers (CI Linux) | All writers (CI macOS) | Now, M1-R34 cap (local) | Now, M1-R34 cap (CI Linux) |
|---|---|---|---|---|---|---|
| unmerged full | 246 / 679 | 66.2 / 73.6 | 24.8 / 28.2 | 57.5 / 90.3 | 28.1 / 33.9 | **30.6 / 39.2** |
| unmerged range | 74 / 486 | 25.3 / 59.8 | 14.7 / 23.8 | 31.1 / 70.3 | 15.2 / 43.1 | **15.2 / 29.1** |
| merged full | 50 / 93 | 19.7 / 25.9 | 13.2 / 15.7 | 23.7 / 32.8 | 15.8 / 22.3 | **14.2 / 18.4** |
| merged range | 14 / 42 | 7.7 / 18.7 | 5.2 / 12.6 | 10.1 / 23.0 | 6.8 / 14.1 | **5.7 / 12.4** |

"Start" is the gate against the code before this work. "Before threads"
is CI with every change below but the writer threads, so one writer.
"All writers" let an answer take every free `validate` permit; "Now"
takes at most half of them (ruling M1-R34), which on CI's 4-vCPU runner
means 2 writers instead of 4.

The Linux runner meets the gate with about 11 ms of p99 margin. The
shared local host passes or fails it with its neighbours' load: at load
average 10 to 22, runs of the same code had unmerged p99s from 34 ms to
97 ms while the p50s stayed within a millisecond. The macOS runner, a
3-vCPU VM, missed the p99 for the unmerged shape even with every writer,
with p50s within it. The gate keeps the one threshold and runs where CI's
timings are stable; the macOS runner's numbers are recorded here, not
gated.

**Run it on a quiet machine.** With 200 samples, the p99 is the third
slowest request, so three scheduler stalls fail it. On a busy host the
p50s stay steady while the p99 fails: that is load, not a regression.

### What the gate proves, and what it doesn't

The gate proves that one reconstruction at a time, on an otherwise idle
node, against an index of 1,000 xorbs, answers the worst-case 100k-term
file within 50 ms at p99. On that idle node the answer is written by
several threads: each past the first runs under a `validate` permit that
is free at that moment, at most half of the free ones (ruling M1-R34), so
CI's 4-vCPU Linux runner writes on 2 threads and the 8-core M3 on 4.

It doesn't prove design §5.8's target: p99 < 50 ms **at 1,000 requests/s
per node, against an index of 10⁷ files and 10⁷ xorbs**, which §16 assigns
to the release rig. Under load the free permits run out and an answer is
written on one thread. Single-threaded, the unmerged 100k-term full
answer took 66.2 ms p50 and 73.6 ms p99 on CI Linux ("Before threads"
above) and 39.9 ms p50 locally, so the worst-case shape likely misses
50 ms under load on a 4-vCPU-class node. Measuring §5.8 under load on the
release rig, and the single-thread headroom it may need, is tracked in
<https://github.com/stubbi/openxet/issues/43>.

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

Tried and dropped: reading the layouts of 1,000 xorbs in four concurrent
queries took 5.1 ms against 6.4 ms for one, not worth the code.

What the layouts cost: `xorb_layouts` repeats 8 of the 40 bytes per chunk
that `xorbs.chunk_table` holds, 20% more chunk-table index. At design
§5.8's sizing of 1 PB of unique data (about 1.6×10¹⁰ chunks) that is about
128 GB more index, for planning that reads a fifth of the bytes. Its rows
reference `xorbs` with a foreign key, so garbage collection must delete a
xorb's layout row before its `xorbs` row. (Migration 0003 itself is left
unedited: sqlx checksums applied migrations, comments included.)

Where the unmerged full answer's time went before the writer threads,
locally: file record 2.5 ms, window 0.4 ms, layouts 5.8 ms, plan 2.6 ms,
then 24 ms for the catalog, signing and writing, the part the threads now
share.

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
registration, under the `commit` semaphore (4 × CPUs permits, 32 here).
Measured with a scripted run: 400 shards of 4 files each (64 KiB stored
shard, one term per file) committed concurrently to a `LocalBucket` and a
SQLite index on the local SSD, with the semaphore at each size:

| Permits | 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 |
|---|---|---|---|---|---|---|---|---|
| Shards/s | 151 | 193 | 172 | 196 | 220 | 202 | 217 | 219 |

Throughput levels off from 2 permits at about 200 shards/s: every
registration is a durable SQLite write, and one writer at a time holds
the database (design §5.8's index scaling is M3's Postgres). 4 × CPUs is
past the plateau, so it never limits throughput on local storage; what it
bounds is memory, which ruling M1-R24 sets it for. A remote bucket's
slower puts overlap under the same permits, so the number stays right
until a bucket's latency makes 32 concurrent puts the limit.

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
