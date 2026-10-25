# S1 + S2: Direct presigned downloads and plain-HTTP endpoints

<!-- description: Whether stock hf_xet downloads from presigned, range-bound object-store URLs, and the plain-HTTP endpoints downloads need. -->

- Date: 2026-10-23
- Clients: `huggingface_hub` 2.2.0 and `hf_xet` 1.7.0, the versions pinned
  in `compat/`. Both were also the latest PyPI releases on this date, so
  there was no separate "latest" leg to run. The source was read at
  xet-core tag `v1.7.0` (`4b48571`) and compared with `main` (`d26341d`).
  The download path is unchanged between them (see "Upstream drift" below).
- Object store: MinIO `RELEASE.2026-09-22T19-25-18Z` (image
  `cgr.dev/chainguard/minio@sha256:d88920d6…`; the `minio/minio` images on
  Docker Hub and quay.io no longer pull). It ran in Docker on colima, aarch64.
- Platforms: macOS 15 (Darwin 24.6, arm64) for every variant. The TLS
  variants were repeated with the client on Linux (`python:3.13-slim`,
  arm64).
- Questions (design §11):
  - **S1:** does stock `hf_xet` download correctly when
    `/v2/reconstructions` returns native S3 presigned URLs with one range
    per fetch entry and no `X-Xet-Signed-Range`?
  - **S2:** what happens with `http://` Hub and CAS endpoints, and what TLS
    setup does a quickstart need?

## Answers

- **S1: yes.** Stock `hf_xet` downloaded the reference file byte-exact
  (sha256 matched, Xet path proven by the Task 5 harness checks) from
  native SigV4 presigned MinIO URLs:
  - with one fetch entry and one range, the shape huggingface.co returns
  - with four fetch entries of one range each
  - with each URL **bound to its exact `Range` header**
    (`X-Amz-SignedHeaders=host;range`)
- **Multi-range against object stores fails, but only when a client opts
  in.** In
  1.7.0, `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` defaults to `false`.
  By default `hf_xet` splits a multi-range fetch entry into separate
  single-range GETs on the same URL. That worked against MinIO. With the
  flag on, it sends `Range: bytes=a-b,c-d`. MinIO answered `200` with the
  whole object, and the download failed after about 120 s of retries.
  That was observed on MinIO. AWS S3, GCS and Azure were checked only
  through anonymous GETs of public objects: AWS S3 and Azure answered `200`
  with the whole object, and GCS answered `400`
  (`compat/fixtures/s1/cloud-multirange.txt`). Their presigned behaviour
  is unverified.
- **v1 fallback works.** When `/v2` returned 404, the client fell back to
  `/v1/reconstructions` and the download succeeded.
- **Expired URLs are refreshed.** On a `403`, `hf_xet` re-requests the
  reconstruction once (single-flight per block) and retries with the new
  URLs. The object store checks expiry when a request starts, not while
  the transfer runs.
- **S2: `http://` works everywhere, with no warning.** Hub, CAS and object
  store all over plain HTTP downloaded correctly on macOS and Linux.
- **S2: HTTPS with a private CA depends on the platform.**
  - On Linux, `SSL_CERT_FILE=<ca.pem>` is honoured by both
    `huggingface_hub` and `hf_xet`.
  - On macOS, `hf_xet` failed with `UnknownIssuer` despite
    `SSL_CERT_FILE` (observed), while `huggingface_hub` accepted the same
    CA. The mechanism is inferred from source: verification goes through
    the system trust store (`rustls-platform-verifier`), which ignores
    `SSL_CERT_FILE`.
  - So the local quickstart should default to `http://` on loopback.
- **S2: the presign endpoint must be reachable from the client.** A
  presigned URL carries, and SigV4 signs, the store host that OpenXet
  presigned with. The client then connects to exactly that host. In the
  Linux runs the client shared MinIO's network namespace, so the stub
  presigned with `127.0.0.1:9000`; from the macOS host it was
  `127.0.0.1:19000`. In a compose quickstart, OpenXet must presign with
  the store's client-facing address, not the in-network service name.

