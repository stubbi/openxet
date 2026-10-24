# S11: What a global dedup answer should offer

<!-- description: Why a global dedup answer should offer a shard's xorbs rather than one xorb, measured against the spec, xet-core and huggingface.co. -->

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
  - huggingface.co's production CAS (`cas-server.xethub.hf.co`), queried
    read-only on 2026-10-24, parsed with `xet-core-structures` 1.7.0; see
    "Observed on huggingface.co".
- Question (M1): a 1 MiB edit in the middle of a 100 MiB file, re-uploaded
  by stock `hf_xet` 1.7.0 with an empty client cache, sends about 30 MB
  of xorbs to openxet; with a warm cache, about 1.2 MB. Design §1a wants
  ≤ 2 MB. Why, and what should a dedup answer contain so the target holds?

Labels used below:
- **spec:** the spec page and section linked.
- **source:** read in `xet-core` at `v1.7.0`; paths relative to the
  repository root.
- **observed:** measured against `openxet serve`, see "Experiment" and
  "Implementation"; or, where the text says so, against huggingface.co,
  see "Observed on huggingface.co".

## Answer

The gap is the size of the answer, not the client. openxet's dedup answer
(ruling M1-R4) offers only the xorb that holds the queried chunk, so one
hit on a file's first chunk dedups only that file's first xorb; every
later xorb needs its own hit on a hash-eligible chunk (about 1 in 1024
chunks), and the client only dedups from the 8 MiB block in which that hit
happens. The spec describes an answer that offers many xorbs, and the
xet-core simulation server answers with the whole shard that registered
the chunk; huggingface.co answers with a shard's xorbs too (8 and 109
xorbs observed, see "Observed on huggingface.co"). With answers offering every xorb of the newest shard that
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
answers to look; the next section records what huggingface.co's
production service answers.

## Observed on huggingface.co (2026-10-24)

- **Setup.** File `model.safetensors` of the public model
  `openai-community/gpt2` (548,105,171 bytes, `X-Xet-Hash`
  `63bed808…8758`). Its v2 reconstruction has 9 terms, each a whole xorb
  (205 to 1,112 chunks, 8,564 chunks in all). All 9 xorbs were fetched
  through the reconstruction's URLs and every chunk decoded and hashed
  (`deserialize_chunk`, `compute_data_hash`); the chunk sizes add up to
  the file size. The file has 12 hash-eligible chunks besides its first.
  A CAS read token came from `GET /api/models/{repo}/xet-read-token/main`
  (S3). Only `GET` and `HEAD` requests were sent; answers were parsed
  with `MDBShardInfo` from `xet-core-structures` 1.7.0.
- **Queries.** `GET /v1/chunks/default/{hash}` for the first chunk (three
  times, and with a second read token), for all 12 eligible chunks, and
  once with the spec's prefix `default-merkledb`, which answered the same
  bytes as `default`. A chunk of the file that is not eligible got `404`
  with a short text body. `200` answers are `application/octet-stream`
  with `Content-Length`, no caching or expiry headers.
- **Two distinct answers.** The first chunk and one eligible chunk
  returned answer A; the other 11 eligible chunks returned answer B.
  Repeated queries returned the same bytes except the footer's creation
  and expiry timestamps (2 or 3 bytes differ), so the answer does not
  depend on which of the shard's chunks was queried.

| | Answer A | Answer B |
|---|---|---|
| Body | 5,244,056 B | 458,792 B |
| Xorbs / chunks | 109 / 109,135 | 8 / 9,543 |
| File-info entries | 0 | 0 |
| Xorb and chunk lookup tables | none | none |
| Order of the xorb section | by xorb hash | not by hash, not the file's term order |
| Position of the matched xorb | 103 of 109 | 0 to 5, varying with the query |
| Of the file's own 9 xorbs | none | none |
| File bytes its keyed hashes match | 514,020,878 (93.8 %), through 13 of its xorbs | 387,312,731 (70.7 %), through 7 of its xorbs |

