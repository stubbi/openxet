# CAS routes

Every route `openxet serve` answers, with its credential, request, answer
and every status it can return. The routes are the Xet CAS API
(<https://huggingface.co/docs/xet/api>) as stock clients call it; where the
spec and `hf_xet` 1.7.0 differ, openxet serves both, as recorded in
[S10](../research/S10-cas-wire-contract.md). This page is written from the
code in `crates/cas/src`; the route list is `served()` in
`crates/cas/src/lib.rs`, and a test fails if a route is served without
being in the credential, scoping and echo test matrices (see
[security tests](security-tests.md)).

The Hub API (`/api/...`), Git and LFS routes arrive in M2.

## Conventions

- **Origin root.** The CAS is served at the root of `server.public_url`
  ([config](config.md#server)); clients append `/v1/...` to it.
- **Credentials.** `Authorization: Bearer <token>`, a static token or a CAS
  token ([config](config.md#static-tokens)). *Read* means any valid token;
  *write* means a token with `mode = "write"`. A token only ever sees its
  own scope's files. The probes need no credential, and the transfer route
  takes its signed URL as the credential instead.
- **Hashes in paths** are exactly 64 lowercase hex characters, in Xet byte
  order. `{prefix}` is `default`; the dedup route also takes
  `default-merkledb`.
- **Order of checks.** The path is checked before the credential, and both
  before any body is read. A refused upload's body is read and discarded
  first, so the client sees the answer, not a reset connection; a body
  declared over the route's limit, or sent with `Expect: 100-continue`, is
  not read, and the answer closes the connection.
- **`X-Request-Id`** is on every answer, success or error. The server
  assigns it and logs the request under it; a client-sent one is ignored.
- **`Cache-Control: no-store`** is on every answer of the routes marked
  *private* below, errors included: their answers depend on the caller's
  credential, so no cache or proxy may serve one caller's answer to
  another. `/v2/shards` streams are `no-cache`; the probes and
  `POST /v1/telemetry` send no `Cache-Control`.
- **`HEAD`** on a `GET` route runs the `GET` route's checks and answers
  without a body, except on the transfer route, where it is `405`.
- **Bodies** are never compressed.

## Errors

Every error is JSON with `Content-Type: application/json`:

```json
{"schema":"openxet.error.v1","code":"unknown_xorb","message":"xorb 1f0c… is not stored on this server. If the server was reset, …","hint":"Upload every xorb before the shard that references it, …"}
```

`code` is stable, for scripts and agents; `message` says what happened, and
`hint` what to do next. A `5xx` never carries its cause: give the operator
the `X-Request-Id`, under which the server logged it.

These answers can come from any route:

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `401` | `missing_credential` | No `Authorization` header. `WWW-Authenticate: Bearer`. |
| `401` | `invalid_credential` | The token is unknown, malformed, signed by an untrusted key, carries a claim openxet can't honour, or the header isn't one `Bearer` value. `WWW-Authenticate: Bearer error="invalid_token"`. |
| `401` | `expired_credential` | The CAS token is at or past its `exp`. Same challenge. |
| `403` | `insufficient_scope` | A read token on a route that writes. |
| `404` | `not_found` | No route serves the path, whatever the method: how stock clients learn a route doesn't exist here. Also a `{prefix}` other than the route's. |
| `405` | `method_not_allowed` | The path exists, but not for this method. `Allow` names the methods. |
| `400` | `invalid_path` | The path holds a percent-encoded byte. Hashes and prefixes are sent as they are, so each object has one URL. |
| `400` | `invalid_hash` | A hash in the path (or a batch) is not 64 lowercase hex characters. It is never echoed. |
| `400` | `bad_request` | The body ended before its `Content-Length`, or its transfer was malformed. |
| `408` | `body_timeout` | The body stalled for `server.body_read_timeout`, or fell below `server.body_min_rate` ([config](config.md#server)). The answer closes the connection. Clients retry. |
| `503` | `not_ready` | The server is starting; the index and bucket are not open yet. `Retry-After: 1`; clients retry. |
| `503` | `unavailable` | The index or bucket failed in a way a retry may fix. Clients retry. |
| `503` | `memory_busy` | The server's memory budget stayed full for the request's wait: 120 s for an upload, 5 s for a reconstruction that doesn't fit at once ([config](config.md#memory)). `Retry-After: 1`; clients retry. A refused upload's body is not read, and the answer closes the connection. |
| `413` | `answer_too_large` | One reconstruction answer would hold more than an eighth of the server's memory budget. Final: ask for the file by range, as `hf_xet` does, or put fewer files in a batch; the operator can raise `server.memory_budget`. |
| `413` | `exceeds_memory_budget` | An upload larger than the server's whole memory budget. Final; stock clients' uploads never are. |
| `500` | `internal` | A server bug or an inconsistency between the index and the bucket, logged under the request id. |

## Routes

| Method and path | Credential | Does |
| --------------- | ---------- | ---- |
| `GET /healthz` | none | Liveness |
| `GET /readyz` | none | Readiness |
| `POST /v1/xorbs/default/{hash}` | write, private | Upload a xorb |
| `HEAD /v1/xorbs/default/{hash}` | read, private | Whether a xorb is stored, and its size |
| `POST /v2/shards` | write | Register files, streaming progress (preferred by `hf_xet`) |
| `POST /v1/shards` | write | Register files |
| `GET /v1/chunks/{prefix}/{hash}` | read, private | Global dedup query |
| `GET /v2/reconstructions/{file}` | read, private | Download plan (preferred by `hf_xet`) |
| `GET /v1/reconstructions/{file}` | read, private | Download plan, v1 shape |
| `POST /v1/reconstructions` | read, private | Download plans for several files |
| `GET /v1/reconstructions?file_id=…` | read, private | The same, as `xet-client` sends it |
| `HEAD /v1/files/{file}` | read, private | Whether a file is stored, and its size |
| `GET /v1/transfer/xorbs/{hash}?range=…&exp=…&sig=…` | the signed URL, private | Bytes of a xorb |
| `POST /v1/telemetry` | read | Accepted and dropped |

Any other path is `404 not_found`. That includes the xorb upload-grant
route that pre-release `hf_xet` builds probe: they fall back to
`POST /v1/xorbs` on its `404`. Grants stay unserved until a stable client
ships them (design §5.5).

### `GET /healthz`, `GET /readyz`

No credential: orchestrators call them unauthenticated. Each answers a
fixed body that names nothing stored.

- `/healthz`: `200 {"status":"ok"}` as soon as the server listens.
- `/readyz`: `200 {"status":"ready"}` once the index and the bucket are
  open. Before that it answers `503 not_ready` with `Retry-After: 1`, as
  does every other path, and closes the connection.

### `POST /v1/xorbs/default/{hash}`

Upload one xorb: its chunks as stock clients serialize them, with or
without a footer (<https://huggingface.co/docs/xet/xorb>). The body is
validated, and its computed xorb hash must be `{hash}`. Bytes are stored
before the index names them. Uploading again is safe.

`200 {"was_inserted": true}` if the xorb is new to the caller's scope,
`{"was_inserted": false}` if the scope already had it.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `400` | `xorb_hash_mismatch` | The body hashes to another xorb hash than `{hash}`. Final. |
| `400` | `xorb_malformed` | The body is not a sequence of chunks. Final. |
| `400` | `xorb_empty` | The xorb has no chunks. |
| `400` | `xorb_too_many_chunks` | More chunks than a xorb may hold. |
| `400` | `xorb_chunk_empty` | A chunk header declares 0 uncompressed bytes. |
| `400` | `xorb_chunk_too_long` | A chunk decompresses to more than its declared length, or more than 128 KiB. |
| `400` | `xorb_chunk_too_short` | A chunk decompresses to fewer bytes than its header declares. |
| `400` | `xorb_truncated` | A chunk's compressed data runs past the end of the body. |
| `400` | `xorb_trailing_bytes` | Bytes follow the footer. |
| `413` | `xorb_too_large` | The body is over 64.5 MiB (64 MiB of chunks plus framing). |

Each `xorb_*` message names the rule and the chunk it was found in. Clients
don't retry a `400`.

### `HEAD /v1/xorbs/default/{hash}`

`200` with the stored size as `Content-Length` and no body, or
`404 xorb_not_found` if the xorb isn't visible to the caller's scope (with
`dedup_visibility = "per-scope"`, another scope's xorb reads as missing;
[config](config.md#dedup-visibility)).

### `POST /v2/shards`

Register the files of one upload shard
(<https://huggingface.co/docs/xet/api#5-streaming-shard-upload>). The
shard is parsed, checked against the verification limits, verified against
the stored chunk hashes of every xorb it references, stored, then its files
are registered to the caller's scope. Uploading again is safe.

Before the body is read, a refusal is a status: the credential errors, and
`413 shard_too_large` for a body over 64 MiB. Once the body is read, the
answer is `200` with `Content-Type: application/x-ndjson` and
`Cache-Control: no-cache`, and every outcome is a line of the stream:

```text
{"type":"validating","verified":0,"total":0}
{"type":"validating","verified":0,"total":3}
{"type":"validating","verified":3,"total":3}
{"type":"committing","stage":"uploading"}
{"type":"committing","stage":"syncing"}
{"type":"result","result":1}
```

| Event | Fields | When |
| ----- | ------ | ---- |
| `validating` | `verified`, `total` (files) | First `0` of `0`, before parsing; then once parsed, and once verified. |
| `committing` | `stage`: `uploading` | Before the shard is stored in the bucket. |
| `committing` | `stage`: `syncing` | Before its files are registered in the index. |
| `result` | `result`: `1` or `0` | Last. `1` if the shard or any of its files was new to the scope, `0` if all were registered already. |
| `error` | `message`, `retryable` | Last, instead of `result`. |

While work runs, the last event repeats every `cas.heartbeat_interval`
(`5s` by default; [config](config.md#cas)), since `hf_xet` fails a stream
that is silent for 300 s. Exactly one terminal event, `result` or `error`,
ends the stream.

An `error` event carries the failure as a message, since the status is
already sent; on `/v2/shards` that message is the only server text the user
sees:
- The shard's own fault (`shard_malformed`, `shard_verification_failed` or
  `unknown_xorb` below): its message, with `retryable: false`.
- A server fault: a generic message with the request id, and
  `retryable: true` if a retry may help (the index or bucket was
  unavailable), `false` for a bug. A retryable error makes `hf_xet` resend
  the whole shard.

A client that disconnects mid-stream doesn't stop the work: the shard is
stored and its files registered all the same, and the client's retry gets
`result: 0`.

### `POST /v1/shards`

The same ingest, answered once: `200 {"result": 1}` if anything was new to
the scope, `{"result": 0}` if not. Failures are statuses:

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `400` | `shard_malformed` | The body doesn't parse as an upload shard. The message names what is wrong. Final. |
| `400` | `shard_verification_failed` | The shard parses but doesn't verify against the stored xorbs (a range hash, a length or a file hash doesn't match, a verification entry is missing, an empty file's SHA-256 is wrong), or it exceeds the verification limits. The message names the rule and the file or xorb; values derived from stored chunks are only logged. Final. |
| `400` | `unknown_xorb` | A file references a xorb that isn't stored, or isn't visible to the scope. The message names the xorb and says that if the server was reset, the client's Xet cache for this endpoint (`~/.cache/huggingface/xet` or `$HF_XET_CACHE`) must be cleared before uploading again ([stale client caches](config.md#stale-client-caches)). Final. |
| `413` | `shard_too_large` | The body is over 64 MiB. |
| `503` | `unavailable` | Storage or index failure. Clients retry. |

`hf_xet` doesn't show a `/v1/shards` error body, so each rejected shard is
also logged at `warn` with its full reason under the request id.

### `GET /v1/chunks/{prefix}/{hash}`

Global dedup: is the chunk with this raw hash stored in a xorb the caller
may see? `{prefix}` is `default` (what `hf_xet` sends) or
`default-merkledb` (what the spec names).

`200 application/octet-stream`: a shard with a footer that offers the
xorb holding the chunk and the other xorbs of the newest shard that
references it, in that shard's order starting at the matched one, up to
16 MiB of answer. Its chunk hashes are HMAC-keyed with a fresh key per
answer that expires in 7 days, so the answer never reveals a raw chunk
hash. Which scopes' xorbs are offered follows `cas.dedup_visibility`
([config](config.md#dedup-visibility)).

Shards registered before index migration 0002 have no xorb list: a match
in one of them offers the matched xorb alone, until the shard is registered
again (see the [changelog](../../CHANGELOG.md)).

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `404` | `not_found` | No visible xorb offers the chunk. `hf_xet` reads it as no match and uploads the chunk. The answer is the same for a chunk never stored and another scope's. |
| `429` | `dedup_busy` | Every dedup slot stayed busy for 1 s. `Retry-After: 1`. `hf_xet` reads it as no match, without retrying, and the upload goes on without this answer. If it persists, the server needs more CPUs for dedup. |

A dedup answer must be sent within 30 s plus 1 s per MiB; a client that
stalls longer has its connection closed.

### `GET /v2/reconstructions/{file}` and `GET /v1/reconstructions/{file}`

The plan to download a file registered to the caller's scope, optionally
for one `Range` (<https://huggingface.co/docs/xet/api#1-get-file-reconstruction>).
`Range` is `bytes=start-end` (inclusive; what `hf_xet` sends),
`bytes=start-`, or `bytes=-length`. A range that ends past the file is
clamped to it.

`200 application/json`. v2:

```json
{
  "offset_into_first_range": 0,
  "terms": [{"hash": "<xorb>", "unpacked_length": 3145728, "range": {"start": 0, "end": 57}}],
  "xorbs": {
    "<xorb>": [{"url": "<signed transfer URL>", "ranges": [{"chunks": {"start": 0, "end": 57}, "bytes": {"start": 0, "end": 3146183}}]}]
  }
}
```

v1 has the same `offset_into_first_range` and `terms`, and `fetch_info`
instead of `xorbs`: per xorb, a list of
`{"range": {"start", "end"}, "url", "url_range": {"start", "end"}}`. Chunk
ranges are end-exclusive, byte ranges end-inclusive. Each fetch entry has
one range and its own URL, signed for exactly that range.

The empty file has no satisfiable range: with a `Range` it answers `416`,
and without one `200` with no terms
(`{"offset_into_first_range":0,"terms":[],"xorbs":{}}`). The spec says
`416`; stock clients download the empty file correctly from either (S10
§1). `hf_xet` always sends a `Range`, so it gets the `416`.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `404` | `not_found` | No file with this hash is registered to the caller's scope. Another scope's file reads the same as one never stored. On v2, `hf_xet` then tries v1 once. |
| `416` | `range_not_satisfiable` | The range starts at or past the end of the file, or is the empty suffix `bytes=-0` (on any file). `Content-Range: bytes */{size}`. `hf_xet` reads it as the end of the file, not an error. |
| `400` | `invalid_range` | `Range` is not one byte range of the `bytes` unit, or its start is after its end. |

### `POST /v1/reconstructions` and `GET /v1/reconstructions?file_id=…`

The plans for several files at once
(<https://huggingface.co/docs/xet/api#6-batch-file-reconstruction>).
`POST` takes the spec's JSON list,
`[{"prefix": "default", "hash": "<file>"}, …]`, of at most 256 KiB; `GET`
takes `file_id=<file>` once per file, as `xet-client` sends it (a URL holds
about 880). Both answer alike: `200 application/json`,

```json
{"files": {"<file>": [<term>, …]}, "fetch_info": {"<xorb>": [<v1 entry>, …]}}
```

with terms and entries in the v1 shapes above, each file planned whole. A
file named twice is answered once; a file not registered to the scope is
left out, the same as one never stored.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `400` | `batch_too_large` | More than 1000 distinct files, or files with more than 100,000 terms in total. Split the batch. |
| `400` | `invalid_batch` | The `POST` body is not a JSON list of `{"prefix": "default", "hash": …}` keys. The body is never echoed. |
| `400` | `invalid_hash` | A hash is not 64 lowercase hex characters. |
| `413` | `payload_too_large` | The `POST` body is over 256 KiB. |

### `HEAD /v1/files/{file}`

`200` with the file's size as `Content-Length` and no body, or
`404 not_found` if it isn't registered to the caller's scope.

### `GET /v1/transfer/xorbs/{hash}?range={start}-{end}&exp={unix}&sig=…`

The data URLs a reconstruction hands out. The signature is the
credential: no `Authorization` is read, as stock clients send none to data
URLs. A URL is valid for one xorb, one byte range and until its `exp`
(`cas.url_lifetime`; [config](config.md#transfer-urls)), and only with
exactly `Range: bytes={start}-{end}`.

`206 Partial Content`, `application/octet-stream`, with
`Content-Range: bytes {start}-{end}/{size}` and exactly those bytes.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `400` | `invalid_transfer_url` | The query is missing `range`, `exp` or `sig`, or doesn't parse. Use the URL exactly as the reconstruction returned it. |
| `400` | `bad_range` | No `Range` header, or more than one. |
| `403` | `forbidden` | The signature doesn't match, the URL has expired, its `exp` lies further ahead than this server's `url_lifetime` allows, or `Range` isn't the signed range. `hf_xet` then asks for a fresh reconstruction and retries. |
| `405` | `method_not_allowed` | `HEAD`. `Allow: GET`. |

Logs record the route, never the query: `sig` is a credential.

### `POST /v1/telemetry`

`hf_xet` reports transfer statistics here. openxet needs a credential, then
reads and drops the body: it is never stored or logged (no telemetry unless
an operator opts in). `204 No Content`.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `413` | `payload_too_large` | The body is over 64 KiB. |
