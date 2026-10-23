# S10: The CAS wire contract stock clients rely on

- Date: 2026-10-23
- Versions examined:
  - `xet-core` tag `v1.7.0` (`4b48571`): `xet_client` (`cas_client/remote_client.rs`,
    `cas_types/`, `retry_wrapper.rs`, `auth.rs`, `shard_upload_v2.rs`,
    `telemetry/`, `common/http_client.rs`, `simulation/local_server/`), `xet_data`,
    `xet_runtime`, `xet_core_structures`, `hf_xet`. The crates.io `xet-data`
    1.7.0 sources are identical to the tag (diffed).
  - `xet-core` tag `v1.8.0-dev0` (`c912975`), for xorb upload grants only.
  - `hf_xet` 1.7.0 as installed in `compat/.venv`, run for real.
  - The spec pages [api](https://huggingface.co/docs/xet/api),
    [auth](https://huggingface.co/docs/xet/auth),
    [upload-protocol](https://huggingface.co/docs/xet/upload-protocol),
    [download-protocol](https://huggingface.co/docs/xet/download-protocol),
    [deduplication](https://huggingface.co/docs/xet/deduplication) and
    [shard](https://huggingface.co/docs/xet/shard), as published on
    2026-10-23 (fetched as `<page>.md`).
- Question (M1): what exactly does a CAS server have to accept and answer,
  per endpoint, so that stock `hf_xet` 1.7.0 uploads and downloads work in
  storage-only mode, and which `xet-data` API does our own client call?

Labels used below:
- **observed:** seen in traffic between `hf_xet` 1.7.0 (or our `xet-data`
  probe) and a server, see "Method".
- **source:** read in `xet-core` at `v1.7.0`; paths are relative to the
  repository root (`@1.8.0-dev0` marks the other tag).
- **spec:** the spec page and section linked.

Where spec and client disagree, openxet follows the client; the table in
"Spec and client disagreements" lists every case found.

Related notes, not repeated here: hash-to-hex conversion and the `Hub`
token routes ([design §3](../superpowers/specs/2026-10-23-openxet-design.md#3-constraints-and-verified-facts)),
download URL signing and range binding
([S1-S2](S1-S2-direct-downloads.md), ADR 0002), Hub API shapes
([S3](S3-hub-api-shapes.md)), the server-side `xet-core-structures` API
([S5](S5-xet-core-structures-server-side.md)), shard verification rules
([S6](S6-shard-verification.md)) and the data paths
([design §5.5](../superpowers/specs/2026-10-23-openxet-design.md#55-data-paths)).

## Answer

`hf_xet` 1.7.0 uses seven routes in a storage-only upload and download
(observed). Nothing else is needed for M1's compatibility target:

| Route | Used for | Must answer |
|---|---|---|
| `GET /v1/chunks/default/{chunk}` | global dedup query | footered shard, HMAC-keyed chunk hashes, or `404` |
| `POST /v1/xorbs/default/{xorb}` | xorb upload | `200 {"was_inserted": bool}` |
| `POST /v2/shards` | shard upload (preferred) | `200` NDJSON stream ending in `result` or `error` |
| `POST /v1/shards` | shard upload (fallback) | `200 {"result": 0 \| 1}` |
| `GET /v2/reconstructions/{file}` + `Range` | download plan (preferred) | v2 JSON, or `416` when the range starts at or past EOF |
| `GET /v1/reconstructions/{file}` + `Range` | download plan (fallback) | v1 JSON, or `416` |
| `POST /v1/telemetry` | transfer summary | any `2xx`; openxet drops the body |

Plus the data URLs the reconstruction hands out, fetched with a single
`Range` and no credential (observed).

The client never sends `HEAD /v1/xorbs/*`, `HEAD /v1/files/*`, the batch
reconstruction, or `GET /v2/file-chunk-hashes/*` in these flows (observed;
no caller in `hf_xet/` or `xet_pkg/`, source). They are specified below
for completeness and for other clients.

Behaviour that shapes the M1 server:
1. **Every write is retried as a whole** on `5xx`, `408`, `429`, timeouts
   and connection errors: up to 6 attempts with full-jitter backoff
   (observed). Xorb and shard uploads must therefore be idempotent, and an
   upload that committed before its response was lost must succeed again.
2. **`404` and `501` on a `/v2/` route switch the client to `/v1/`.** A
   `404` from `/v2/shards` for a content reason (say, a missing xorb) would
   make the client retry the same shard on `/v1/shards`. Content failures
   on `/v2/shards` must be an in-stream `error` event, not a status.
3. **The client never reacts to `401`.** It refreshes a token only by
   clock, 30 s before `exp` (source). A `401` or `403` is final.
4. **HTTP error bodies don't reach the user** (source: the error is built
   from the status alone, `retry_wrapper.rs:198`). The `message` of a
   `/v2/shards` `error` event does (observed). Actionable shard errors go
   in that event; `X-Request-Id` ties a failure to the server log.
5. **The dedup response is a footered shard.** HMAC-keyed chunk hashes, an
   empty file-info section and lookup tables are all accepted (observed).
6. **Stock clients cache shards for 21 days** and dedup against them
   without asking the server (source, observed). Xorbs those shards
   reference must stay readable that long. This sets a floor for the GC
   grace period (design §5.6).

## Method

- **Source:** read the files listed above at both tags; line numbers below
  are at `v1.7.0`.
- **Observed:** `local_cas_server` (built from `xet-client` `v1.7.0` with
  `--features simulation`) behind a logging reverse proxy. The proxy
  records every request and response and can replace a route's status or
  body. `hf_xet` 1.7.0 ran `upload_files` then `download_files` with a
  static token, in fresh cache directories, for these variants:
  - baseline (three files: 1000 B, 3 MiB, empty)
  - re-upload with an empty client cache (global dedup hit)
  - the dedup shard re-exported HMAC-keyed with
    `MDBShardInfo::export_as_keyed_shard_streaming` (`xet-core-structures`
    1.7.0), with and without lookup tables
  - `404` on `/v2/shards`, `501` on `/v2/reconstructions`, `404` on
    `/v1/telemetry`
  - `401` and `403` on `POST /v1/xorbs`
  - `500` on `POST /v1/xorbs`, `429` and `503` on `/v1/chunks`, `429` on
    `/v2/reconstructions` (with `HF_XET_CLIENT_RETRY_BASE_DELAY=100ms`)
  - `404` on both reconstruction versions, `403` on the data URL
  - rewritten `/v2/shards` bodies: the spec's `result` form, an unknown
    event type, a fatal and a retryable `error`, no terminal event
  - a token expiring in 10 s with a refresher
  - `416` for the empty file's reconstruction
- **Our client:** a scratch binary on crates.io `xet-data` 1.7.0 uploaded
  and downloaded a 5 MB file through the same proxy (see "Our client core").

## Conventions for every route

| Aspect | Contract | Label |
|---|---|---|
| Base URL | The client concatenates `"{endpoint}/v1/..."`, so the endpoint must have no trailing slash. Telemetry instead resolves the absolute path `/v1/telemetry` against the endpoint, dropping any path prefix (`telemetry/mod.rs:121-123`). Serve the CAS at the root of its origin. | source |
| Hash in path | 64 lowercase hex characters in Xet byte order (`DataHash::hex`). | spec, source |
| Prefix | `default` for xorbs and chunks (`HF_XET_DATA_DEFAULT_PREFIX`, `xet_runtime/src/config/groups/data.rs:109`). | source, observed |
| Protocol | HTTP/1.1 only (`http1_only`, `common/http_client.rs:90`). | source |
| `Authorization` | `Bearer <token>` on every CAS route when the client has a token (`common/http_client.rs:341`); absent when it has none. Never sent to data URLs. | source, observed |
| `X-Xet-Session-Id` | A UUIDv7 per upload or download session, on every request including data URLs (`cas_types/mod.rs:17`, `http_client.rs:366`). | observed |
| `User-Agent` | `hf_xet/1.7.0`, or `{caller UA}; hf_xet/1.7.0` when the caller passes one (`hf_xet/src/headers.rs:7-22`). Our probe sent only its own UA. | observed, source |
| `Accept` | `*/*`. No `Accept-Encoding`: responses must not be compressed. | observed |
| Request body | `Content-Length` always set; no `Content-Type` on xorb or shard uploads. | observed |
| `X-Request-Id` | Read from every response and logged with each failure (`cas_types/mod.rs:19`, `common/http_client.rs:397`). openxet should send one. | source |
| Timeouts | Connect 60 s; read 300 s of silence (`HF_XET_CLIENT_READ_TIMEOUT`); `POST /v1/shards` has no read timeout (`remote_client.rs:350`). | source |

## Endpoints

### 1. Reconstruction

#### `GET /v2/reconstructions/{file_id}` (preferred)

- **Request:** `Range: bytes={start}-{end}` (inclusive), **always present**
  on the download path (observed). The client plans a download in blocks:
  it first requests `0-255999999` and `256000000-767999999` concurrently,
  then a third block, growing the block size up to 8 GB
  (`reconstruction_terms/manager.rs:83-85`,
  `xet_runtime/src/config/groups/reconstruction.rs:13,21`). Ranges past
  EOF are normal.
- **Range handling the server must do:**
  - `start >= file_size` → `416` (the client reads it as "past the end",
    `remote_client.rs:271`). An empty file may answer `416` or `200` with
    no terms; both download correctly (observed). openxet answers `416`,
    as the spec says.
  - `end >= file_size` → clamp to the file; answer `200`.
- **Response `200`, `application/json`** (observed, `cas_types/mod.rs:223-249`):

```json
{
  "offset_into_first_range": 0,
  "terms": [
    { "hash": "066dec20…0973", "unpacked_length": 3145728, "range": { "start": 1, "end": 57 } }
  ],
  "xorbs": {
    "066dec20…0973": [
      {
        "url": "https://…signed…",
        "ranges": [ { "chunks": { "start": 1, "end": 57 }, "bytes": { "start": 1008, "end": 3147183 } } ]
      }
    ]
  }
}
```

| Field | Type | Meaning |
|---|---|---|
| `offset_into_first_range` | u64 | Bytes to skip in the first term, when the range starts mid-chunk. Applied to the first term only. |
| `terms[]` | array, in file order | One run of chunks in one xorb. |
| `terms[].hash` | hex string | Xorb hash. |
| `terms[].unpacked_length` | u32 | Decompressed size of the term. Must equal the sum of its chunk lengths: the client checks the decoded size and retries on mismatch (`remote_client.rs:688-695`). |
| `terms[].range` | `{start, end}` u32 | Chunk indices in the xorb, end-exclusive. |
| `xorbs` | object keyed by xorb hex | Fetch entries per xorb. |
| `xorbs[h][].url` | string | Opaque URL; fetched without credentials. |
| `xorbs[h][].ranges[].chunks` | `{start, end}` u32 | Chunk indices, end-exclusive. |
| `xorbs[h][].ranges[].bytes` | `{start, end}` u64 | Byte offsets in the stored xorb, **end-inclusive**. They cover exactly the chunk headers and payloads of `chunks`. |

Invariants the client enforces (source,
`xet_data/src/file_reconstruction/reconstruction_terms/file_term.rs:148-230`):
- every term's chunk range lies inside a single `ranges[]` descriptor
  (`chunks.start <= term.start && term.end <= chunks.end`) of an entry
  under that term's xorb, otherwise the download fails as a corrupted
  reconstruction
- terms may extend past the requested end; the client trims.

openxet emits one descriptor per fetch entry, each with its own range-bound
URL (ADR 0002, S1). By default the 1.7.0 client splits multi-range entries
into single-range GETs anyway (`HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING`,
default off, `xet_runtime/src/config/groups/client.rs:285`).

- **Errors the client treats specially:**
  - `416`: end of file, not an error.
  - `404` or `501`: switch to v1 for the rest of the session, once v1
    succeeds (`remote_client.rs:296-335`). A missing file therefore costs
    one v2 and one v1 request (observed).
  - Everything else: the common table in "Status codes and retries".
    `429` is retried here; after 6 attempts the download fails with no v1
    fallback (observed).

#### `GET /v1/reconstructions/{file_id}` (fallback, deprecated)

Same request and range rules (observed). The response replaces `xorbs`
with `fetch_info`, one single-range URL per entry
(`cas_types/mod.rs:196-217`, observed):

```json
{
  "offset_into_first_range": 0,
  "terms": [ { "hash": "066ac7c5…5ef4", "unpacked_length": 2097152, "range": { "start": 0, "end": 35 } } ],
  "fetch_info": {
    "066ac7c5…5ef4": [ { "range": { "start": 0, "end": 35 }, "url": "https://…", "url_range": { "start": 0, "end": 2097431 } } ]
  }
}
```

The client converts each `fetch_info` entry into a v2 entry with one
descriptor (`chunks` = `range`, `bytes` = `url_range`,
`cas_types/mod.rs:251-277`), so the same invariants apply. Without a
`Range` header, v1 is also how `xet-data`'s `upload_ranges` reads a file's
segments (`remote_client.rs:738-773`); stock `hf_xet` 1.7.0 never calls it.

#### Batch reconstruction

- **Spec:** `POST /v1/reconstructions` with a JSON array of
  `{"prefix":"default","hash":"…"}` ([api#6](https://huggingface.co/docs/xet/api#6-batch-file-reconstruction)).
- **Client:** `GET /v1/reconstructions?file_id={hex}&file_id={hex}…`
  (`remote_client.rs:535-568`), never called by `hf_xet` 1.7.0 or
  `xet-data`.
- **Response, both forms:** `{"files": {file_hex: [term…]}, "fetch_info":
  {xorb_hex: [entry…]}}`, the v1 shapes (`cas_types/mod.rs:286-295`).

#### Data URLs

| Aspect | Contract | Label |
|---|---|---|
| Request | `GET {url}` with `Range: bytes={bytes.start}-{bytes.end}`, `X-Xet-Session-Id`, `User-Agent`, `Accept: */*`; no `Authorization` | observed |
| Success | Any `2xx` (`200` and `206` both observed). The body must be exactly the requested bytes. The client decodes it as a chunk sequence and retries on a size mismatch (`remote_client.rs:698-726`). | observed, source |
| Multipart | Parsed only if `Content-Type` contains `multipart/byteranges`, which the client asks for only with multi-range fetching on | source |
| `403` | The client re-requests the reconstruction for exactly that block (observed: `Range: bytes=0-2097151`), takes the fresh URL and retries, up to 6 attempts (`remote_client.rs:600,634-639`) | observed |
| Other | Common table | source |

### 2. `GET /v1/chunks/{prefix}/{chunk_hash}` (global dedup)

- **Request:** prefix `default` (observed); the spec says
  `default-merkledb` ([api#2](https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication)).
  openxet serves both. The path hash is the **raw** chunk hash. No body.
- **When the client asks** (source,
  `xet_data/src/deduplication/file_deduplication.rs:155-170`): on the first
  pass over a file, for the file's first chunk and for chunks whose hash
  is eligible (`hash_is_global_dedup_eligible`, last 8 bytes as LE u64
  `% 1024 == 0`), and at most once per 256 chunks
  (`min_spacing_between_global_dedup_queries`, `data.rs:14`). Queries run
  in the background while chunking continues.
- **Response `200`, `application/octet-stream`:** a shard in the form a
  server builds with `MDBShardInfo::export_as_keyed_shard_streaming`
  (`xet_core_structures/src/metadata_shard/shard_format.rs:966`), given a
  footered shard listing the xorbs to offer:
  - **Header:** standard, version 2.
  - **File-info section:** empty (bookend only), `include_file_info =
    false`. The spec requires this ([shard#global-deduplication](https://huggingface.co/docs/xet/shard#global-deduplication)).
    The client ignores file entries here, so they would only expose other
    files' reconstructions.
  - **Xorb-info section:** the offered xorbs. Xorb hashes are plain;
    every chunk hash is `HMAC(chunk_hash, key)` (`keyed_chunk_hash`,
    `shard_format.rs:603-611`).
  - **Footer: required.** The client reads the last 200 bytes and checks
    footer version 1 (`shard_format.rs:293-305`).
    `chunk_hash_hmac_key` is non-zero, and `shard_key_expiry` is a
    future Unix time. Within the session the shard is used regardless
    (an expiry of 0 still deduped, observed). On later sessions the
    client skips expired shards in its cache
    (`shard_file_handle.rs:216`). The spec reference shard uses 21 days.
  - **Lookup tables:** optional. Without them the client rebuilds its
    chunk index from the xorb section (`read_all_truncated_hashes`,
    `shard_format.rs:761-790`). Both forms deduped (observed).
- **How the client uses it:** it stores the shard in its cache directory,
  indexes it by the keyed truncated hash, and matches its own chunks by
  computing `HMAC(chunk_hash, key)` (`shard_file_manager.rs:451-457`).
  Matched runs become file segments that reference the offered xorbs, so
  the next shard upload names xorbs this client never uploaded (observed:
  the second upload sent no xorb). Verification of those references is
  S6's job.
- **`404`:** no match. In fact **any HTTP error status** ends the query as
  "no shard" and the upload continues (`remote_client.rs:191-200`): after
  6 attempts for `5xx` (observed with `503`), after one for `429`
  (`with_429_no_retry`, observed), and at once for other `4xx`. A
  failing dedup endpoint therefore costs dedup, not uploads.

### 3. Xorbs

#### `POST /v1/xorbs/{prefix}/{xorb_hash}`

- **Request:** prefix `default`. The body is the serialized xorb
  **without footer** (native client: `SerializedXorbObject::from_xorb(…,
  false, …)`, `xet_data/src/processing/file_upload_session.rs:392-397`).
  It is streamed, with `Content-Length` and no `Content-Type` (observed,
  `remote_client.rs:873-886`). S5 lists the validation functions; openxet
  also accepts a consistent footer.
- **Response `200`, `application/json`:** `{"was_inserted": true}`, or
  `false` when the xorb already existed (spec
  [api#3](https://huggingface.co/docs/xet/api#3-upload-xorb)). The field
  is required: a body that doesn't parse is retried as a transient decode
  error (`retry_wrapper.rs:394-418`). Its value is only logged.
- **Errors:** `400` (body malformed or hash mismatch), `401` and `403` are
  final (observed for `401` and `403`). `5xx` is retried, 6 attempts
  observed, each resending the whole body.

#### `HEAD /v1/xorbs/{prefix}/{xorb_hash}`

Not sent by `hf_xet` 1.7.0 (observed, and no `.head(` call in the tree).
Per spec ([api#7](https://huggingface.co/docs/xet/api#7-head-xorb)):
`200` with `Content-Length` = stored xorb size and an empty body, or `404`.

#### `/v1/xorb-grants/*` (`hf-xet` 1.8.0.dev0)

- **Client** (`@1.8.0-dev0`, `xet_client/src/cas_client/remote_client.rs:389-455`):
  sends `POST /v1/xorb-grants/{prefix}/{hash}` with
  `Content-Type: application/json`. `201` returns a grant, `200` means
  the xorb exists.
- **`404` falls back.** The client marks `404` as expected (not retried)
  and maps it to `Unavailable` (`remote_client.rs:413,441`). That returns
  `GrantUploadOutcome::Unavailable` (`:347`), and `upload_xorb` then calls
  `POST /v1/xorbs` (`:1233-1250`).
- **`501` does not fall back.** It is not retried (`retry_wrapper.rs:268` `@1.8.0-dev0`)
  and is not mapped to `Unavailable`, so the upload fails.
- **No memo.** The client asks for a grant before every xorb, so until
  openxet supports grants each xorb costs one extra round trip.
- **Opt-out.** `HF_XET_CLIENT_LEGACY_DIRECT_XORB_UPLOAD=1` skips grants
  (`xet_runtime/src/config/groups/client.rs:84` `@1.8.0-dev0`).
- The rest of the grant protocol is in design §3 and §5.5.

### 4. Shards

The body for both versions is the shard without footer, as S6 specifies,
with `Content-Length` and no `Content-Type` (observed). The client tries
v2 first and keeps v1 for the rest of the session once v1 succeeds after a
v2 `404` or `501` (`remote_client.rs:475-519`, observed).

#### `POST /v2/shards`

- **Response:** `200` then a stream of newline-delimited JSON events. The
  client doesn't check `Content-Type`; send `application/x-ndjson` and
  `Cache-Control: no-cache` as the spec says
  ([api#5](https://huggingface.co/docs/xet/api#5-streaming-shard-upload)).
  Events (`cas_types/mod.rs:309-343`):

```text
{"type":"validating","verified":0,"total":512}      verified, total: u64
{"type":"committing","stage":"uploading"}           stage: "uploading" | "syncing"
{"type":"result","result":1}                        terminal success
{"type":"error","message":"…","retryable":false}    terminal failure
```

- **Parsing rules** (`shard_upload_v2.rs`, observed):
  - `{"type":"result"}` and the spec's `{"type":"result","result":1}`
    both parse. openxet sends the spec form.
  - An unknown `type` is ignored (observed with `{"type":"heartbeat"}`).
  - A line over 1 MiB, a line that isn't JSON, or a stream that ends
    without `result` or `error` fails the upload. No retry for these
    (observed for the missing terminal event).
  - `error` with `retryable: true` retries the whole upload, 6 attempts
    (observed). `retryable` false or absent is final, and `message` is
    shown to the user (observed: `Internal error: verification failed:
    xorb missing`).
- **Silence:** the read timeout is 300 s. The spec says the server
  re-emits the last progress event as a heartbeat while it works; openxet
  should do so every few seconds.
- **Status before the stream:**
  - `404` or `501` trigger the v1 fallback.
  - `400`, `401` and `403` are final.
  - `5xx` is retried.

  Verification failures go in an `error` event: a `404` here would send
  the client to v1 with the same bad shard, and the message would be lost.

#### `POST /v1/shards`

- **Response `200`, `application/json`:** `{"result": 1}` if registered,
  `{"result": 0}` if it already existed. The value is a `u8` enum
  (`serde_repr`, `cas_types/mod.rs:297-307`). Any other value fails to
  decode and is retried. The spec says the value carries no meaning
  ([api#4](https://huggingface.co/docs/xet/api#4-upload-shard)).
- **Errors:** `400` for an invalid shard or failed verification (final).
  The body isn't shown to the user, so log the reason under `X-Request-Id`.

### 5. Files

#### `HEAD /v1/files/{file_id}`

Not sent by `hf_xet` 1.7.0 (observed). Per spec
([api#8](https://huggingface.co/docs/xet/api#8-head-file)): `200` with
`Content-Length` = file size and an empty body, or `404`.

#### `GET /v2/file-chunk-hashes/{file_id}`

- Only `xet-data`'s public `upload_ranges` (delta upload) calls it
  (`xet_data/src/processing/range_upload.rs:171`). Nothing in `hf_xet` 1.7.0
  does.
- **Request:** `X-Range-Dirty: bytes=A-B,C-D` (inclusive ends, at least
  one range), built in `remote_client.rs:950-960`.
- **Response, camelCase** (`cas_types/mod.rs:396-418`): `{"totalChunks":
  u64, "fileSize": u64, "windows": [{"dirtyByteRange": [start, end)}],
  "hashRanges": [MerkleHashSubtree | null; windows+1], "gapVerification":
  [hex…]}`.
- `MerkleHashSubtree` serializes in JSON as `{"nodes": [{hash, size}…],
  "levels", "at_start", "at_end"}` (`merkle_hash_subtree.rs:472-488`).
- Upstream's server-side builder is public:
  `xet_client::cas_client::chunk_window_builder::build_file_chunk_hashes_response`.
  Using it would add a `xet-client` dependency (ADR 0001's single doorway
  needs a decision). This is not M1 scope for the stock-client target.

### 6. `POST /v1/telemetry`

- **Request** (observed): `Content-Type: application/json`, the CAS
  `Authorization` header, and a body of exactly five keys: `time`
  (RFC 3339 ms UTC), `event` (`xet_upload_summary`,
  `xet_download_summary`, heartbeats), `session_id`, `user_agent`, and
  `metrics` (a flat object of counters, timings, OS, arch, endpoint host).
- **Client behaviour** (`xet_client/src/cas_client/telemetry/sink.rs:83-218`):
  sent at the end of each transfer, and every 5 min for transfers longer
  than 5 min. Never retried, 5 s timeout, at most 2 s added to
  `finalize()`. Any non-`2xx` is dropped silently (observed with `404`).
- **Client opt-outs:** `HF_XET_TELEMETRY_ENABLED=false`, or any of
  `HF_HUB_DISABLE_TELEMETRY`, `DISABLE_TELEMETRY`, `DO_NOT_TRACK`,
  `HF_HUB_OFFLINE`, `TRANSFORMERS_OFFLINE`
  (`xet_runtime/src/config/groups/telemetry.rs`).
- **openxet:** answer `204` without reading the body into any store or
  log (non-negotiable 6). The request carries a credential, so the usual
  no-logging rule applies.

## Auth

- **Format:** `Authorization: Bearer <token>`, where the token is opaque
  to the client (`http_client.rs:341`). The spec's tokens come from the
  Hub token routes ([auth](https://huggingface.co/docs/xet/auth),
  design §3).
- **Storage-only `hf_xet` 1.7.0** (source `hf_xet/src/legacy/functions.rs:134-330`,
  run for real; each emits a deprecation warning pointing at
  `XetSession`):
  - `upload_files(file_paths, endpoint, token_info, token_refresher,
    progress_updater, _repo_type, request_headers=None, sha256s=None,
    skip_sha256=False) -> list[PyXetUploadInfo]`
  - `upload_bytes(file_contents, …same…)`
  - `download_files(files: list[PyXetDownloadInfo(destination_path, hash,
    file_size=None)], endpoint, token_info, token_refresher,
    progress_updater, request_headers=None) -> list[str]`
  - `endpoint`: the CAS base URL; `None` means
    `HF_XET_DATA_DEFAULT_CAS_ENDPOINT` (default `http://localhost:8080`).
  - `token_info`: `(token: str, exp: int unix seconds)` or `None`.
  - `token_refresher`: a zero-argument callable returning
    `(token, exp)`, or `None`.
  - `huggingface_hub` 2.2.0 uses `XetSession` instead, with a refresh URL
    that `hf_xet` GETs and parses as `{"accessToken","exp","casUrl"}`
    (S3). That is M2.
- **What the client does with them** (`xet_client/src/cas_client/auth.rs:30,164-226`):

| `token_info` | refresher | Behaviour |
|---|---|---|
| `None` | `None` | No `Authorization` header at all. |
| `(t, exp)` | `None` | Sends `t`. Once `now + 30 s >= exp` every request fails locally with an auth error, before reaching the server. |
| `(t, exp)` or `None` | callable | Sends `t` until `now + 30 s >= exp`, then calls the refresher under a lock and uses its result (observed: `exp = now+10` → refresher called once per session). |

- **On `401`/`403`** the client does not refresh or retry: the upload or
  download fails at once (observed for `401` and `403` on `POST /v1/xorbs`).
- **What the server must do:**
  - Accept a token until its advertised `exp` and answer `401` after. The
    client's 30 s early refresh absorbs clock skew up to 30 s.
  - With M1's static tokens, the caller passes a far-future `exp` (for
    example `2**62`), since `hf_xet` requires an integer.
  - Data URLs carry their own authorization (signed, range-bound;
    design §5.9). The client sends them no header, so a data route must
    never fall back to reading `Authorization`.

## Status codes and retries

Retry policy for every CAS call (`retry_wrapper.rs`, defaults in
`xet_runtime/src/config/groups/client.rs:12-31`):
- **Attempts:** 6 in total (`HF_XET_CLIENT_RETRY_MAX_ATTEMPTS=5` retries;
  observed 6).
- **Delays:** each retry sleeps a uniform random time below 3 s, 9 s,
  27 s, 81 s, then 243 s (`exponential_retry_delays`, `:485`, with
  `tokio-retry` 0.3.1 full jitter). Each delay is capped at 6 min.
- **Retried as a whole:** the request, body upload included, plus
  response processing.

| Status or failure | Default | Exceptions |
|---|---|---|
| `2xx` | success | — |
| `5xx` except `501` | retry | — |
| `501` | final (`:219`) | v2 reconstruction and v2 shards: fall back to v1 |
| `408`, `429` | retry | `429` on `/v1/chunks`: final, treated as no dedup |
| `404` | final | v2 reconstruction and shards: fall back to v1. `/v1/chunks`: no dedup. Grants (`@1.8.0-dev0`): use `POST /v1/xorbs` |
| `416` | final | reconstruction: end of file |
| `403` | final | data URLs: refresh URL and retry |
| `400`, `401`, other `4xx` | final | `/v1/chunks`: no dedup |
| timeout, connect error, truncated body | retry | — |
| JSON decode error on a `2xx` | retry | — |
| redirect, builder, other body errors | final | — |

`404` means "this route doesn't exist here" on `/v2/*` and on the grant
route, so openxet's `404` there must mean exactly that. "File not found"
on `/v2/reconstructions` also returns `404`. That costs the client one
extra v1 request (observed), which is acceptable. A `501` would also fall
back on `/v2/*`, but fails the upload on the grant route, so openxet uses
`404` for unimplemented CAS routes.

## Spec and client disagreements

| Topic | Spec | `hf_xet` 1.7.0 | openxet |
|---|---|---|---|
| Dedup prefix | `default-merkledb` | `default` | serve both |
| Batch reconstruction | `POST` with a JSON key list | `GET ?file_id=` (never called) | serve both, same response |
| v2 shard `result` event | `{"type":"result","result":1}` | unit `{"type":"result"}`; accepts both | spec form |
| v2 shard `error` event | `type`, `message` | also `retryable` (default false) | add `retryable: true` only for transient faults |
| v2 shard fallback | on `404` | on `404` or `501` | `404` for "route absent" only |
| `403` on xorb upload | "Clients MUST retry with a write scope token" | final, no retry | `403` only for a real scope failure |
| `401` | refresh the token | no reaction; refresh by clock only | honour `exp` exactly |
| Data fetch | one multi-range request per entry | one single-range request per descriptor by default | one range per entry (ADR 0002) |
| `Accept-Encoding` | clients SHOULD send gzip or zstd | sends none | compress only when asked |
| Footer bytes 24–72 and 168–192 | reserved | lookup-table offsets and byte counts (`shard_format.rs:103-131`) | write them with upstream's serializers |
| Empty file reconstruction | `416` when `start >= size` | `416` and `200` with no terms both work | `416` |

## Our client core: `xet-data` 1.7.0

The M1 `openxet` upload and download commands are thin wrappers over the
code below, which compiled against crates.io `xet-data`, `xet-runtime`
and `xet-client` `=1.7.0` and round-tripped a 5 MB file through the proxy
(observed). All paths are under `xet_data/src/processing/` unless noted.

```rust
use std::sync::Arc;
use xet_data::processing::data_client::default_config;
use xet_data::processing::{FileDownloadSession, FileUploadSession, Sha256Policy, XetFileInfo};
use xet_runtime::config::XetConfig;
use xet_runtime::core::XetContext;

let mut cfg = XetConfig::new();               // reads HF_XET_* env overrides
cfg.telemetry.enabled = false;                // non-negotiable 6; observed: no /v1/telemetry
let ctx = XetContext::with_config(cfg)?;

let translator = Arc::new(default_config(&ctx, endpoint, Some((token, exp)), None, Some(Arc::new(headers)))?);
let upload = FileUploadSession::new(translator).await?;
let infos: Vec<XetFileInfo> = upload.upload_files([(path, Sha256Policy::Compute)]).await?;
let metrics = upload.finalize().await?;       // uploads remaining xorbs and the shard; nothing is registered before this

let translator = Arc::new(default_config(&ctx, endpoint, Some((token, exp)), None, Some(Arc::new(headers)))?);
let download = FileDownloadSession::new(translator, None).await?;
let (_id, n_bytes) = download.download_file(&XetFileInfo::new_hash_only(hash_hex), &out_path).await?;
download.finalize().await?;
```

| Item | Signature (1.7.0) | Notes |
|---|---|---|
| Context | `XetContext::with_config(XetConfig) -> Result<XetContext, RuntimeError>`; `XetContext::default()` | Inside a multi-thread tokio runtime with time and IO enabled, it wraps that runtime; otherwise it starts its own pool (`xet_runtime/src/core/context.rs:59-77`, `runtime/native.rs:586`). Use `#[tokio::main(flavor = "multi_thread")]`. |
| Config | `data_client::default_config(&XetContext, endpoint: String, token_info: Option<(String, u64)>, token_refresher: Option<Arc<dyn TokenRefresher>>, custom_headers: Option<Arc<HeaderMap>>) -> Result<TranslatorConfig>` (`data_client.rs:27`) | New UUIDv7 session id. Auth rules as in "Auth". `custom_headers` should carry `User-Agent: openxet/{version}` (observed on the wire). `TokenRefresher` is `xet_client::cas_client::auth::TokenRefresher` (async `refresh() -> Result<(String, u64), AuthError>`). |
| Cache dirs | `TranslatorConfig::new(&ctx, SessionContext { cache_root: Some(..), .. })` (`configurations.rs:93-140`) | Root: `cache_root`, else `HF_XET_CACHE`, `$HF_HOME/xet`, `$XDG_CACHE_HOME/huggingface/xet`, `~/.cache/huggingface/xet`. Per endpoint, `{root}/{first 16 URL chars, non-alphanumerics as _}-{hash prefix}/` holds `shard-cache/` and `staging/shard-session/` (observed `http___127_0_0_1-Wi0j41aQ6YKwrYPW`). `default_config` leaves `cache_root` unset. |
| Upload | `FileUploadSession::new(Arc<TranslatorConfig>) -> Result<Arc<Self>>`; `upload_files(&self, impl IntoIterator<Item = (impl AsRef<Path>, Sha256Policy)>) -> Result<Vec<XetFileInfo>>` (native only); `finalize(self: Arc<Self>) -> Result<DeduplicationMetrics>` (`file_upload_session.rs:73,127,752`) | `Sha256Policy::{Compute, Provided(Sha256), Skip}`. `spawn_upload_from_path`, `spawn_upload_bytes` and `start_clean` exist for streaming. `dry_run` constructor available. |
| Download | `FileDownloadSession::new(Arc<TranslatorConfig>, Option<Arc<dyn ChunkCache>>) -> Result<Arc<Self>>`; `download_file(&self, &XetFileInfo, &Path) -> Result<(UniqueId, u64)>`; `download_to_writer(&self, &XetFileInfo, impl RangeBounds<u64>, W: Write + Send + 'static)`; `finalize(&self)` (`file_download_session.rs:40,347,387,214`) | A size mismatch against a known `file_size` is an error. |
| File id | `XetFileInfo::new(hash, size)`, `new_hash_only(hash)`; `hash()`, `file_size()`, `sha256()` (`xet_file.rs:43-89`) | `hash` is the 64-char hex file hash. |

Two client-side facts that bind the server:
- **Local dedup cache.** After a successful shard upload the client
  copies the shard into `shard-cache/` with a 21-day expiry
  (`MDB_SHARD_LOCAL_CACHE_EXPIRATION`, `xet_data/src/processing/constants.rs:16`,
  `shard_interface/native.rs:313-317`; observed expiry 21.0 days). Later
  uploads to the same endpoint dedup against it with no network call.
- **Consequence.** If the server loses or collects a xorb inside that
  window, those uploads reference a missing xorb and S6 rule 8 rejects
  the shard. The upload keeps failing until the user clears the cache.
  Nothing in the protocol tells the client to drop the cache. So:
  - GC grace for referenced-then-orphaned xorbs must be at least 21 days
    plus the dedup-shard key validity.
  - A self-hoster who wipes a server must change the endpoint URL or
    clear `~/.cache/huggingface/xet/{endpoint tag}`. `openxet` docs and
    errors should say so.

The `hf-xet` crate (lib `xet`, crates.io 1.7.0) wraps the same calls as
`xet::legacy::data_client::{upload_async, download_async}` and the newer
`XetSession` builder. Using `xet-data` directly avoids its Python and
session layers.

## Not verified

- The client's behaviour against Hugging Face's production CAS. Everything
  observed here ran against upstream's simulation server and our proxy;
  production-only behaviour (status texts, heartbeat interval, the HMAC key
  lifetime) comes from the spec.
- `huggingface.js` and `git-xet`: whether they call `HEAD /v1/xorbs`,
  `HEAD /v1/files` or the batch route. `git-xet` in `v1.7.0` has no
  `.head(` call; `huggingface.js` wasn't read.
- The wasm client's xorb uploads (footer or not); M1 targets native.
- `hf_xet` with `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` on against
  openxet; S1 covers it against MinIO.
