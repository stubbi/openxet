# S5: Is `xet-core-structures` usable server-side?

- Date: 2026-10-23
- Version examined: `xet-core-structures` 1.7.0 (crates.io, 2026-10-06), the
  exact version `openxet-protocol` pins. Paths below are relative to that
  crate's `src/`. Upstream `main` was at `d26341d4` (2026-10-22).
- Question (design §11, ADR 0001): can openxet use upstream's crate for
  every format, hashing and shard operation the server needs, or must some
  piece be reimplemented?

## Answer

Yes. Everything the server needs from the M0–M2 plans is public in 1.7.0.
Nothing has to be reimplemented, so this spike proposes no ADR 0002.

The costs are a heavy transitive graph (`xet-runtime`), sync-only parsers
for tokio code, and an API that breaks often within 1.x. The exact pin and
the single `openxet-protocol` doorway (ADR 0001) contain these costs.

## 1. What it pulls in, measured

`xet-core-structures` depends unconditionally on `xet-runtime` 1.7.0, the
client runtime. That brings:
- an HTTP client: `reqwest` with `hyper`, `default-features = false`, so no
  TLS backend
- `sysinfo`, `whoami`, `dirs`, `shellexpand`, `chrono`, `colored`
- `git-version`, `ctor`
- `tracing-subscriber`, `tracing-appender`, `tokio`, `tokio-util`

The format code uses `xet-runtime` only for error variants
(`error.rs`: `RuntimeError`, `RwTaskLockError`) and the
`test_configurable_constants!` macro (`xorb_object/constants.rs`,
`metadata_shard/constants.rs`). The client-cache modules
(`metadata_shard/shard_file_manager.rs`, `session_directory.rs`) use
`XetContext` and `XetRuntime`; the server doesn't need those.

`cargo tree -p openxet-protocol -e normal` lists 205 unique crates
(the protocol crate plus 204 dependencies).

Measured on an Apple M3 (8 cores) with Rust 1.97.1, using two scratch
binaries kept outside the repo. Both read stdin. The second also runs
`deserialize_chunks`, `compute_data_hash`, `xorb_hash`, `file_hash` and
`range_hash_from_chunks` on it, so the linker keeps the code.

| | without dependency | with `xet-core-structures =1.7.0` | delta |
|---|---|---|---|
| clean `cargo build --release` | 2.7 s | 139.7 s, 198.0 s (two runs) | +137 to +195 s |
| rebuild after touching `main.rs` | 0.16 s | 2.6 s | +2.5 s |
| binary size (default release profile) | 432,624 B | 523,744 B | +91,120 B (+21%) |
| dynamically linked libraries | libSystem | + SystemConfiguration, IOKit, OpenDirectory, Foundation, CoreFoundation, libobjc, libiconv | +7 |
| static initializers (`__mod_init_func`) | 0 | 1 (`ctor::priority_ctor`, the `ctor` crate's own hook) | +1 |

The size cost is small because dead-code elimination drops the unused
runtime; the compile-time cost is not. The build time is a one-off cost per
clean CI cache, and `Swatinem/rust-cache` hides it in CI. The binary
produced the reference xorb's hashes correctly (xorb `eea25d6e…`, file
`118a5332…`, range `d81c11b1…`).

## 2. Is the server-side shard and xorb API public?

Each function or type the server needs:

| Need | Function or type (1.7.0) | Public |
|---|---|---|
| Hash string form | `merklehash::DataHash::hex`, `from_hex` (`merklehash/data_hash.rs`) | yes |
| Chunk, xorb and file hashes | `merklehash::{compute_data_hash, xorb_hash, file_hash, file_hash_with_salt}` | yes |
| Range verification hash | `metadata_shard::chunk_verification::range_hash_from_chunks` | yes |
| Validate a xorb upload (`POST /v1/xorbs`), footer optional, and build the stored form | `xorb_object::{deserialize_chunk_header, CompressionScheme::decompress_from_reader, XorbObjectInfoV1::deserialize}` to validate, then `reconstruct_xorb_with_footer` (`xorb_object/xorb_object_format.rs`) for the stored form; composed as `openxet_protocol::xorb_with_footer` | yes |
| Parse a footer-less chunk sequence (no footer allowed) | `xorb_object::{deserialize_chunk_header, CompressionScheme::decompress_from_reader}`, the same bounded loop as uploads; composed as `openxet_protocol::read_xorb_chunks` | yes |
| Async xorb parse | `deserialize_chunks_from_async_read`, `deserialize_chunks_from_stream` (`xorb_chunk_format/deserialize_async.rs`) | yes |
| Read a stored xorb's footer for range reads | `XorbObject::deserialize`, `get_byte_offset` (`xorb_object/xorb_object_format.rs`) | yes |
| Parse a footer-less shard upload | `metadata_shard::streaming_shard::MDBMinimalShard::from_reader`; `shard_in_memory::MDBInMemoryShard::from_reader` | yes |
| Parse with a size budget (untrusted input) | `streaming_shard::process_shard_file_info_section`, `process_shard_xorb_info_section` (`size_limit: Option<u64>`) | yes |
| Read file entries and verification entries | `file_structs::MDBFileInfoView::{entry, verification, file_hash, contains_verification}`, `FileVerificationEntry::range_hash` | yes |
| Write a shard with footer and lookup tables | `shard_format::MDBShardInfo::serialize_from(writer, &MDBInMemoryShard, expiry)`; `MDBInMemoryShard::{add_xorb_block, add_file_reconstruction_info}` | yes |
| Dedup response: shard with footer, HMAC-keyed chunk hashes (lesson 6) | `MDBShardInfo::export_as_keyed_shard`, `export_as_keyed_shard_streaming` (take `hmac_key`, `key_valid_for`, include flags; input must be a footered shard, `Read + Seek`) | yes |
| Dedup response from an xorb subset | `MDBMinimalShard::serialize_xorb_subset_with_expiry` (footer, no HMAC) | yes |
| HMAC of one hash | `DataHash::hmac`; `MDBShardInfo::keyed_chunk_hash`, `chunk_hmac_key` | yes |
| Global-dedup eligibility | `metadata_shard::hash_is_global_dedup_eligible`; `MDBMinimalShard::global_dedup_eligible_chunks`; `MDBShardInfo::filter_cas_chunks_for_global_dedup` | yes |

What upstream doesn't provide, which we compose ourselves without
reimplementing the format:

- **Shard upload verification rules (S6).** No upstream function checks a
  shard's verification entries against stored chunk hashes. It composes
  from `MDBFileInfoView::verification`, the stored chunk hashes and
  `range_hash_from_chunks`.
- **Covering "every xorb of the files containing the chunk" (lesson 6).**
  This is index logic: build an `MDBInMemoryShard` from the index, then
  `serialize_from` followed by `export_as_keyed_shard`.