- **Shape.** Header version 2, footer version 1, an empty file-info
  section. Each body is exactly header, empty file-info bookend, xorb
  section and footer (48 B per xorb and per chunk, plus 296 B), so no
  lookup tables. The footer's `stored_bytes` equals the sum of the offered
  xorbs' unpacked sizes, so the footer describes the answer, not a larger
  original. Answer A has `num_bytes_on_disk` 0 for every xorb; answer B
  carries on-disk sizes.
- **Keying.** Every chunk hash is keyed: the raw hash of the queried
  chunk appears nowhere in the body, and
  `MDBShardInfo::keyed_chunk_hash` of it is found in the matched xorb.
  `chunk_hash_hmac_key` is non-zero, and it was the same in all 16
  answers: both shards, both read tokens, over about three minutes.
  Whether it differs per account or changes over a longer period was not
  tested.
- **Expiry.** `shard_key_expiry` is exactly 7 days (604,800 s) after
  `shard_creation_timestamp`, which is the time of the response.
- **Which xorbs.** Neither answer offers any of the queried file's own
  xorbs. The file's content is also stored in other xorbs on the service,
  and the answer is a shard containing the queried chunk, not
  necessarily one registered by the upload of the file it came from.
  Together the two answers match 541,327,375 bytes (98.8 %) of the file.
- **Size and time.** No count cap is visible: 8 and 109 xorbs, both far
  below the spec's 64 MB shard maximum. Measured from the client, answer A
  took 1.6 to 2.0 s, answer B 0.67 to 0.79 s and the `404` 0.26 s,
  transfer included.

What this means for M1-R30:
- **Granularity matches.** huggingface.co offers a shard's xorbs, not the
  single matched xorb and not a small fixed count; that is the content
  M1-R30 chose. The larger observed answer, 109 xorbs in 5.2 MB, is a
  third of openxet's 16 MiB budget (ruling M1-R32), so openxet would
  offer it whole; a
  shard past the budget is where openxet's answer is cut, a size not seen
  here.
- **Form matches:** no file info, no lookup tables, every chunk hash
  keyed, a 7-day expiry (M1-R4 already used 7 days), and `404` when
  nothing matches. The non-eligible chunk's `404` is consistent with an
  index of eligible and first chunks only, which is what openxet's dedup
  index records (one sample).
- **Order and key differ, without effect on dedup.** huggingface.co does
  not put the matched xorb first and keeps one key across answers;
  openxet orders by the matched xorb only to choose which xorbs fit the
  budget, and keys each answer freshly. The client indexes every offered
  chunk whatever the order and keys each cached shard by its own footer
  (see "What the client does with an answer").
- **Which shard.** openxet resolves chunk → xorb → the newest visible
  shard referencing that xorb; huggingface.co returned a shard that
  holds the chunk, here not the queried file's own. When a chunk is
  stored in one xorb only, both arrive at the shards that reference it.
  For dedup against openxet's own
  data, M1-R30 is therefore at least as good as the observed answer.

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
2. **Bound** (64 MiB here; ruling M1-R32 set 16 MiB after review, see
   "Implementation"). Stop adding xorbs at a 64 MiB answer, the spec's shard
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
- **Budget.** `DEDUP_ANSWER_BUDGET` = 16 MiB (ruling M1-R32; Task 10b
  first shipped 64 MiB, the spec's shard maximum, which review found
  costly for no measured gain). At 48 B per chunk, 16 MiB offers about
  350 xorbs of the typical density (about 47 KB of answer each), about
  22 GB of file. `dedup_shard` counts the exact serialized size (header,
  empty sections and footer, plus upstream's `MDBXorbInfo::num_bytes` per
  xorb, 48 B per chunk plus 48 B per xorb), always takes the first xorb
  and stops before the next one that would exceed the budget. The route
  loads chunk tables 32 xorbs per index read and stops at the first table
  that fills the budget.
