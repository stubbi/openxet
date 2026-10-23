# S6: Shard upload verification rules

- Date: 2026-10-23
- Versions examined: `xet-core-structures` 1.7.0 (the pinned format crate),
  `xet-client` 1.7.0 (upstream's reference server,
  `cas_client/simulation/local_client.rs` and `local_server/handlers.rs`),
  `xet-data` 1.7.0 (the client's shard upload path), `hf_xet` 1.7.0 (run for
  real), and the spec pages
  [shard](https://huggingface.co/docs/xet/shard),
  [upload-protocol](https://huggingface.co/docs/xet/upload-protocol),
  [hashing](https://huggingface.co/docs/xet/hashing) and
  [api](https://huggingface.co/docs/xet/api) as published on 2026-10-23.
- Question (design §11): which rules must the server apply to an uploaded
  shard before registering its files, and do they hold for the reference
  files and for real clients?

## Answer

The rules below are implemented in `openxet-protocol` (`src/shard.rs`) as
`read_upload_shard`, `verify_segment` and `verify_file`, and enforced by
tests in `crates/protocol/tests/reference_files.rs` and
`crates/protocol/tests/hf_xet_upload.rs`.

- Parsing uses upstream's section parsers
  (`process_shard_file_info_section`, `process_shard_xorb_info_section`),
  header and footer types. No format code is reimplemented.
- Verification needs only the chunk hashes and lengths the server recorded
  when it accepted each xorb. It never reads a xorb back.
- The spec's reference upload shard and a shard captured from a real
  `hf_xet` 1.7.0 upload both pass every rule.

Upstream's reference server applies none of the verification rules (see
"Upstream vs. openxet"). openxet is stricter, but only by rules that
upstream's own client satisfies by construction, which the two stock shards
demonstrate.

## Rules

| # | Rule | Source | Test |
|---|---|---|---|
| 1 | The header has the shard magic and version 2. | [shard#1-header-mdbshardfileheader](https://huggingface.co/docs/xet/shard#1-header-mdbshardfileheader), [shard#version-compatibility](https://huggingface.co/docs/xet/shard#version-compatibility) | `wrong_header_version_is_rejected`, `truncated_shard_is_rejected` |
| 2 | The body is at most 64 MiB. Every section's claimed size is charged against that budget before anything is allocated for it. | [upload-protocol#6-shard-serialization-and-upload](https://huggingface.co/docs/xet/upload-protocol#6-shard-serialization-and-upload), [api#4-upload-shard](https://huggingface.co/docs/xet/api#4-upload-shard) ("the shard is too large") | `oversized_section_claim_is_rejected_before_allocation` |
| 3 | The body ends at the CAS info bookend with `footer_size` 0, or carries exactly the footer its header declares, with offsets that match the sections. Any other trailing bytes are rejected. | [shard#shard-upload](https://huggingface.co/docs/xet/shard#shard-upload) (footer MUST be omitted); tolerance from upstream, see below | `verification_shard_without_footer_parses_and_names_the_reference_file`, `footered_shard_parses_like_the_footerless_one`, `shard_serialized_with_lookup_tables_and_footer_parses`, `footerless_shard_with_trailing_bytes_is_rejected`, `shard_with_inconsistent_footer_is_rejected` |
| 4 | Every segment has a verification entry. | [shard#fileverificationentry-optional](https://huggingface.co/docs/xet/shard#fileverificationentry-optional) ("MUST be set for shard uploads"), [hashing#term-verification-hashes](https://huggingface.co/docs/xet/hashing#term-verification-hashes) | `shard_without_verification_entries_is_rejected_for_upload` |
| 5 | A segment's chunk range `[chunk_index_start, chunk_index_end)` is non-empty and inside the xorb. | [shard#filedatasequenceentry](https://huggingface.co/docs/xet/shard#filedatasequenceentry) (end-exclusive ranges) | `segment_out_of_xorb_bounds_is_rejected` |
| 6 | The verification hash equals the range hash of the stored chunk hashes for that range. | [hashing#term-verification-hashes](https://huggingface.co/docs/xet/hashing#term-verification-hashes) | `every_reference_segment_verifies_against_the_reference_xorb`, `tampered_verification_hash_is_rejected` |
| 7 | `unpacked_segment_bytes` equals the sum of the stored chunk lengths for that range. | [shard#filedatasequenceentry](https://huggingface.co/docs/xet/shard#filedatasequenceentry) ("term size when unpacked") | `tampered_unpacked_bytes_is_rejected` |
| 8 | Every xorb a segment references is already stored. | [upload-protocol#ordering-and-concurrency](https://huggingface.co/docs/xet/upload-protocol#ordering-and-concurrency), [api#4-upload-shard](https://huggingface.co/docs/xet/api#4-upload-shard) | `file_referencing_an_unknown_xorb_is_rejected` |
| 9 | The file hash recomputed from the referenced chunks, in segment order, equals the claimed one. | [hashing#file-hashes](https://huggingface.co/docs/xet/hashing#file-hashes) | `file_hash_recomputes_from_segments` |
| 10 | Stock clients' shards pass all of the above. | | `every_reference_segment_verifies_against_the_reference_xorb`, `hf_xet_upload_shard_verifies_against_its_xorb`, `hf_xet_upload_shard_names_its_files_and_xorb` |

Every `VerifyError` names the xorb and chunk range (or the file), and the
expected and actual values, so a client author can see which term is wrong.
`verify_segment` checks in this order: bounds, verification present, range
hash, length.

Rule 9 matters beyond integrity. Rules 5 to 7 only prove the client knows
each term's chunk hashes; without rule 9 a client could register any file
hash for those terms. A wrong file hash would then serve another file's
reconstruction.

Rule 8 for the xorbs listed in the CAS info section (not referenced by any
segment) is an index lookup. `UploadShard::xorbs` hands M1 the list.

### The stock-client shard

`scripts/capture-hf-xet-upload.py` runs `hf_xet.upload_files` (1.7.0, as
locked in `compat/uv.lock`) against a stub CAS server and stores the exact
xorb and shard bodies in `testdata/hf-xet-1.7.0-upload/`. It uploads three
seeded files, one of them empty. A rerun reproduces the bytes exactly.

The reference shard alone was not enough:
- it has one file with one segment and no empty file
- its provenance (which client, which version) isn't stated
- it can't show what a 1.7.0 client sends today

The capture showed two behaviours the reference shard doesn't have (the
empty file and the dedup prefix, both below). It also confirmed that
`hf_xet` 1.7.0 sends the spec's footer-less form with verification and
SHA-256 for every file.

## Upstream vs. openxet

Upstream's reference server (`xet-client` 1.7.0, `LocalClient::upload_shard`)
parses with `MDBMinimalShard::from_reader`, re-serializes and stores. That
is all it does:

| Check | Upstream reference server | openxet |
|---|---|---|
| Header version | not checked (`MDBShardFileHeader::deserialize` checks only the magic) | rule 1 |
| Size limit | none (`from_reader` passes `None` budgets) | rule 2 |
| Trailing bytes | ignored, whatever they are | rule 3 |
| Verification present | not checked; the sync parser doesn't even enforce the spec's "all files or none" (the async variant does) | rule 4, for every segment, so stricter than "all or none" |
| Range hash, lengths, bounds | not checked | rules 5 to 7 |
| Referenced xorbs exist | not checked at upload; `verify_integrity` (a test helper) checks later | rule 8 |
| File hash | not checked | rule 9 |
| Duplicate file hashes | the first entry wins (`from_reader` dedups) | all entries kept and each verified; M1 registers idempotently |

Hugging Face's production CAS validates in `validate_shard_from_async_read`
(named in the reference server's comments). That code isn't published, so
the spec's error list (api#4-upload-shard: "Shard contents failed
verification", missing xorb, too large) is the only description of it.

These checks can't break a stock client: upstream's shard builder derives
range hashes, lengths and file hashes from the same chunks it uploads, and
the stock shards pass. A client that fails them would produce files that
can't be reconstructed, or that claim content they don't have.

## Where the spec and upstream disagree

- **Footers on upload.** The spec says the footer "MUST be omitted", but the
  wasm build of `xet-data` 1.7.0 uploads `MDBInMemoryShard::to_bytes`
  (`processing/shard_interface/wasm.rs`): sections, lookup tables and a
  footer. Upstream's server accepts it. openxet accepts it too when the
  footer's size and offsets are consistent (rule 3), and discards the tail.
  The native client strips the footer and lookup tables
  (`read_shard_to_bytes_remove_footer`, `processing/shard_interface/native.rs`).
- **Empty files.** The spec says a File Info block has "at least 1 data
  sequence entry" ([shard#2-file-info-section](https://huggingface.co/docs/xet/shard#2-file-info-section)).
  `hf_xet` 1.7.0 uploads an empty file as a block with zero entries, file
  hash all zeros, and SHA-256 all zeros, with the verification and metadata
  flags set. Upstream's `file_hash` of no chunks is all zeros, so
  `verify_file` accepts it by rule 9 and rejects any other hash for an empty
  file.
- **SHA-256 byte order.** The spec calls the metadata field "the 32 byte
  SHA256 of the file contents"
  ([shard#filemetadataext-optional](https://huggingface.co/docs/xet/shard#filemetadataext-optional)).
  It is stored in the hash layout: its string form
  ([api#converting-hashes-to-strings](https://huggingface.co/docs/xet/api#converting-hashes-to-strings))
  is the digest's hex, and the raw bytes are the digest with each 8-byte
  group reversed. `ShardFileEntry::sha256` holds the real digest bytes.
  The reference shard (against `shasum -a 256` of the CSV) and the
  `hf_xet` capture (against Python's `hashlib`) both pin this.
- **CAS info fields.** Both stock shards set `num_bytes_on_disk` to 0, though
  the spec describes it as the serialized xorb length
  ([shard#caschunksequenceheader](https://huggingface.co/docs/xet/shard#caschunksequenceheader)).
  In the captured shard, `chunk_byte_range_start` is the chunk's offset in
  uncompressed bytes, not in the serialized xorb. openxet takes neither from
  the client.
- **Dedup prefix (outside S6, recorded for M1).** `hf_xet` 1.7.0 queries
  global dedup at `/v1/chunks/default/{hash}`. The spec says the only
  acceptable prefix is `default-merkledb`
  ([api#2-query-chunk-deduplication-global-deduplication](https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication)).
  The client also tries `POST /v2/shards` first, falls back to `/v1/shards`
  on 404, and then sends `POST /v1/telemetry`.

## Gaps where the spec is silent, and what openxet does

- **A footer declared in the header.** Accepted only if `footer_size` is
  upstream's footer size, the footer parses (version 1), and its
  `file_info_offset`, `cas_info_offset` and `footer_offset` match the bytes
  read. Lookup tables between the sections and the footer are discarded
  unread, as upstream's parser does.
- **Bytes after the bookend with no footer declared.** Rejected.
- **Empty chunk ranges** (`chunk_index_start == chunk_index_end`). Rejected
  as out of bounds: a term must cover data, and no stock client writes one.
- **Duplicate file entries.** Kept and each verified. Registering the same
  file hash twice is idempotent in M1.
- **The client's CAS info chunk listings.** Dropped by the parser. They feed
  global dedup, so trusting them would let one client poison dedup for
  everyone. M1 takes chunk hashes from xorb ingest instead.
- **SHA-256 verification.** Impossible without the file's bytes, so the
  value is client-asserted. M1 should keep the SHA-256 to file-hash mapping
  scoped to the repository whose writer uploaded it, never global. Otherwise
  one writer's claim could redirect another repository's LFS pointer.
- **Reserved flag bits** (`cas_flags`, other `file_flags` bits). Ignored.
- **The size limit** includes everything in the body, footer and lookup
  tables too.

## M1 index implication

Verification needs, per stored xorb, the ordered list of `(chunk hash,
uncompressed length)`: exactly `XorbChunks::hashes`, which xorb ingest
(`xorb_with_footer`, `read_xorb_chunks`) already computes.

- Store it in the index when a xorb is accepted, keyed by xorb hash.
- It is derived data. Like the rest of the index it can be rebuilt from the
  stored xorbs in the bucket.
- Shard ingest then runs in memory: one index read per referenced xorb, no
  object-store reads. Lesson from the competitive review of Shardline:
  never re-download xorbs to verify a shard.
- Global dedup's chunk-to-xorb entries come from the same ingest-time
  listing, never from a shard's CAS info section.
- `verify_file` takes a lookup closure, so M1 can batch the index reads per
  shard and pass a map.

Parsing is synchronous over a buffered body, so M1 runs it in
`spawn_blocking`, as S5 recommends.