## Method

All spike code lived in a scratch directory outside the repo:
- a ~200-line FastAPI stub
- a MinIO bucket holding the reference xorb
- a driver that reuses `compat/conftest.py` (`build_hf_env`,
  `_DOWNLOAD_SCRIPT`, `assert_xet_download`) for a single, non-retried
  `hf_hub_download` against `OPENXET_COMPAT_TARGET`

Every run used an isolated `HF_HOME` and `HF_XET_CACHE`, so the 1.7.0
chunk cache (off by default in hf_xet wheels) could not serve anything. A
run passed when three things held:
- the downloaded file's sha256 matched the reference
- the harness's call-boundary hook recorded the Xet hash
- `hf_xet`'s log recorded "Reconstructing file to disk" for it

**Stub.** It served the reference file with `huggingface_hub` 2.2.0's
source as the specification:
- **`HEAD|GET /datasets/{repo}/resolve/{rev}/{file}`**: `200` with
  `X-Repo-Commit`, `X-Linked-Size`, `X-Linked-Etag`, `ETag`,
  `X-Xet-Hash`, `Content-Length`, and
  `Link: <…/xet-read-token/{rev}>; rel="xet-auth"`.
  `get_hf_file_metadata` reads exactly these
  (`file_download.py:1675-1688`, `utils/_xet.py`
  `parse_xet_file_data_from_response`).
- **`GET /api/datasets/{repo}/xet-read-token/{rev}`**: the
  `X-Xet-Cas-Url`, `X-Xet-Access-Token` and `X-Xet-Token-Expiration`
  headers, which `huggingface_hub` reads. It also returns the JSON body
  `{casUrl, exp, accessToken}`, which `hf_xet`'s own token refresh parses
  (`hf_xet/src/py_xet_session.rs`).
- **`GET /v2/reconstructions/{file}`** and **`/v1/reconstructions/{file}`**:
  - Bearer token required.
  - Honours the client's file `Range` header, returns `416` past EOF, and
    computes `offset_into_first_range`.
  - Byte ranges come from the reference xorb's chunk headers. The chunk
    section ends at byte 14737813; the 4 trailing bytes are `XETB`.

Request and response capture:
- The stub logged every exchange.
- `mc admin trace -v --json` logged every object-store request, including
  the exact `Range` header `hf_xet` sent and MinIO's status.

Fetch-entry shapes (`STUB_MODE`):

| mode | terms | fetch entries per xorb | ranges per entry |
|---|---|---|---|
| a1 | 1 (chunks 0–796) | 1 | 1 (bytes 0–14737812), as huggingface.co returns it |
| a4 | 4 | 4 | 1 each |
| b  | 4 | 2 | 2 non-adjacent each (pieces 0+2, 1+3) |
| v1 | 4 | `/v2` → 404; `/v1` `fetch_info` with 4 entries | 1 each |

## Results per variant

The redacted request/response logs are in `compat/fixtures/s1/`. The file
named in each row is that variant's log.