- **Lookup tables.** Omitted through upstream's own flags
  (`export_as_keyed_shard_streaming(…, include_xorb_lookup_table: false,
  include_chunk_lookup_table: false)`); no format code was written.
  Stock `hf_xet` 1.7.0 deduped against them as before (rows above).
- **Permits.** Each answer holds one permit of its own pool,
  `AppState::dedup`, a quarter of the CPUs and at least one (ruling
  M1-R32), from the table load until its body has been written or
  dropped: the body owns the permit (`Bytes::from_owner`). Dedup never
  takes a `validate` permit, so it can't starve xorb or shard validation
  (`dedup_never_starves_validation`); a client that never reads keeps its
  permit, so bodies can't pile up
  (`the_dedup_permit_lives_until_the_body_is_gone`).
- **Peak per answer.** `dedup_shard` consumes the tables, dropping each
  once copied into upstream's in-memory shard; serializes only the
  sections the keyed export reads; and drops the in-memory shard before
  `UnkeyedDedupShard::keyed` keys the serialization into an exactly sized
  buffer. The peak is during upstream's serialization: the in-memory shard
  (48 B per chunk plus its chunk index, about 100 B per chunk) and the
  serialization, about 55 MB for a 16 MiB answer (estimate from those
  sizes).
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
16 MiB answer (about 345,000 chunks) takes about 80 ms of one permit;
answers can't be cached, as each has a fresh key.

**Memory** (ruling M1-R32, measured after review I1). Release build,
Apple M3 with 8 CPUs, so 2 `dedup` permits; one shard of 200 xorbs of
8192 chunks, so every answer is a full 16.5 MB. True peak RSS, from
`/usr/bin/time -l`, by the re-review; the idle process after setup is
38.5 MB (observed, two runs each):

| Load | Peak RSS after the fix | Before (64 MiB, `validate` permits) |
|---|---|---|
| 1 answer | 160 MB | about 0.55 GB |
| 8 concurrent, reading | 292-337 MB | 2.05 GB |
| 16 concurrent, reading | 308-310 MB | 2.8 GB |
| 32 connections that never read | 241-258 MB | 2.68 GB |
| 64 connections that never read | 246-248 MB | 3.2 GB |

Over idle that is about 120 MB for one answer and about 250 to 300 MB
with both permits busy. Load beyond the permits waits for one, holding
only its request, so memory stays flat from 2 to 64 clients. Per process,
dedup memory is at most the `dedup` permits × about 150 MB, whatever the
number of clients. (The implementer's first figures, 5 ms RSS sampling,
missed short peaks and read about a third lower.)

**Stalled clients** (ruling M1-R33). With the permits held by bodies a
client never reads, other queries would wait without limit, and stock
`hf_xet` waits for its block's queries for up to its 300 s read timeout.
So a query waits at most 1 s for a permit and then answers `429
dedup_busy` (`Retry-After: 1`), which `hf_xet` takes as "no match"
without retrying (S10 §2); and an answer must be sent within 30 s plus
its length at 1 MiB/s (46 s for 16 MiB), after which a timer frees the
answer and its permit. Observed with a scratch test (release, not
committed; both permits held by sockets that never read; a 2 s test
deadline): a third query answered `429` after 1.003 s, and after the
deadline it answered `200` with the full 15.7 MB.

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

- huggingface.co's answer was observed for one file whose content is
  stored more than once on the service (see "Observed on huggingface.co").
  Not observed: an answer for content stored only once, a shard larger
  than 64 MB, and whether the HMAC key differs per account or over days.
- The query spacing was read in source but not counted on the wire; the
  experiment logged only matches.
- Uploads of files larger than 1 GiB through a real client (only the
  answer cost of a 160-xorb shard was measured, above), and many-file shards (an upload of many small
  files), where the newest shard referencing a xorb may belong to a
  different file than the queried one.
