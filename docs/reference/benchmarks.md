# Benchmarks

What OpenXet measures about its own speed, how to rerun it, and the last
results. AGENTS.md's rule holds here: a performance claim comes with a
benchmark, and a speed regression fails CI.

| What | Kind | Command |
|---|---|---|
| Reconstruction latency, design §5.8 | CI gate (Linux runner): against the base on a pull request, absolute on `main` (ruling M2-R17); locally, needs a quiet machine | `cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)' --no-capture` |
| Hub token, resolve and tree latency, design §5.8 | CI gate (Linux runner), compared like the reconstruction gate (ruling M2-R17); locally, needs a quiet machine | `cargo nextest run --release -p openxet-hub -E 'binary(hub_latency_gate)' --no-capture` |
| Serving a clone: OpenXet's CPU against git's | CI gate (Linux runner), both measured in the same run | `cargo nextest run --release -p openxet-git -E 'binary(fetch_gate)' --no-capture` |
| The thin-base walk and `git.thin_bases`' defaults | CI gate (Linux runner) | `cargo nextest run --release -p openxet-git -E 'binary(thin_bases_budget)' --no-capture` |
| Reconstruction and URL signing | criterion, for humans | `cargo bench -p openxet-cas --bench reconstruction` |
| Xorb ingest | criterion, for humans | `cargo bench -p openxet-cas --bench xorb_ingest` |
| Shard and index commit throughput | a plain run, for humans | `cargo bench -p openxet-cas --bench commit_throughput` |
| Hub metadata reads, the gate's and the rest | a plain run, for humans | `cargo bench -p openxet-hub --bench metadata` |
| S9's fetch replay: clone and incremental fetch | a plain run, for humans | `cargo bench -p openxet-git --bench fetch` |
| Memory under attack, ruling M1-R40 | script, for humans; needs `cargo build --release -p openxet` | `python3 scripts/measure-memory-bounds.py --openxet "$PWD/target/release/openxet" --scenario reconstruct --connections 64 --memory-budget 1GiB` |
| Stock client throughput, `503`s and peak memory | script, for humans; needs `cargo build --release -p openxet` | `uv run --frozen --directory compat python ../scripts/measure-storage-only.py --openxet "$PWD/target/release/openxet" --size-mib 4096` |

Criterion keeps its last run under `target/criterion` and reports the
change on the next one. The benches are not CI gates: shared runners vary
too much for criterion's statistics to fail a build on.

## Machines

- **Local:** Apple M3 (4 performance and 4 efficiency cores), 16 GB,
  macOS 15.6.1, rustc 1.97.1, release profile. The host was shared with
  other builds during every run here (load average 4 to 27 on 8 cores), so
  local numbers are noisy, tails most of all. Where a number moved only
  because of load, the text says so.
- **CI:** GitHub's `ubuntu-latest` (4 vCPU, an AMD EPYC 7763, 9V45 or 9V74,
  or an Intel Xeon 6973P-C or Platinum 8573C)
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
and a SQLite index on disk, timed to the body's last byte. The server uses
`per-tenant` visibility, as a multi-tenant host does; reconstruction reads
chunk layouts the same way in either mode. The gate runs on the Linux
runner only, as the other release-build gate does.

p50 / p99 in ms:

<!-- table-caption: Reconstruction latency by implementation stage (p50 / p99, ms) -->

| Shape | Start (local) | Before threads (CI Linux) | All writers (CI Linux) | All writers (CI macOS) | Half the free permits (CI Linux, slow runner) | Now (local) | Now (CI Linux, 10 runs) |
|---|---|---|---|---|---|---|---|
| unmerged full | 246 / 679 | 66.2 / 73.6 | 24.8 / 28.2 | 57.5 / 90.3 | 43.7 / 53.1 | 17.4 / 20.4 | **14.4 to 26.8 / 16.4 to 29.6** |
| unmerged range | 74 / 486 | 25.3 / 59.8 | 14.7 / 23.8 | 31.1 / 70.3 | 21.5 / 40.3 | 7.6 / 15.4 | **5.4 to 9.2 / 12.3 to 22.6** |
| merged full | 50 / 93 | 19.7 / 25.9 | 13.2 / 15.7 | 23.7 / 32.8 | 19.3 / 26.2 | 6.5 / 8.6 | **4.0 to 7.8 / 4.3 to 8.8** |
| merged range | 14 / 42 | 7.7 / 18.7 | 5.2 / 12.6 | 10.1 / 23.0 | 7.9 / 19.6 | 4.2 / 7.0 | **2.6 to 4.4 / 3.9 to 7.0** |

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
p99 fails first, and on a loaded host the p50 of the unmerged full shape,
whose answer is written on several threads, rises too: that is load, not
a regression. Locally the test asserts p99 < 50 ms, as it always has.