| variant | Hub | CAS | result | evidence |
|---|---|---|---|---|
| **a1** one entry, one range, presigned | http | http | ✅ sha256 ok, Xet proven | 1 GET `bytes=0-14737812` → 206 (`a1-http.jsonl`) |
| **a4** four entries, one range each | http | http | ✅ | 4 GETs, each one exact entry range → 206 (`a4-http.jsonl`) |
| **a1/a4 range-bound** (`SignedHeaders=host;range`) | http | http | ✅ both | 1 and 4 GETs → 206 (`a4-signedrange-http.jsonl`) |
| **b** multi-range entries, default client | http | http | ✅ | client split into 4 single-range GETs → 206 (`b-http.jsonl`) |
| **b** + `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING=true` | http | http | ❌ after 120.5 s | 12 GETs `bytes=0-3318991,7209526-11085556` etc. → **200, full 14737817-byte object** (`b-multirange-http.jsonl`) |
| **v1** fallback (`/v2` → 404) | http | http | ✅ | each of the 3 parallel block requests got 404 on v2, then 200 or 416 on v1 (`v1-http.jsonl`) |
| **c** https Hub, http CAS, CA in `SSL_CERT_FILE` (macOS) | https | http | ✅ | (`c-https-http-ca.jsonl`) |
| **c** https Hub, https CAS, no CA (macOS) | https | https | ❌ at the Hub HEAD | `huggingface_hub` never reached CAS |
| **c** https Hub, https CAS, `SSL_CERT_FILE` (macOS) | https | https | ❌ after 69.4 s | Hub HEAD and token OK; `hf_xet` → CAS fails TLS |
| **c** http Hub, https CAS, `SSL_CERT_FILE` (macOS) | http | https | ❌ after 93.5 s | same `hf_xet` TLS failure |
| **c** http/http (Linux) | http | http | ✅ | |
| **c** https/https, no CA (Linux) | https | https | ❌ at the Hub HEAD | |
| **c** https/https, `SSL_CERT_FILE` (Linux) | https | https | ✅ | (`linux-https-https-ca.jsonl`) |
| **d** URLs expire before first use, fresh on refresh (expiry 20 s, first answer delayed 25 s) | http | http | ✅ in 31.3 s | 4× 403 → 1 reconstruction refresh → 4× 206 (`d-expired-once.jsonl`) |
| **d** every URL already expired (expiry 1 s, every answer delayed 3 s) | http | http | ❌ after 165.9 s | 22 GETs → 403, 16 refreshes |
| **d** URL expires mid-transfer (curl, not `hf_xet`) | – | – | ✅ | 5 s URL; transfer throttled to 1.5 MB/s → 206, all 14737813 bytes in 9.6 s |

Verbatim errors:

- **b with multi-range on** (`hf_xet` log, then stderr):
  ```
  No more retries; aborting: Format error: I/O error: unexpected end of file
  RuntimeError: Task error: File reconstruction error: CAS Client Error: Format error: I/O error: unexpected end of file
  ```
  The `200` body was parsed as one xorb segment
  (`remote_client.rs:697-725`, non-multipart branch). This object fails
  on its 4 trailing `XETB` bytes. A xorb without a trailer would fail
  instead on `get_file_term_data: expected N uncompressed bytes, got M`
  (`remote_client.rs:715-722`). That second error is from the source and
  was not observed.
- **c, no CA, macOS** (`huggingface_hub` uses the system trust store when
  no `SSL_CERT_FILE` is set):
  ```
  httpx2.ConnectError: ('“localhost” certificate is not trusted',)
  huggingface_hub.errors.LocalEntryNotFoundError: An error happened while trying to locate the file on the Hub and we cannot find the requested files in the local cache. Please check your connection and try again or make sure your Internet connection is on.
  ```
- **c, no CA, Linux:**
  ```
  httpx2.ConnectError: [SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed: unable to get local issuer certificate (_ssl.c:1032)
  ```
  The final error was the same `LocalEntryNotFoundError`.
- **c, `SSL_CERT_FILE` set, CAS on https, macOS** (`hf_xet` log, then
  stderr):
  ```
  failed to verify TLS certificate: invalid peer certificate: UnknownIssuer   (rustls_platform_verifier::verification::apple, rustls-platform-verifier 0.6.2)
  RuntimeError: Task error: File reconstruction error: CAS Client Error: Request middleware error: error sending request for url (https://localhost:18081/v2/reconstructions/118a53328412787fee04011dcf82fdc4acf3a4a1eddec341c910d30a306aaf97)
  ```
- **c, self-made CA without key identifiers:**
  ```
  certificate verify failed: Missing Authority Key Identifier (_ssl.c:1032)
  ```
  Python 3.13's strict X.509 checks rejected the first test CA.
  Quickstart certificates must carry SKI/AKI.