- **Xorb upload validation and footers.** Upstream's in-process test
  backend accepts an optional footer, so openxet does too:
  - `xet-client` 1.7.0 `cas_client/simulation/local_server/handlers.rs`
    (`post_xorb`) passes the body with `footer_start: None`.
  - `local_client.rs` (`upload_xorb`) then calls
    `reconstruct_xorb_with_footer`, compares the computed hash with the path
    hash, and stores the result: chunks plus a recomputed footer.
  - openxet's `xorb_with_footer` applies its own checks first, composed from
    upstream primitives (ADR 0001): each chunk header through
    `deserialize_chunk_header`, decompression into a buffer bounded by the
    header's uncompressed length (at most 128 KiB) with an exact length
    match, 1 to 8192 chunks, and a body of at most 64 MiB plus framing.
  - A header position that starts with the footer ident `XETBLOB`
    (`parse_chunk_header` returns `ChunkHeaderParse`) ends the chunks. What
    follows must be exactly one footer: `XorbObjectInfoV1::deserialize`
    reads it, its trailing length must match, and nothing may follow it.
  - Only then does `reconstruct_xorb_with_footer` write the stored form from
    the validated chunk section, with a footer computed from the chunks.
  - Other trailing bytes are rejected. The reference file's 4 bytes `XETB`
    fail with `UnexpectedEof`.
  - `read_xorb_chunks` runs the same bounded loop but rejects a footer, so
    it serves only footer-less contexts.
  - Clients send no footer: `xet-data` calls `from_xorb(xorb, false, …)`.
    The tolerance covers clients that serialize with `serialize_footer = true`.
  - The protocol crate's tests pin this: footer-less, client-footered and
    re-submitted stored forms all hash to `eea25d6e…`, and the stray-`XETB`
    form is rejected. `tests/xorb_limits.rs` covers each limit.
- **Shard trailing bytes.** `MDBMinimalShard::from_reader` and
  `MDBInMemoryShard::from_reader` stop at the xorb-section bookend and don't
  read or check what follows, so a footer-less upload and a footered one
  parse alike. S6 decides whether shard uploads may carry a footer or must
  end at the bookend.
- **Size limits.** `from_reader` passes `None` (unlimited) to the section
  parsers. For untrusted uploads, call the `process_shard_*_section`
  functions with a budget instead.

### Reference-file findings

- The reference xorb is 796 chunks followed by the 4 bytes `XETB` (a
  truncated footer ident). It is neither the footer-less form clients upload
  nor a valid footered xorb.
  - Clients send footer-less xorbs: `xet-data` 1.7.0,
    `processing/file_upload_session.rs`, calls `SerializedXorbObject::from_xorb(xorb, false, …)`.
  - Both `deserialize_chunks` and `reconstruct_xorb_with_footer` reject the
    reference file with `UnexpectedEof`.
  - The golden tests check the chunk section and assert that the full file
    is rejected.