### How CI gates it (ruling M2-R17)

A shared runner can be slow enough to miss 50 ms with nothing changed. On
2026-10-25 the `rust` job of PR #82, which changes nothing the gate
compiles, failed on all twelve attempts of one CI run (issue #83). The
first three logged `unmerged full` at p50 40 to 48 / p99 58 to 68 ms on an AMD
EPYC 7763 and an Intel Xeon 6973P-C, and the twelfth p50 39.7 / p99
61.0 ms on an AMD EPYC 9V74, the other shapes within their usual range. A
re-run of PR #81's merged job, which had passed at p50 30.7 / p99 34.2 ms
hours earlier, failed the same way at the same time (p50 40.2 / p99
58.8 ms). Ruling M1-R35 fixes a flaky gate rather than loosening it, so
the gate now compares a pull request with its base on the same runner:

- On a **pull request** the job checks out the PR's base, the base
  branch's tip recorded in the event (`pull_request.base.sha`), and runs
  the gate from it and then from the PR, each in a release build with
  `OPENXET_LATENCY_GATE=measure`. Both runs use HEAD's measurement code:
  the job copies the PR's `crates/cas/tests/latency_gate.rs` and its
  fixture (`crates/cas/tests/fixture/large_files.rs`) over the base's
  before building, so only the measured library comes from the base, and
  a base from before this ruling is measured the same way. The base
  builds in its own target directory (`target/latency-base`): in a shared
  one, cargo's fingerprints for the workspace crates match across the two
  checkouts, and the head step would run the base's build again. Each run
  prints one `openxet.latency-gate.v1` JSON line per shape, such as this
  one from the local machine:

  ```json
  {"schema":"openxet.latency-gate.v1","build":"<commit>","shape":"unmerged full","p50_ms":20.178,"p99_ms":26.571,"max_ms":28.145,"runs":200}
  ```

  `build` is `OPENXET_LATENCY_GATE_BUILD` when the test was compiled, which
  CI sets to each checkout's commit (`null` when unset, as in a local run).
  `scripts/check-latency-gate.py --base base.json --head head.json
  --head-build <commit>` exits 2 when the head's `build` isn't the commit
  CI checked out, when both runs report the same `build`, or when a line
  carries a missing, non-numeric, negative or non-finite value. It fails a
  shape when the PR's p99 is at or past 50 ms and more than 1.25 × the
  base's p99, or when the PR's p50 > max(1.25 × the base's p50, the base's
  p50 + 2 ms). A runner that can meet 50 ms on the base still holds the PR
  to it, failing at 50 ms as the test's own assertion does; a runner too
  slow for it holds the PR to its base, so a slowdown of more than a
  quarter fails on any runner. The 2 ms floor matters for the merged
  shapes, whose p50s of a few milliseconds would otherwise fail on a
  millisecond of noise between the runs. A shape the base didn't measure
  (a base that can't build the PR's test because it lacks an API the test
  uses, or a new shape) is held to 50 ms alone.
- On **`main`** the job runs the pushed commit alone and asserts
  p99 < 50 ms, as before; a slow runner can turn it red without a
  regression, so read its log before reverting anything. The release rig
  (<https://github.com/stubbi/openxet/issues/55>) enforces the absolute
  bar on every release, whatever CI's runners do.

**Reading the summary.** The comparison writes the two builds' commits
and a table to the job's summary page: each shape's base and PR p50 /
p99, the two ratios, the p99 bar it applied, and pass or fail, followed
by each failure with the number that broke it. Ratios near 1.00 with both
runs over 50 ms mean a slow runner, not a regression. A p50 past its bar
with the base steady is a regression; rerun once to rule out a load
change between the two runs, and if it holds, find what made the answer
slower. When the base's build or run fails, the summary says so in one
line above the table, and the shapes it didn't measure are held to 50 ms
alone. Run the script's tests with
`python3 -I scripts/test_check_latency_gate.py`.

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
## Hub metadata latency gate

`crates/hub/tests/hub_latency_gate.rs` asks for p99 < 10 ms on token,
resolve (`HEAD`) and tree (design §5.8), against an index of 10,000
repositories, one of which holds 10,000 files. The shape is pinned
(`crates/hub/tests/fixture/metadata.rs`) so the gate can't pass on an easy
repository:

- 9,000 Xet files in the root, each an LFS pointer whose SHA-256 is
  registered to the repository, so every one a `tree` page lists needs its
  `xetHash` from the index
- 1,000 small regular files under `configs/`, which `resolve` serves from
  their Git blob
- the other 9,999 repositories are index rows; the token route reads only
  a repository's row

Each kind is asked 1,000 times, one at a time, in process through the whole
router, the Git engine (a per-request snapshot, ADR 0004 item 1) and a
SQLite index on disk, timed to the body's last byte:

- `token`: `GET /api/models/{repo}/xet-read-token/main`, a random
  repository of the 10,000
- `resolve head xet`: `HEAD /{repo}/resolve/main/{path}` of a random Xet
  file, a `302` to the file gateway
- `resolve head regular`: the same for a random regular file, a `200`
- `tree`: `GET /api/models/{repo}/tree/main`, the root's first page of
  1,000 entries: `.gitattributes`, `configs` and 998 Xet files

It prints the reconstruction gate's `openxet.latency-gate.v1` lines and
reads the same variables, so CI gates it the same way (ruling M2-R17): on
a pull request against the base, measured on the same runner, with
`scripts/check-latency-gate.py --p99-limit-ms 10 --title "Hub metadata
latency gate"`; on `main` absolute. A pull request whose base measured
`tree` at p99 70 ms holds the PR to 10 ms, as the absolute bar applies
whenever the PR misses it by more than a quarter of the base.

p50 / p99 in ms:

<!-- table-caption: Hub metadata latency by implementation stage (p50 / p99, ms) -->

| Request | Before (CI Linux, EPYC 9V45) | Before (CI Linux, Xeon 8573C) | Paged listing and batched lookups (CI Linux, EPYC 9V45) | The same (CI Linux, Xeon 8573C) | Now (CI Linux, 10 runs) |
|---|---|---|---|---|---|
| token | 0.06 / 0.08 | 0.1 / 0.1 | 0.07 / 0.09 | 0.1 / 0.1 | **0.1 / 0.1 to 0.2** |
| resolve head xet | 1.70 / 1.83 | 2.4 / 2.6 | 1.82 / 1.97 | 2.3 / 2.6 | **1.6 to 2.8 / 1.8 to 3.4** |
| resolve head regular | 1.76 / 3.25 | 2.3 / 2.7 | 1.78 / 1.97 | 2.4 / 2.5 | **1.7 to 2.8 / 1.9 to 3.2** |
| tree | 58.7 / 70.5 | 101.7 / 107.7 | 6.90 / 9.67 | 7.8 / 9.9 | **4.1 to 7.7 / 4.6 to 9.5** |

- "Before" is `main` at the time, measured by this gate as CI's base run.
  A `tree` page read every blob of the folder (9,001) for its size and
  pointer, then looked each Xet file up in the index one query at a time.
- "Paged listing and batched lookups" reads blobs only for the page
  (`Engine::read_tree_page`), looks the page's Xet files up in batches
  (`Repos::xet_files_by_sha256`), formats ids with a lookup table instead
  of a `write!` per byte, and serializes entries from a struct instead of
  building a `serde_json::Value` each. On the slower runner it passed with
  0.1 ms to spare, and four of ten absolute runs on the EPYC 7763 and
  9V74 missed 10 ms at p99 (10.4 to 11.8 ms).
- "Now" also sorts a folder's entries in place and makes paths only for
  the page; reads a large page's blobs on up to four threads sharing one
  object store (gix's `parallel` feature); and sends the page's SHA-256s
  to SQLite as one JSON parameter and takes the matches back as one JSON
  row.

Where a `tree` page's time went on the EPYC 7763 before the last two
changes (medians of 1,000, phases timed in a throwaway build): 0.75 ms for
the snapshot and the revision, 1.0 ms to inflate and decode the 9,001-entry
root tree, 1.6 ms to read the page's blobs, 2.7 ms for the index lookup,
0.65 ms to serialize. The lookup is SQLite's: binding 998 parameters and
returning 998 rows through sqlx cost about as much as the seeks, and one
JSON parameter in and one row out took 1.3 ms to 1.0 ms locally; four
concurrent quarter batches were no faster, as the CAS found for layout
reads. Every Git-backed read also pays the snapshot, about 2 ms of a
`resolve` locally; reusing snapshots across reads is
<https://github.com/stubbi/openxet/issues/107>.

**The ten CI runs.** Ten parallel jobs on `ubuntu-latest`, each measuring
the base (`main` at `30b6fc2`, with this gate's code) and the head as a
pull request does, all passing the comparison and, read as `main` reads
them, all under 10 ms at p99. p50 / p99 in ms:

<!-- table-caption: Hub metadata latency in ten Linux CI runs (p50 / p99, ms) -->

| Run | CPU | token | resolve head xet | resolve head regular | tree | tree on the base |
|---|---|---|---|---|---|---|
| 1 | EPYC 9V74 | 0.1 / 0.1 | 2.8 / 2.9 | 2.8 / 3.2 | 7.7 / 8.7 | 93.7 / 99.9 |
| 2 | EPYC 7763 | 0.1 / 0.2 | 2.7 / 3.4 | 2.7 / 2.8 | 7.2 / 9.5 | 105.2 / 132.9 |
| 3 | EPYC 7763 | 0.1 / 0.2 | 2.7 / 3.2 | 2.7 / 2.8 | 7.1 / 8.2 | 107.9 / 133.1 |
| 4 | EPYC 9V74 | 0.1 / 0.1 | 2.1 / 2.6 | 2.1 / 2.2 | 5.6 / 5.9 | 72.1 / 76.7 |
| 5 | EPYC 9V74 | 0.1 / 0.1 | 2.1 / 2.2 | 2.2 / 2.7 | 5.8 / 7.2 | 72.1 / 83.5 |
| 6 | EPYC 7763 | 0.1 / 0.2 | 2.7 / 2.9 | 2.7 / 3.0 | 7.4 / 8.0 | 110.9 / 131.9 |
| 7 | EPYC 9V74 | 0.1 / 0.1 | 2.2 / 2.8 | 2.2 / 3.2 | 5.9 / 8.0 | 74.6 / 85.7 |
| 8 | EPYC 9V45 | 0.1 / 0.1 | 1.6 / 1.8 | 1.7 / 1.9 | 4.1 / 4.6 | 52.8 / 57.3 |
| 9 | EPYC 7763 | 0.1 / 0.2 | 2.6 / 2.8 | 2.6 / 2.7 | 7.1 / 7.8 | 97.8 / 104.4 |
| 10 | EPYC 9V45 | 0.1 / 0.1 | 1.8 / 1.9 | 1.8 / 1.9 | 5.0 / 7.0 | 65.6 / 86.6 |

`tree` on the EPYC 7763, the most common machine type, has 0.5 to 2 ms
of p99 margin; a loaded runner can take that, which is what the
comparison with the base (ruling M2-R17) absorbs on a pull request.

`cargo bench -p openxet-hub --bench metadata` times the same fixture's
other reads. Local, Apple M3, load average 8, at the "paged listing" stage;
p50 / p99 / max in ms:

<!-- table-caption: Hub metadata reads on the gate's fixture (p50 / p99 / max, ms) -->

| Request | p50 | p99 | max |
|---|---|---|---|
| token (read) | 0.03 | 0.67 | 1.27 |
| token (write) | 0.03 | 0.11 | 1.10 |
| resolve HEAD, Xet file | 3.64 | 6.68 | 30.15 |
| resolve HEAD, regular file | 3.77 | 8.41 | 47.44 |
| resolve GET, regular file | 3.86 | 11.91 | 21.88 |
| tree, root's first page | 8.29 | 13.52 | 47.51 |
| tree, first page with expand | 268.75 | 341.58 | 494.59 |
| tree, recursive first page | 5.10 | 5.82 | 52.75 |
| paths-info, 100 Xet files | 15.96 | 23.25 | 57.78 |

`expand=true` decodes the folder's tree once per listed path to find each
path's last commit, 270 ms a page here; that is
<https://github.com/stubbi/openxet/issues/106>. The gate doesn't measure it,
as design §5.8 bars token, resolve and tree.

**What the gate doesn't prove.** One request at a time on an idle node,
against a SQLite index of 10,000 repositories. Design §5.8's target holds
at 1,000 requests/s per node against an index of 10⁷ files, which the
release rig measures (<https://github.com/stubbi/openxet/issues/55>).

## Fetch gate

`crates/git/tests/fetch_gate.rs` replays spike S9's server-only fetch
(`crates/git/tests/fixture/clone_replay.rs`): one protocol v2 `fetch`
request for a clone, served by OpenXet's upload-pack (snapshot, R22 checks
of the wants, `git upload-pack` in the snapshot, the response streamed
through) and by `git upload-pack --stateless-rpc` alone on the same
repository, each response read whole and discarded. The repository is
`testdata/build-fixture.sh`'s `tf10k` of 10,001 commits, repacked into one
pack with a bitmap as S9's was, about 7 MB. Each side's CPU is what the
operating system charged: for OpenXet, the test process and every git it
started (`getrusage` `RUSAGE_SELF` plus `RUSAGE_CHILDREN`, as S9 counted
it); for git, its process tree. After a warm-up of each (OpenXet's first
request hydrates its node cache), 7 alternating runs; the medians must
stay within 2× (design §16 "Speed"). Measuring git in the same run makes
the bar the runner's own, so a slow runner can't fail it.

<!-- table-caption: Clone of the 10,001-commit fixture: server CPU -->

| Machine | git, ms CPU | OpenXet, ms CPU | Ratio |
|---|---|---|---|
| Local, Apple M3 | 19.1 | 23.9 | 1.25 |
| CI Linux, EPYC 9V45 | 18.4 | 20.8 | 1.13 |
| CI Linux, Xeon 8573C | 24.2 | 26.9 | 1.11 |
| CI Linux, ten runs (EPYC 7763, 9V45, 9V74) | 16.3 to 29.8 | 18.5 to 33.9 | 1.11 to 1.17 |

S9 measured git at 178 ms for its `tf10k`, the real `huggingface/transformers`
history (218,574 objects, a 72.7 MB pack); the fixture's files are small,
so its pack is a tenth of that and the snapshot's fixed cost weighs more in
the ratio. `cargo bench -p openxet-git --bench fetch` adds a fetch 100
commits behind `main`. Local, Apple M3, medians of 9:

<!-- table-caption: S9's fetch replay on the 10,001-commit fixture -->

| Request | Engine | Server CPU ms | Server wall ms | Bytes sent |
|---|---|---|---|---|
| Full clone | git | 22.1 | 21.7 | 6,965,735 |
| Full clone | OpenXet | 26.6 | 22.8 | 6,962,895 |
| Fetch 100 commits (thin) | git | 13.6 | 15.6 | 69,389 |
| Fetch 100 commits (thin) | OpenXet | 15.7 | 17.1 | 69,384 |

The byte counts differ by side-band framing only: git's pack writes fall
differently from run to run, and the gate allows 1%.

## Thin-base budget

A push's thin-pack bases are placed by one tree lookup each where they
can be, and otherwise by one walk of everything the snapshot's refs reach,
under `git.thin_bases.max_objects` and `max_ms` (ADR 0004 item 3).
`crates/git/tests/thin_bases_budget.rs` pushes, to the 10,001-commit
fixture, a commit sent as a delta against the root commit's version of a
file every later commit rewrote: no boundary or tip tree holds that base,
so only the walk finds it, and the walk lists the root commit's objects
last.

<!-- table-caption: The thin-base walk over the 10,001-commit fixture -->

| Machine | Objects the refs reach | Walked before the base | Full `rev-list --objects` | Objects per ms | Push through the walk (median of 5) |
|---|---|---|---|---|---|
| Local, Apple M3 (load 8) | 90,074 | 90,022 | 110 ms | 819 | 215 ms |
| CI Linux, EPYC 9V45 | 90,074 | 90,022 | 109 ms | 823 | 126 ms |
| CI Linux, Xeon 8573C | 90,074 | 90,022 | 144 ms | 625 | 166 ms |
| CI Linux, ten runs (EPYC 7763, 9V45, 9V74) | 90,074 | 90,022 | 93 to 166 ms | 542 to 969 | 113 to 194 ms |
| CI Linux, slowest seen (EPYC 7763) | 90,074 | 90,022 | 188 ms | 479 | 194 ms |

The defaults, from these numbers:

- **`max_ms` stays 2,000**, R22's limit: a push through the walk of this
  repository takes 113 to 215 ms, so it has 9 to 17 times its time.
- **`max_objects` is 1,000,000** (it was a 2,000,000 placeholder): at 479
  to 969 objects per ms, 2,000 ms walks 0.96 to 1.94 million objects, so
  the object budget, the one that doesn't depend on the machine, binds
  first on all but the slowest runner seen, and every node gives the same
  answer for the same push. It is 11 times this repository and 4.6 times
  S9's real `tf10k` (218,574 objects).

The test asserts both: the full walk fits `max_objects`, and the push
through it finishes within `max_ms`.

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
| `xorb_ingest/validation_pass`, OpenXet's decompress-and-hash pass alone | 77.6 ms | 825 MiB/s |

**The M1-R8 double decompression costs about 46 ms per 64 MiB xorb,
37% of ingest.** `xorb_with_footer` decompresses and hashes every chunk
to validate it, then upstream's `reconstruct_xorb_with_footer`
decompresses and hashes them again to build the footer. Ruling M1-R8
keeps that, since OpenXet doesn't reimplement footer serialization (ADR
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
| Shards/s, with `fullfsync` (same load) | 18 to 57 | 51 to 61 | 58 to 66 | 59 to 62 | 72 to 73 | 67 to 77 | 79 to 85 | 78 to 87 |
| Shards/s, also writing the registration record and taking its number (M1-R37; load 13 to 17) | 48 | 47 | 59 | 68 | 70 | 72 | 64 | 59 |

The two rows taken under load ran back to back, minutes apart, with only
the pragma changed; the first row is the original measurement, on a less
loaded host, kept for reference. Throughput levels off past a few permits:
every registration is a durable SQLite commit, and one writer at a time
holds the database (design §5.8's index scaling is M3's Postgres), so the
plateau is the rate of durable index commits. `fullfsync` makes each of
those commits slower, which lowers the plateau on macOS (next section).
Since ruling M1-R37 each commit also takes a registration number from
the index and creates the shard's registration record in the bucket
before registering, as `ingest` does. The last row, one run at similar
load, matches the `fullfsync` row up to 32 permits and is about a quarter
lower above it, likely because the extra counter commit competes for
the one writer. 4 × CPUs is past the plateau, so it never limits throughput on
local storage; what it bounds is memory, which ruling M1-R24 sets it for. A
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
| `synchronous=FULL` | 0.38 to 0.43 ms | 2,319 to 2,610 | 2,421 to 2,563 |
| plus `fullfsync=ON` | 8.5 to 15 ms | 66 to 118 | 71 to 119 |

One further round with the pragma took 73 ms per pair while another build
saturated the disk. Each durable index commit costs about 4 to 7 ms more
with `F_FULLFSYNC`, and whole shard commits fall from about 115 to 126 to
about 70 to 87 per second (table above). Durability of an acknowledged write
(design §5.9 req 7) comes first.

## Memory bounds

Ruling M1-R40 bounds what requests hold in memory by one process-wide
budget, `server.memory_budget` (a quarter of the host's memory;
config.md, "Memory"). On the local machine (16 GB) the default is 4 GiB.
The "after" numbers are from the code as amended by its review: answers
pass queued uploads, one answer holds at most an eighth of the budget, and
reservations outlive a client that leaves.

### Under attack

`scripts/measure-memory-bounds.py` attacks a fresh dev-mode
`openxet serve` from many sockets for 20 s, stops it, and reports its peak:

- **reconstruct:** each socket asks for the full reconstruction of the
  latency gate's unmerged 100k-term file (a 42.3 MB answer) and never
  reads it
- **upload:** each socket posts a 64 MiB xorb, sends all of it but the
  last byte, and stalls

Local machine, one run each, peak in MiB. "Before" is `ff53a33`, which
had no budget:

| Scenario | Budget | Before: footprint / RSS | After: footprint / RSS |
|---|---|---|---|
| reconstruct, 16 sockets | 1 GiB | 712 / 1038 | 780 / 906 |
| reconstruct, 64 sockets | 1 GiB | 2658 / 1243 | 1101 / 1102 |
| upload, 32 sockets | 1 GiB | 1983 / 1913 | 1040 / 1045 |
| reconstruct, 128 sockets | default, 4 GiB | | 4201 / 1078 |
| upload, 96 sockets | default, 4 GiB | | 4143 / 3685 |

On macOS the physical footprint is the number to read: it counts the
memory the kernel compressed, which RSS leaves out, and leaves out freed
memory the allocator has not yet returned, which RSS counts. The answers'
JSON compresses well, so RSS understates the reconstruction rows.

- 16 answers fit a 1 GiB budget (each reserves about 60 MB while it is
  built, then its 42.3 MB), so the bound changes nothing there; they are
  freed by the send deadline, 70 s for this answer, instead of whenever
  TCP gives up.
- 64 answers held 2.7 GB before. Now about 15 fit 1 GiB at once (each
  reserves about 65 MB with its decoded record, within an answer's
  eighth), the rest wait 5 s and get `503 memory_busy`, and the peak
  stays within about 100 to 250 MB of the budget, the process's own
  baseline. With the default budget, 128 answers peak at the budget plus
  the same baseline.
- 32 stalled uploads held 32 × 64 MiB before. Now 16 fit 1 GiB and the
  rest queue, unread, so the peak is the budget plus the baseline; 96 on
  the default budget peak at it.

### Stock client throughput

The budget must not cost a stock client anything: `hf_xet` sends up to 64
xorbs at once, and uploads past the budget queue for up to 120 s instead
of failing. `scripts/measure-storage-only.py` (`--size-mib 4096`) uploads
and downloads a 4 GiB file with stock `hf_xet` 1.7.0 at its default
concurrency against a default dev-mode server, and counts the server's
`memory_busy` and other `5xx` answers. Local machine, at load average 5
to 76 from other builds, so single runs vary by a factor of two;
alternating runs, in MB/s, the last two of each row and the pinned
budgets from the amended code:

| Server | Upload | Download | Peak footprint (MiB) | `503 memory_busy` | Other `5xx` |
|---|---|---|---|---|---|
| before (`ff53a33`) | 443, 376, 203, 256, 274, 383 | 299, 166, 529, 380, 274, 235 | 271 to 804 | 0 | 0 |
| after, default budget (4 GiB) | 297, 322, 422, 370, 322, 387 | 361, 324, 482, 554, 382, 384 | 376 to 1122 | 0 | 0 |
| after, `memory_budget = "1GiB"` | 297, 428 | 165, 204 | 536 to 709 | 0 | 0 |
| after, `memory_budget = "256MiB"` | 246, 338 | 514, 279 | 449 to 528 | 0 | 0 |

Throughput is the same within the noise, and no upload was refused,
even with the smallest budget allowed, where at most 3 xorbs are
buffered at once and the rest queue.

### The cost of the send deadline

Sending an answer copies it out in 64 KiB frames, so a stalled client
never pins the answer's buffers past its deadline. On the latency gate's
full 100k-term answer that costs about 5 ms at p50 locally (at load
average 13 to 21: p50 25.6 to 26.8 ms with the copy, 19.4 to 30.0 ms
without).

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