- **d, every URL expired** (stderr, then MinIO's body):
  ```
  RuntimeError: Task error: File reconstruction error: CAS Client Error: Request error: HTTP status client error (403 Forbidden), domain: http://127.0.0.1:19000/openxet-spike/xorbs/default/eea25d6ee393ccae385820daed127b96ef0ea034dfb7cf6da3a950ce334b7632
  <Error><Code>AccessDenied</Code><Message>Request has expired</Message>…
  ```

Pitfall seen while running **d**: a first attempt used a 1 s expiry with
only the first answer delayed. It also failed, because the refreshed URLs
expired too (observed). The reasons, from source and measurement:
- On a 403, `hf_xet` refreshes **before** it sleeps
  (`remote_client.rs:633-638`). It then sleeps a pre-jitter 3, 9, 27, 81
  and 243 s for attempts 1–5, each capped at `retry_max_duration` (6 min).
  Jitter only shortens a sleep (`retry_wrapper.rs:272`, `485-497`; the
  upstream test `test_exponential_retry_strategy` pins 3/9/27/81/243 s).
- The VM clock ran about 2 s ahead of the host (measured).

So a refreshed URL must outlive the next sleep, up to 243 s with defaults,
plus the skew. On top of that, URLs for prefetched blocks wait before
their first GET. ADR 0002 derives the lifetime floor from these terms.

### Range-bound native URLs

huggingface.co's own v2 URL (CloudFront) binds expiry and the exact range.
Its decoded policy conditions are:
```json
{"DateLessThan": {"EpochTime": …}, "ByteRange": {"ExpectedHeader": "bytes=0-14737812"}}
```

SigV4 can do the same: presigning `get_object` with `Range` makes `range`
a signed header. Against MinIO:
- the signed range → 206
- a different range (`bytes=0-99`) → 403
- no `Range` header → 400

`hf_xet` sends exactly the entry's `bytes` range as `Range` (one range →
`HttpRange::range_header`, `remote_client.rs:616-624`), so range-bound
URLs work with it, as variant a4-range-bound shows.

This only composes with **one range per URL**. Under the default
splitting, every range of a multi-range entry is sent to the same URL, so
a URL bound to one range would 403 on the others. Without the binding, a
presigned URL can read the **whole xorb**. Xorbs are globally shared
(design §5.4), so that can expose chunks of files outside the caller's
scope.

### Multi-range on real cloud stores

These were anonymous GETs of public objects, not presigned URLs and not
`hf_xet`:

| store | `Range: bytes=0-9,100-109` | `Range: bytes=0-9` |
|---|---|---|
| AWS S3 (`noaa-ghcn-pds`) | `200`, full object, no `Content-Range` | `206` |
| GCS (`gcp-public-data-landsat`) | `400` `InvalidArgument: Multiple ranges are not supported` | `206` |
| Azure Blob (`azureopendatastorage`, also with `x-ms-version: 2021-08-06`) | `200`, full object | `206` |
| MinIO (presigned) | `200`, full object | `206` |

The exact commands and status/header lines are in
`compat/fixtures/s1/cloud-multirange.txt`, re-run on 2026-10-23.

## Why the client behaves this way (xet-core v1.7.0)

- **URLs are opaque.**
  - No `X-Xet-Signed-Range` or other URL parsing exists anywhere in the
    client.
  - `XorbMultiRangeFetch.url` is passed as is to `reqwest`
    (`remote_client.rs:611-631`).
  - Object-store GETs use the **unauthenticated** `http_client`, so no
    CAS `Authorization` reaches the object store. The traces confirm this.
    `huggingface_hub` also strips auth from `custom_headers`
    (`xet_headers_without_auth`).
- **Multi-range is opt-in.**
  - `enable_multirange_fetching` defaults to `false`
    (`xet_runtime/src/config/groups/client.rs:276-285`).
  - With the default, `retrieve_file_term_block` creates one block per
    `XorbRangeDescriptor` with a single range
    (`xet_data/src/file_reconstruction/reconstruction_terms/file_term.rs:205-229`).
  - With the flag on, the client sends the joined header and parses
    `multipart/byteranges` only if the `Content-Type` says so; any other
    body is parsed as a single segment (`remote_client.rs:648-727`).