- The spec's printed hash-string example
  (<https://huggingface.co/docs/xet/api#converting-hashes-to-strings>) is 62
  characters: it drops the `10` byte. Its own reordered byte list and
  `DataHash::hex` give
  `07060504030201000f0e0d0c0b0a090817161514131211101f1e1d1c1b1a1918`.

Both are worth reporting upstream.

## 3. Async and runtime assumptions vs. a tokio server

- **Parsers are synchronous.** They take `std::io::Read` (plus `Seek` for
  the footer readers). Hashing (BLAKE3) and LZ4 are CPU-bound. In a tokio
  handler, buffer the body (limits: xorb ≤ 64 MiB serialized, spec "Xorb
  Format") and parse in `spawn_blocking`.
- **The async variants use `futures::io::AsyncRead`, not tokio's.** They
  need `tokio_util::compat`, and they are still CPU-bound on the executor.
- **No runtime is created on the format paths.** `XetRuntime` and
  `XetContext` appear only in `shard_file_manager.rs`, `session_directory.rs`
  and `shard_benchmark.rs`, which the server doesn't use.
- **tokio features are unified into ours.** The non-wasm dependency enables
  tokio `rt-multi-thread`, `macros`, `time`, `sync`, `io-util` and
  `test-util`, so openxet's tokio always has `test-util`. That feature only
  adds APIs (time pausing), but it is compiled into production builds.
- **Debug builds read `HF_XET_*` environment variables.**
  `test_configurable_constants!` (`xet-runtime`,
  `utils/configuration_utils.rs`) reads `HF_XET_<NAME>` under
  `debug_assertions`. Examples are `HF_XET_MDB_SHARD_GLOBAL_DEDUP_CHUNK_MODULUS`
  and `HF_XET_MAX_XORB_BYTES`. Release builds use the constants. Tests run
  in debug, so CI must not set these variables.
- **One static initializer runs before `main`.** It is the `ctor` crate's
  internal hook (see the table in section 1). `test_set_constants!` is the
  only `#[ctor]` user, and it is test-only. No upstream code runs before
  `main`.
- **No global logging or tracing setup happens unless called.**

None of these conflict with a tokio server. The rule for the protocol crate
is to buffer, then parse off the reactor.

## 4. Semver behaviour within 1.x

Published releases (crates.io): 1.5.0 (2026-04-03), 1.5.1, 1.5.2
(2026-04-20), 1.5.3 (2026-07-08), 1.5.4 (2026-07-20), 1.6.0 (2026-08-03)
and 1.7.0 (2026-10-06).

Method: diff the public item signatures (`pub fn|struct|enum|use|static|…`)
of each published crate source. Multi-line signatures are matched by their
first line only, so this is a lower bound.

| Upgrade | Removed or changed public items | Examples |
|---|---|---|
| 1.5.0 → 1.5.2 | 0 | |
| 1.5.2 → 1.5.3 (patch) | 12 | `metadata_shard::{MDBShardFile, ShardFileManager}` re-exports and `MDBShardFile::load_from_file` removed; `SerializedXorbObject::from_xorb` gained parameters; `XORB_CUT_THRESHOLD_*` removed |
| 1.5.3 → 1.5.4 (patch) | 1 | `MAX_CHUNK_SIZE` changed from `lazy_static` to `LazyLock<usize>` (deref-compatible) |
| 1.5.4 → 1.6.0 | 5 | `DataHash::from_hex`, `from_slice` and `from_base64` now return `DataHashError` (the old `DataHashHexParseError` and `DataHashBytesParseError` were removed) |
| 1.6.0 → 1.7.0 | 6 | `process_shard_file_info_section` and `process_shard_xorb_info_section` gained `size_limit`; `MDBMinimalShard::serialize` gained `creation_timestamp`; `num_bytes`, `num_info_entry_following` and `recalculate_shard_size` now return `Result` |

Three of the five upgrades broke the public API, one of them in a patch
release. The breaks hit exactly the functions we wrap: hex parsing in 1.6.0,
the shard section parsers in 1.7.0.

Upstream's `api_changes/` folder (23 notes, 2026-03-04 to 2026-07-28) is
written for agents applying merges, not as a semver policy. It records
breaks openly, for example `update_260318_anyhow_removal_error_alias_cleanup.md`
("API-breaking cleanup") and `update_260402_runtime_and_threadpool_renames.md`.
There is no CHANGELOG file.

Consequences:
- Keep the exact `=` pin.
- Treat every bump as a port. Read `api_changes/`, adapt `openxet-protocol`,
  and run the golden tests and the compatibility suite.
- Never use `xet_core_structures::…` outside `crates/protocol`.

## Recommendation

1. Use `xet-core-structures =1.7.0` for all hashing, xorb and shard work,
   behind `openxet-protocol`. Nothing needs reimplementing; no ADR 0002.
2. In the protocol crate, own the policies upstream leaves open:
   - xorb uploads go through `xorb_with_footer`, which validates chunk
     lengths and limits per spec, then calls `reconstruct_xorb_with_footer`.
     An optional `XETBLOB` footer is accepted and replaced, as upstream's
     in-process test backend does. Other trailing bytes are rejected, and
     the computed hash must equal the path hash.
   - for shard uploads, S6 decides footer and trailing-byte handling
   - parse untrusted shards through the size-budgeted section parsers
   - parse in `spawn_blocking` from a buffered body
3. Accept the compile-time cost. If it becomes a CI problem, propose
   upstream that `xet-runtime` be made an optional feature of
   `xet-core-structures`, rather than vendoring.
4. Report upstream: the reference xorb's 4 trailing bytes, and the 62-character
   example in the spec's api page.
