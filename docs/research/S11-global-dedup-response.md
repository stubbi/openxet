# S11: What a global dedup answer should offer

- Date: 2026-10-24. Implemented by Task 10b as ruling M1-R30; the
  sections "Implementation" and "Answer cost" record what it measured.
- Versions examined:
  - `xet-core` tag `v1.7.0` (`4b48571`): `xet_data` (`deduplication/`,
    `processing/deduplication_interface.rs`,
    `processing/shard_interface/native.rs`), `xet_core_structures`
    (`metadata_shard/shard_file_manager.rs`, `streaming_shard.rs`,
    `constants.rs`), `xet_client` (`cas_client/remote_client.rs`,
    `cas_client/simulation/{memory_client,local_client}.rs`,
    `simulation/local_server/handlers.rs`), `xet_runtime`
    (`config/groups/{data,shard}.rs`).
  - `hf_xet` 1.7.0 as installed in `compat/.venv`, run for real.
  - openxet `main` at `061c1ac`, and a scratch build of it with the
    experiment below (never committed); then the M1-R30 implementation on
    branch `m1/t10b-dedup-shard` (from `f2da565`).
  - The spec pages [deduplication](https://huggingface.co/docs/xet/deduplication),
    [shard](https://huggingface.co/docs/xet/shard#global-deduplication) and
    [api](https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication),
    as published on 2026-10-23/24, and the reference file
    `Electric_Vehicle_Population_Data_20250917.csv.shard.dedupe` at the
    revision `scripts/fetch-reference-files.sh` pins.
- Question (M1): a 1 MiB edit in the middle of a 100 MiB file, re-uploaded
  by stock `hf_xet` 1.7.0 with an empty client cache, sends about 30 MB
  of xorbs to openxet; with a warm cache, about 1.2 MB. Design §1a wants
  ≤ 2 MB. Why, and what should a dedup answer contain so the target holds?

Labels used below:
- **spec:** the spec page and section linked.
- **source:** read in `xet-core` at `v1.7.0`; paths relative to the
  repository root.
- **observed:** measured against `openxet serve`, see "Experiment" and
  "Implementation".

## Answer

The gap is the size of the answer, not the client. openxet's dedup answer
(ruling M1-R4) offers only the xorb that holds the queried chunk, so one
hit on a file's first chunk dedups only that file's first xorb; every
later xorb needs its own hit on a hash-eligible chunk (about 1 in 1024
chunks), and the client only dedups from the 8 MiB block in which that hit
happens. The spec describes an answer that offers many xorbs, and the
xet-core simulation server answers with the whole shard that registered
the chunk. With answers offering every xorb of the newest shard that
references the matched xorb, stock `hf_xet` 1.7.0 with an empty cache
sends (observed):

| File | Answer offers | Xorb bytes sent after a 1 MiB edit | Dedup answers |
|---|---|---|---|
| 100 MiB | 1 xorb (as `main`) | 30,090,089 | 2 × ~66 KB |
| 100 MiB | the shard's xorbs (2) | **1,202,561** | 1 × 101,648 B |
| 1 GiB | 1 xorb (as `main`) | 531,509,544 | 12 × ~66 KB |
| 1 GiB | at most 4 xorbs | 75,225,448 | 4 × ~264 KB |
| 1 GiB | at most 8 xorbs | 48,224,010 | 2 × ~530 KB |
| 1 GiB | the shard's xorbs (17) | **1,229,669** | 1 × 1,059,924 B |
| 100 MiB | M1-R30 as implemented (2) | **1,202,561** | 1 × 76,328 B |
| 1 GiB | M1-R30 as implemented (17) | **1,229,669** | 1 × 795,080 B |

The experiment rows kept the lookup tables; the implemented answer drops
them, which shrinks it by a quarter and sends the same xorb bytes.

The fresh-cache result then matches the warm-cache one (about 1.2 MB),
and meets §1a. A cap on xorbs per answer is costly: each window boundary
lost about 24 MB here. M1 should answer with the xorbs of the shard,
bounded by the spec's 64 MB shard size rather than a small xorb count
(see "Recommended design").

## What the spec says

- The answer is a shard "containing: **CAS Info Section**: Contains
  metadata about many xorbs that store chunks" (spec,
  [deduplication, Query Process](https://huggingface.co/docs/xet/deduplication)).
- "The CAS Info section returned by this API contains xorbs, where a xorb
  described in the CAS Info section contains the chunk that was queried.
  Clients can deduplicate their content against any of the other xorbs
  described in any CAS Info block in the CAS Info section of the returned
  shard. Other xorb descriptions returned in a shard are possibly more
  likely to reference content that the client has." (spec,
  [shard#global-deduplication](https://huggingface.co/docs/xet/shard#global-deduplication)).
- The file-info section is empty (spec, same section).
- "The global dedupe API is optimized to return information about nearby
  chunks when there is a match. Consider only issuing a request to an
  eligible chunk every ~4MB of data." (spec, deduplication, Eligibility
  Criteria). Shards have a maximum size of 64 MB (spec, deduplication,
  Shards).
- Eligible means: the first chunk of every file, or a chunk whose hash's
  last 8 bytes as a little-endian u64 are `% 1024 == 0` (spec, same).
- Security property: a match reveals which xorb holds a chunk the client
  has and where, "but has not revealed any other raw chunk hashes in that
  xorb or other xorbs" (spec, deduplication, HMAC Security Mechanism).
- The reference answer `…csv.shard.dedupe` (observed, parsed): footer
  version 1, keyed, `shard_key_expiry` 21 days after its creation
  timestamp, no file entries, no xorb or chunk lookup tables, and one xorb
  of 796 chunks. Its file is a single xorb, so it shows neither one nor
  many xorbs per answer.

The spec doesn't say which xorbs beyond the matched one, nor a count.

## What the client does with an answer (source)

- **Which chunks it asks for** (`xet_data/src/deduplication/file_deduplication.rs:149-176`):
  in the first of at most two passes over each ingestion block
  (`INGESTION_BLOCK_SIZE` = 8 MiB, `xet_data/src/processing/constants.rs:31`),
  for every chunk not already matched by a known shard that is the file's
  first chunk or `hash_is_global_dedup_eligible`. A matched chunk is never
  queried; the scan skips the matched run.
- **Spacing:** the deduper's `min_spacing_between_global_dedup_queries`
  is initialized to 0 (`file_deduplication.rs:74`) and nothing in the tag
  sets it; the 256-chunk config value (`xet_runtime/src/config/groups/data.rs:14`)
  has no reader in `xet_data`. At 1.7.0 every eligible unmatched chunk is
  queried (source; not isolated by observation). S10 §2 said "at most once
  per 256 chunks", which describes the config value, not this behaviour;
  Task 10b corrected it.
- **Second pass:** queries run in the background
  (`processing/deduplication_interface.rs:49-82`). After the first pass
  the block waits for all of them; if any returned a shard, every
  still-unmatched chunk of the **same block** is matched again
  (`file_deduplication.rs:127-186`). Blocks already processed are not
  revisited, so dedup starts at the block that held the hit.
- **Storing it:** `query_dedup_shard_by_chunk` imports the answer into the
  shard cache directory (`processing/shard_interface/native.rs:148-162`,
  `ShardFileManager::import_shard_from_bytes`, `shard_file_manager.rs:208`)
  and indexes every chunk of every xorb in it under its keyed truncated
  hash (`register_shards`, `shard_file_manager.rs:247-360`). Every
  offered xorb is used; no xorb count is checked.
- **Limits:** no cap on the answer's size (`remote_client.rs:775-781`
  reads the whole body). Chunks past index `u16::MAX` within one xorb are
  not indexed (`shard_file_manager.rs:325`), which `MAX_XORB_CHUNKS` = 8192
  never reaches. Indexing stops once a session holds
  `chunk_index_table_max_size` = 64 Mi chunks (`config/groups/shard.rs:24`).
  The cache is pruned to `cache_size_limit` = 16 GB at session start
  (`shard.rs:16`).
- **Lifetime:** a shard in the cache is skipped on later sessions once its
  `shard_key_expiry` passes (S10 §2); the client's own shards are written
  with a 3-week expiry (`MDB_SHARD_LOCAL_CACHE_EXPIRATION`,
  `processing/constants.rs`).

## What the simulation server answers (source)

The in-process backends `xet-core` tests against answer with the shard
that registered the chunk:

- `MemoryClient::upload_shard` maps every chunk from
  `MDBMinimalShard::global_dedup_eligible_chunks()` (each xorb's
  hash-eligible or flagged chunks, plus each file's first chunk) to the
  uploaded shard's bytes; a later upload overwrites the mapping
  (`simulation/memory_client.rs:921-927`). `LocalClient` keeps chunk →
  shard hash in a `global_dedup` table (`simulation/local_client.rs:1712-1725`).
- `query_for_global_dedup_shard` returns that shard. With an expiration
  configured, it is reserialized with
  `serialize_xorb_subset_with_expiry(…, |_| true)`: **every xorb of the
  shard, no file-info section**, the expiry in the footer
  (`memory_client.rs:861-885`, `local_client.rs:1628-1660`). Without one,
  the stored shard is returned as uploaded.
- The local HTTP server's `get_dedup_info_by_chunk` returns that body or
  `404` (`simulation/local_server/handlers.rs:513-523`).
- `global_dedup_eligible_chunks` covers only xorbs listed in the shard's
  own xorb section (`streaming_shard.rs:553-596`): a shard that only
  dedups against older xorbs registers no candidates for them.

The simulation is a test backend, so this is how `xet-core` expects
answers to look, not a statement about huggingface.co's production
service, which this note did not query.

## Experiment

Scratch worktree of `main` (`061c1ac`), never committed; the patch is
kept outside the repository.
- Index: a table of the xorbs each registered shard references through
  its files' terms, in term order, written by `register_shard`.
- Route: the dedup query resolves the chunk to a xorb as now
  (`dedup_xorb`), then to the newest registered shard referencing that
  xorb, and offers that shard's xorbs (filtered through `chunk_tables`,
  so visibility applies), keyed and built as now
  (`export_as_keyed_shard_streaming`, no file info, lookup tables on,
  fresh key, 7-day expiry). `OPENXET_EXP_CAP=N` limits the answer to N
  xorbs starting at the matched one; `N=1` is `main`'s answer.
- Measurement: `openxet serve` in dev mode, release build. Upload a file
  of seeded random bytes with `hf_xet.upload_files` (client cache A), then
  replace the 1 MiB at the middle with other random bytes and upload that
  with a new `HF_XET_CACHE` (cache B). "Xorb bytes sent" is the growth of
  the bucket's `xorbs/` directory during the second upload; the server
  stores the xorb bodies it is sent, and a xorb the client skips is never
  sent. The `N=1` run (30,090,089) reproduces the 30,130,924 measured
  against `main` within the noise of different random content.
- Every upload succeeded, including the shards that reference xorbs the
  second client learned only from the answer, so S6 verification accepts
  them.

Why the capped runs lose about 24 MB per boundary (source, consistent
with the counts above): after the window, the next xorb's chunks are
unmatched until the client queries an eligible chunk in it. An 8 MiB
block of about 128 chunks contains no eligible chunk with probability
(1 − 1/1024)^128 ≈ 0.88, and only the block holding the hit is matched
again, so the blocks before it are uploaded.

## Recommended design (M1)

Ruling M1-R30 adopts this design; "Implementation" records where the code
differs in detail.

1. **Content.** Offer the xorbs of the newest visible shard that
   references the xorb holding the queried chunk, in that shard's term
   order, starting at the matched xorb, then forward, then the xorbs
   before it. Fall back to the single xorb (today's answer) when no
   visible shard references it yet. This supersedes the "single xorb"
   part of M1-R4; the fresh key and 7-day expiry stay.
2. **Bound.** Stop adding xorbs at a 64 MiB answer, the spec's shard
   maximum, and omit the xorb and chunk lookup tables, as the reference
   answer does (the client rebuilds its index from the xorb section; both
   forms dedup, S10 §2). Cost per chunk is then 48 B: observed density
   was about 62 KB per xorb with lookup tables, so about 47 KB without.
   A 10 GB file (about 160 xorbs) answers in about 7.5 MB; the 64 MiB
   bound holds about 1,400 such xorbs (about 90 GB of file), or 170 xorbs
   of the 8192-chunk maximum. Past the bound the client's next eligible
   hit starts a new answer, at the boundary cost measured above. A count
   cap such as 4 or 8 xorbs misses §1a by a factor of 25 or more.
3. **Index.** A `shard_xorbs (shard, pos, xorb)` table with an index on
   `xorb`, written in `register_shard`'s transaction from the terms it
   already has, plus the registration order. One row per distinct xorb
   per shard (a file's xorb count, against one dedup row per ~1024
   chunks). It is rebuildable from the stored shards, so the bucket stays
   the source of truth. The rejected alternative, chunk → shard rows as in
   the simulation, needs a row per candidate per shard and gives the same
   answer; file → xorbs gives one file's xorbs where the spec's hint is
   the uploader's whole batch.
4. **Visibility.** Under `PerScope`, the shard must be registered to the
   caller's scope (`shard_scopes`) and every offered xorb must be visible
   to it, as `chunk_tables` filters today; an invisible xorb is dropped,
   never named. Under `Global`, any shard.
5. **Cost** (estimated here; "Answer cost" has the measurement). One more
   indexed lookup per query and up to 64 MiB built per
   answer. Building and keying a full 8192-chunk table takes about 1 ms (release, Apple
   M3, as measured for `crates/cas/src/dedup.rs`), so a 160-xorb answer is about 20 ms of CPU under the `validate`
   permit (M1-R25). Answers can't be cached: each has a fresh key.

## Implementation (Task 10b, ruling M1-R30)

- **Index.** `shard_xorbs (shard, pos, xorb)` with an index on `xorb`,
  written by `register_shard` in its transaction: the distinct xorbs of
  the shard's files' terms in order of first reference, which is
  `UploadShard::referenced_xorbs` order. "Newest" is a monotonic
  `registered_seq` on `shard_scopes`, taken as `MAX + 1` under the write
  lock, so it never depends on wall time. `dedup_shard_xorbs(xorb, scope,
  vis)` picks the newest shard referencing the xorb (under `PerScope`
  registered to the caller's scope, ordered by that scope's
  registrations; under `Global` by any), and lists its xorbs visible to
  the caller, in one statement. Migration `0002_shard_xorbs.sql`; shards
  registered before it have no rows, so their answers fall back to the
  single xorb until they are registered again or reindexed.
- **Order.** The route rotates the shard's term order to start at the
  matched xorb and passes that priority order to `dedup_shard`, which
  takes xorbs in it until the budget is spent. The answer's xorb section
  itself is in upstream's order (by xorb hash): `MDBInMemoryShard` keeps
  xorbs in a `BTreeMap`, and writing another order would mean writing the
  section by hand (ADR 0001). The client indexes every offered chunk
  whatever the order (see "What the client does with an answer"), so only
  which xorbs make the cut depends on the term order.
- **Budget.** `MAX_DEDUP_ANSWER_BYTES` = 64 MiB. `dedup_shard` counts the
  exact serialized size (header, empty sections and footer, plus
  upstream's `MDBXorbInfo::num_bytes` per xorb, 48 B per chunk plus 48 B
  per xorb), always takes the first xorb and stops before the next one
  that would exceed the budget. The route loads chunk tables 64 xorbs per
  index read and stops once the loaded tables fill the budget, so an
  answer holds at most about 64 MiB of tables plus one read whatever the
  shard references.
- **Lookup tables.** Omitted through upstream's own flags
  (`export_as_keyed_shard_streaming(…, include_xorb_lookup_table: false,
  include_chunk_lookup_table: false)`); no format code was written.
  Stock `hf_xet` 1.7.0 deduped against them as before (rows above).
- **Permit.** Loading the tables and building the answer hold one
  `validate` permit (M1-R25); the test
  `building_the_answer_takes_a_validation_permit` checks the tables load
  with none free.
- **End-to-end test.** `fresh_cache_edit_dedups_against_the_server`
  (`crates/openxet/tests/transfer_cmd.rs`) uploads a seeded 100 MiB file
  with `openxet upload`, edits 1 MiB in the middle and re-uploads with a
  fresh cache: 1,091,178 bytes of xorbs were accepted (observed; the
  `main` route accepted about 38 MB). It takes 11 s in a debug build, so it
  runs in the ordinary test job.

## Answer cost

Measured in a release build on an Apple M3, with a scratch test (not
committed) that registers a one-file shard of N synthetic xorbs of 1000
chunks each straight in the index and times 30 sequential queries of the
file's first chunk over HTTP against the CAS test server, plus 30 calls
of `dedup_shard` alone (observed):

| Shard | Chunks | Answer | `dedup_shard` median | HTTP median (p90) |
|---|---|---|---|---|
| 17 xorbs, like a 1 GiB file | 17,000 | 817,160 B | 2.5 ms | 5.9 ms (13 ms) |
| 160 xorbs, like a 10 GB file | 160,000 | 7,688,024 B | 32 ms | 50 ms (98 ms) |

In the 1 GiB run with `hf_xet` above, the server logged the 17-xorb
answer at 14 ms while it was also ingesting the upload. The cost is about
0.2 µs per chunk of building and keying, plus the index read. A full
64 MiB answer (about 1.4 million chunks) would take about 0.3 s of one
permit; answers can't be cached, as each has a fresh key.

## Security

- **Keyed hashes only.** The answer is built by the same
  `export_as_keyed_shard_streaming` call; only xorb hashes are plain and
  every chunk hash is keyed. The file-info section stays empty
  (`include_file_info = false`, spec). Tests should assert
  `read_dedup_answer(..).files == 0` and that no raw chunk hash of any
  offered xorb appears in the body, for multi-xorb answers too.
- **What a larger answer reveals under `Global`.** Anyone holding one
  candidate chunk of a stored file learns, for every xorb of the shard
  that referenced it: the xorb hash, its chunk count, each chunk's length,
  and keyed hashes that let it test, until the key expires, whether it
  holds any of those chunks. Today that holds for one xorb; the change
  widens it to the shard. It reveals no raw hash and no data: reading a
  xorb still needs a file registered in the caller's scope, and
  registering one needs the raw chunk hashes of every referenced range
  (shard verification, S6). This is the model the spec describes ("any
  of the other xorbs" in the answer) and the simulation implements, and
  `Global` is documented as following it (design §5.4); operators for
  whom it is too much use `PerScope`, where the answer stays inside the
  scope.
- **No credential, key or file name in the answer,** as now.

## Client cache and GC (M1-R5)

- Each answer pins more xorbs in clients' caches: every offered xorb may
  be referenced by uploads for the key's lifetime (7 days; the client
  skips the cached answer after its expiry, S10 §2). M1-R5's grace floor
  of 28 days therefore holds per offered xorb with no change.
- GC (M4) must not offer xorbs that are pending deletion. If the dedup
  query only offers xorbs some live file references, every offer happens
  before the xorb becomes unreferenced, and the grace period, counted
  from then, outlasts the key. The `shard_xorbs` rows of a shard whose
  files are all gone must be dropped with it.
- A client whose cached answer outlives a deleted xorb gets M1-R5's
  stale-cache error; a larger answer makes that more likely but not
  different in kind.

## Not verified

- huggingface.co's production answer for a multi-xorb file (which xorbs,
  how many, whether lookup tables are present). The spec and the
  simulation agree on "many xorbs"; the production service was not
  queried for this note. Task 10b could not query it either: neither the
  `HF_COMPAT_TOKEN` secret nor a signed-in 1Password session was available
  without a prompt, so this stays **unverified**.
- The query spacing was read in source but not counted on the wire; the
  experiment logged only matches.
- Uploads of files larger than 1 GiB through a real client (only the
  answer cost of a 160-xorb shard was measured, above), and many-file shards (an upload of many small
  files), where the newest shard referencing a xorb may belong to a
  different file than the queried one.