- **v2 → v1 fallback** happens on 404 or 501 only, and the detected version
  is cached per client (`remote_client.rs:296-332`). Each of the three
  prefetch blocks that started in parallel did its own fallback.
- **Reconstruction is requested in blocks:**
  - `Range: bytes=0-255999999`, then `256000000-767999999` and
    `768000000-1023999999`. The server must answer `416` past EOF; a
    refresh re-requests the exact block (`bytes=0-63527243`).
  - Any term may overshoot the requested end; the client trims the last
    one (`file_term.rs:303-313`).
- **403 handling:** `s3::get_range` runs with `with_retry_on_403`. On a 403
  it calls `refresh_url()` (`remote_client.rs:633-638`), which re-requests
  the whole block's reconstruction once per acquisition
  (`retrieval_urls.rs:63-134`). It then retries after the backoff sleep,
  3, 9, 27, 81 and 243 s pre-jitter.
- **TLS:**
  - The wheels build `reqwest` 0.13 with rustls and
    `rustls-platform-verifier` (`Cargo.lock`).
  - Inferred from source and not traced at runtime: on macOS that is
    Security.framework trust evaluation, which ignores `SSL_CERT_FILE`; on
    Linux it honours the OpenSSL-style env vars. The observed failures
    (macOS `UnknownIssuer`, logged by
    `rustls_platform_verifier::verification::apple`) and successes (Linux)
    match.
  - `huggingface_hub`'s `httpx2` used the system store on macOS when
    `SSL_CERT_FILE` was unset and OpenSSL with the file when set. This is
    inferred from the two different error texts.

## Hub-layer findings for M1

- `HEAD resolve` may answer `200` directly with the Xet headers;
  `huggingface_hub` accepts it.
  - A **same-origin** redirect is followed, so any headers on the 302 are
    discarded (`utils/_http.py:735-784`, `_is_same_or_hub_host`).
  - A cross-origin redirect stops there and uses the 302's headers. That is
    what huggingface.co does (302 to `us.aws.cdn.hf.co`).
  - **If OpenXet redirects `resolve` to its own transfer role on the same
    origin, the redirect target must repeat the Xet headers.**
- `huggingface_hub` caches the read token until 60 s before `exp`
  (`XET_CONNECTION_INFO_SAFETY_PERIOD`).
- `hf_xet` 1.7.0 sent no `/v1/telemetry` request in any run.

## Upstream drift (xet-core `main`, not in 1.7.0)

- The download path (`remote_client.rs` reconstruction and range fetch,
  `file_reconstruction/`) is unchanged on `main`.
- `main` adds `XorbUploadGrantRequest` and `XorbUploadGrant`
  (`cas_types/mod.rs`): a server-issued URL, method and exact headers for
  uploading a xorb directly, with a CRC64-NVMe checksum. Nothing uses them
  yet.
- Follow-up (2026-10-23): the client flow that uses them is on tag
  `v1.8.0-dev0` and in the PyPI pre-release `hf-xet` 1.8.0.dev0, not in a
  stable release. Design §3 records the API shape and §5.5 how OpenXet
  will support it.

## Not verified here

- Presigned URLs on **real** AWS S3, GCS and Azure. Only MinIO was
  presigned; the cloud observations above are anonymous public GETs.
- SigV4 range binding on AWS S3 itself, and V4 signed-header range binding
  on GCS.
- Whether Azure SAS can bind a range. This must be checked before relying
  on it.
- `hf_xet` with a private CA trusted in the **macOS keychain**. Not tried,
  to avoid changing the machine's trust store.
- The object store itself behind HTTPS. Object-store GETs use the same
  `reqwest` TLS stack as CAS calls, so the CAS result is expected to apply,
  but that is inferred.
- `huggingface.js` and `git-xet`.
