# S4: analytics clients against a custom endpoint

<!-- description: Which analytics clients (DuckDB, polars, HfFileSystem) can reach a custom Hub endpoint, and the plain-HTTP fallback for those that cannot. -->

- Date: 2026-10-23
- Question (design §11): can DuckDB `hf://`, polars `hf://` and
  `HfFileSystem` (fsspec) target a non-huggingface.co endpoint? If not,
  what is the clean HTTPS route?
- Clients tested (latest stable on PyPI, scratch venv):
  - DuckDB 2.0.0 (httpfs extension pinned by DuckDB v2.0.0 at
    `duckdb/duckdb-httpfs` `77392a7`)
  - polars 2.0.0 (source tag `py-2.0.0`, `22a147d`)
  - `huggingface_hub` 2.2.0 (the version pinned in `compat/`), pandas 3.1.0,
    pyarrow 26.0.0, fsspec 2026.9.0
- Stub: a throwaway FastAPI app (not in the repo) serving one dataset repo
  `datasets/openxet-spike/ev-data` over plain `http://127.0.0.1:8765`. It
  implements the Hub read shapes from `compat/fixtures/hub/` and
  [`S3-hub-api-shapes.md`](S3-hub-api-shapes.md) (repo info, `tree`,
  `paths-info`, `resolve` with `HEAD`, `ETag`, `Accept-Ranges: bytes`,
  `X-Repo-Commit` and single-range `206`). The data is the Xet reference CSV
  (`Electric_Vehicle_Population_Data_20250917.csv`, 261,698 rows)
  converted to Parquet (4.47 MB, 14 row groups of at most 20,000 rows).
  The stub logs every request, so "zero requests" below is a measured fact.

Labels: **source** = read at the cited `file:line`; **observed** = seen in
a run against the stub; **inferred** = our conclusion.

## Answer

| Client | Custom endpoint? | How / evidence | Fallback |
|---|---|---|---|
| DuckDB `hf://` (httpfs) | **No** | Host hard-coded: `ParsedHFUrl.endpoint = "https://huggingface.co"` is never assigned elsewhere (source). `HF_ENDPOINT` ignored; the `huggingface` secret has no `endpoint` (observed). | plain HTTP(S) `read_parquet('{endpoint}/datasets/{repo}/resolve/{rev}/{path}')` works (observed) |
| polars `hf://` | **No** | Host hard-coded in two `format!` strings (source). `HF_ENDPOINT` ignored (observed). | plain HTTP(S) `scan_parquet(url)` works with row-group pushdown (observed) |
| `HfFileSystem` (fsspec) | **Yes** | `HfFileSystem(endpoint=...)` or `HF_ENDPOINT`; all calls go to that host (source and observed). pandas `hf://` works via both routes. | not needed |

Practical summary: for the two engines that matter most (DuckDB, polars),
`hf://` cannot reach OpenXet today. The §5.5 read gateway URL
`{endpoint}/{repo_type}/{repo}/resolve/{rev}/{path}` works with both as a
plain URL if it meets the conformance checklist below. Private repos work
with DuckDB (`http` secret) but **not** with polars on a plain URL (only
through an fsspec file object); see the checklist and "Round 2".

## Evidence

### DuckDB 2.0.0

Source (`duckdb/duckdb-httpfs` at `77392a72`, the commit named by
`duckdb/duckdb` v2.0.0 `.github/config/extensions/httpfs.cmake`):
- `src/include/hffs.hpp:14`: `string endpoint = "https://huggingface.co";`
  is the only value `endpoint` ever has. `grep -n "endpoint" src/hffs.cpp`
  shows reads only (`hffs.cpp:54-61`, `432`, `457`), never a write.
- `src/hffs.cpp:364-420` (`HFUrlParse`) fills `repo_type`, `repository`,
  `revision` and `path` from the `hf://` string. It does not touch
  `endpoint`.
- `src/hffs.cpp:430-453` (`GetTreeUrl`) builds
  `{endpoint}/api/{repo_type}/{repo}/tree/{rev}{path}?limit=N` for globs
  (it follows `Link` headers for pagination, `hffs.cpp:45-61`).
- `src/hffs.cpp:455-467` (`GetFileUrl`) builds
  `{endpoint}/{repo_type}/{repo}/resolve/{rev}{path}` for reads; `HEAD` and
  `GET` both use it (`hffs.cpp:303-320`).
- Only the token is configurable: `SetParams` reads the `huggingface`
  secret's `token` (`hffs.cpp:343-356`). The secret type is created at
  `create_secret_functions.cpp:261-276`.
- The repo types accepted are `datasets`, `spaces` and `buckets`
  (`hffs.cpp:380`), so even with a custom host `models` would be rejected.

Observed (`HF_ENDPOINT=http://127.0.0.1:8765`, `python -I`):
- `select count(*) from 'hf://datasets/openxet-spike/ev-data/data.parquet'`
  failed with `HTTP GET error on
  'https://huggingface.co/datasets/openxet-spike/ev-data/resolve/main/data.parquet'
  (HTTP 401 Unauthorized)`. Stub request count: **0**.
- The same for a glob (`.../tree/main` on huggingface.co, 401), with and
  without a `huggingface` secret.
- `create secret (type huggingface, token 'x', endpoint '...')` fails:
  `Unknown parameter "endpoint" for secret type "huggingface"`.
- Plain URL works: `read_parquet('http://127.0.0.1:8765/datasets/openxet-spike/ev-data/resolve/main/data.parquet')`
  returns 261,698 rows, and a `group by` returns `TESLA 108777,
  CHEVROLET 18908`. The stub saw `HEAD` then ranged `GET`s (footer
  `bytes=4452352-4471462`, then individual column chunks, all `206`).
  Observed over `http://`; `https://` with a private CA is covered in "Round 2".
- A glob in a plain HTTP URL is refused (`Globs (*) for generic HTTP file
  is are not supported`), so multi-file reads need explicit URLs or an
  `hf://`-style listing, which a custom host cannot get (observed).

### polars 2.0.0

Source (`pola-rs/polars` tag `py-2.0.0`, `22a147d`):
- `crates/polars-io/src/path_utils/hugging_face.rs:48-55`
  (`HFRepoLocation::new`): both bases are literals,
  `"https://huggingface.co/api/{}/{}/tree/{}/"` and
  `"https://huggingface.co/{}/{}/resolve/{}/"`. They are the only
  construction path for `hf://` (`path_utils/mod.rs:431-433` dispatches
  `CloudScheme::Hf` to `expand_paths_hf`, `hugging_face.rs:221`).
- Elsewhere polars reads `HF_TOKEN` and `HF_HOME` only
  (`cloud/options.rs:826-835`); there is no `HF_ENDPOINT` handling
  (`grep -rn HF_ENDPOINT crates` finds nothing).

Observed:
- `pl.scan_parquet('hf://datasets/openxet-spike/ev-data/data.parquet')` with
  `HF_ENDPOINT` set failed with `error decoding response: ... {"error":"Invalid username or password."}`,
  which is huggingface.co's answer. Stub request count: **0**.
- Plain URL works: `scan_parquet('http://127.0.0.1:8765/.../data.parquet')`
  gives `len` 261,698; `select("Make").head(3)` works. Requests were
  `GET Range: bytes=-262144` (suffix range for the footer, twice) and one
  bounded range for the column chunk, all `206` (observed). A suffix range
  therefore must be supported by the gateway.

### `HfFileSystem` (huggingface_hub 2.2.0, fsspec)

Source (`compat/.venv/.../huggingface_hub/`):
- `hf_file_system.py:217-226`: `endpoint` is a constructor argument;
  `self.endpoint = endpoint or constants.ENDPOINT` and
  `HfApi(endpoint=endpoint, ...)`.
- `constants.py:69`: `ENDPOINT = os.getenv("HF_ENDPOINT", ...)`, read at
  import time, so the env var must be set before `import huggingface_hub`.
- `hf_file_system.py:1086-1116`: file URLs are built from `self.endpoint`.
- pandas, via fsspec, forwards `storage_options` to the constructor.

Observed (all against the stub, `huggingface_hub` 2.2.0):
- With `HF_ENDPOINT` set, and separately with `HfFileSystem(endpoint=...)`:
  `ls`, `info` (size 4,471,463), `glob` and a seek-to-end `open().read()`
  returned correct results.
- `pd.read_parquet('hf://datasets/openxet-spike/ev-data/data.parquet')`
  with `HF_ENDPOINT` set, and with
  `storage_options={"endpoint": ...}`: 261,698 rows each.
- Requests used: `GET /api/datasets/{repo}` (repo info),
  `GET /api/datasets/{repo}/tree/main?recursive=false&expand=false`,
  `POST /api/datasets/{repo}/paths-info/main`
  (form-encoded `paths=`; the client in `hf_api.py` also has a JSON
  variant, so serve both), and ranged `GET .../resolve/...`
  (`bytes=4-4471462`, `bytes=4405927-4471462`, `bytes=4471455-4471462`).
  An intermediate stub bug (no form parser) returned 500 for `paths-info`
  and made the client fail, so this endpoint is on the path.
- Side finding: registering `HfFileSystem` with DuckDB
  (`con.register_filesystem(...)`, then `read_parquet('hf://...')`) fails
  with `NotImplementedException: hf: modified() is not implemented` for
  repository paths (`hf_file_system.py:877`, the `modified` method). So
  fsspec does not rescue DuckDB. This is independent of the stub (inferred
  from the method's own message; observed as the failure).

## Round 2: tests of the gateway behaviour (observed)

Stub variant (still throwaway, same app): `If-Range`, `Last-Modified`,
`416`, `a-` ranges, a `302` pair (relative and absolute `Location`) and a
bearer-protected path. `curl` rows test the stub itself; engine rows are
the clients.

- Stub conformance by `curl` (HEAD gives 200 with `Content-Length`,
  `Accept-Ranges: bytes`, `ETag`, `Last-Modified`; no `Range` gives `200`
  and the full 4,471,463 bytes; `0-9`, `4471460-` and `-8` give `206` with
  the right `Content-Range` and `Content-Length`; `4471463-` gives `416`
  with `Content-Range: bytes */4471463`; `ETag` identical on HEAD and
  ranged GET; `If-Range` with the matching ETag gives `206`, with a
  mismatching one `200` and the full body).
- `302` from `resolve` with a **relative** `Location` and with an
  **absolute** `Location`: DuckDB 2.0.0 and polars 2.0.0 both followed
  both, and read 261,698 rows. The stub log shows `302` on the
  redirecting path and `206` on the target (`/v/blob/`).
- Private path (stub answers `401` unless `Authorization: Bearer REDACTED`):
  - DuckDB without credentials: `HTTP 401`. With
    `create secret (type http, bearer_token '...', scope
    'http://127.0.0.1:8765')`: 261,698 rows, and every request the stub
    answered with 200/206 carried the `Authorization` header (the
    anonymous ones got `401`). An `http` secret with
    `extra_http_headers map {'Authorization': ...}` also works.
  - polars without credentials: `401` (`PermissionError`). `storage_options`
    cannot carry headers for a plain `http(s)` URL: `CloudType::Http =>
    Ok(Self::default())` ignores the config (source,
    `cloud/options.rs:788`); the only `Authorization` that polars adds is
    for `hf://` (`options.rs:852-855`). Observed: keys `authorization`,
    `bearer_token` and `http_headers` still gave `401`, and `headers` with
    a dict raised `ValueError (expected str)`.
  - polars through an fsspec file object works with the token:
    `fsspec.filesystem("http", headers={"Authorization": ...})`, then
    `pl.scan_parquet(f)` and `pl.read_parquet(f)` returned 261,698 rows
    with `Authorization` on every request (observed). This reads through
    fsspec, not polars' native reader.
- `https://` with a private CA (local CA, `localhost` certificate):
  - DuckDB fails by default (`SSL peer certificate ... was not OK`) and
    works after `SET ca_cert_file='/path/ca.pem'` (observed).
  - polars failed both with and without `SSL_CERT_FILE` on macOS
    (`Generic HTTP error ... after 10 retries`), which is the S2 caveat for
    the system trust store (observed on macOS only; Linux and Windows are
    unverified). `curl --cacert` worked.
  - **Unverified guidance:** a self-hosted deployment with a private CA
    probably needs a publicly trusted certificate for polars on macOS, or
    a fsspec route with a custom CA. The fsspec private-CA route was not
    run.

## Gateway conformance checklist for M1

Each item is pass/fail against the plain URL
`https://{host}/{repo_type}/{repo}/resolve/{rev}/{path}`. "Result on the S4
stub" is what the item did against the stub, so the client behaviour is
known and the stub is a reference. "Status for OpenXet" says what OpenXet's
real gateway still needs: every row is an M1 test, because none has run
against it yet.

| # | Check | Result on the S4 stub | Status for OpenXet |
|---|---|---|---|
| G1 | `HEAD` returns `200`, `Content-Length` = file size, `Accept-Ranges: bytes`, `ETag`; `Last-Modified` if the backend has one | Pass (curl); DuckDB sends `HEAD` first (observed) | M1 test |
| G2 | `GET` without `Range` returns `200` with the full body | Pass (curl: 200, 4,471,463 bytes) | M1 test |
| G3 | `Range: bytes=a-b` returns `206`, `Content-Range: bytes a-b/N`, `Content-Length` = b-a+1 | Pass (curl `0-9`); both engines send this form (stub log) | M1 test |
| G4 | `Range: bytes=a-` returns `206` to end of file with correct `Content-Range` and `Content-Length` | Pass (curl `4471460-`); neither engine sent this form | M1 test |
| G5 | `Range: bytes=-n` (suffix) returns `206` with the last n bytes | Pass (curl `-8`); polars sent `bytes=-262144` for the footer in the first polars run (stub log, two requests) | M1 test |
| G6 | A range with start >= N returns `416` and `Content-Range: bytes */N` | Pass (curl `4471463-`); no engine was seen hitting it | M1 test |
| G7 | End beyond EOF is clamped (`a-b` with b >= N returns `206` up to N-1) | Pass (curl `4471460-9999999`: `206`, `Content-Range: bytes 4471460-4471462/4471463`, 3 bytes); no engine was seen relying on it | M1 test |
| G8 | `ETag` is identical on `HEAD`, full `GET` and ranged `GET` | Pass (curl: HEAD and ranged GET equal) | M1 test |
| G9 | `If-Range` with the current `ETag` returns `206`; with another value `200` and the full body | Pass (curl); no engine sent it | M1 test |
| G10 | `302` from `resolve` to a relative `Location` is followed with `Range` by DuckDB and polars | Pass (DuckDB 2.0.0, polars 2.0.0) | M1 test against real storage redirects |
| G11 | Same with an absolute `Location` (a presigned object-store URL) | Pass with an absolute `http://localhost` URL (both engines) | M1 test with presigned URLs and range-bound signatures (ADR 0002) |
| G12 | Private repo: unauthenticated request returns `401` with `X-Error-Code`; DuckDB `http` secret (`bearer_token`, host-scoped) is accepted on `HEAD` and every ranged `GET` | Pass (DuckDB) | M1 test with OpenXet tokens |
| G13 | `Authorization` is not forwarded to a different origin on redirect | Not run | M1 test (client behaviour unknown) |
| G14 | polars on a private repo: native `scan_parquet(url)` cannot send `Authorization`; documented route is an fsspec file object with `headers=` | Pass (documents a polars limitation; fsspec route works) | M1 test: keep as documentation check |
| G15 | `https://` with the deployment's certificate | DuckDB: Pass with `ca_cert_file` for a private CA. polars: Fail with a private CA on macOS (only macOS tested; Linux and Windows unverified) | M1 test with the real certificate |
| G16 | Performance: a scan issues many small ranges (about 20 for a 4.5 MB file: footer, then 12 to 16 KiB column chunks), so each range must be cheap and cacheable, with per-request cost not scaling with file size | Not run (the stub reads the whole file per request) | M1 test |

Result column: Pass / Fail / Not run on the S4 stub; "curl" means the stub
itself was checked, not an engine.

## Notes carried over for M1

- `HfFileSystem` (and so pandas `hf://`) needs the Hub read shapes already
  captured in [`S3-hub-api-shapes.md`](S3-hub-api-shapes.md) (repo info,
  `tree`, `paths-info` form or JSON, `resolve`); a Hub-compatible OpenXet serves
  them for the other clients anyway.
- Both engines issue many small ranges per scan (see G16), so the gateway
  must be cheap per range and cacheable.

## Upstream draft

Not filed; per the standing ruling, the controller decides.

### duckdb/duckdb-httpfs: configurable Hugging Face endpoint

> **Title:** `hf://`: allow a custom endpoint (HF_ENDPOINT / secret option)
>
> `ParsedHFUrl::endpoint` is initialised to `"https://huggingface.co"`
> (`src/include/hffs.hpp:14`) and `HFUrlParse` (`src/hffs.cpp:364`) never
> sets it, so `hf://` can only reach huggingface.co. Users who run
> Hub-compatible servers (self-hosted mirrors, enterprise proxies, staging)
> cannot use `hf://` paths, and `huggingface_hub` and the HF CLI already
> support this through `HF_ENDPOINT`.
>
> Proposal:
> 1. Add an optional `endpoint` parameter to the `huggingface` secret
>    (next to `token`, `create_secret_functions.cpp:261-276`), read in
>    `HuggingFaceFileSystem::SetParams` (`hffs.cpp:343`) and copied into
>    `ParsedHFUrl.endpoint`.
> 2. Fall back to the `HF_ENDPOINT` environment variable when no secret
>    sets it, matching `huggingface_hub` (`rstrip("/")`).
> 3. Keep the default unchanged. Do not send the huggingface.co token to a
>    non-default endpoint unless the secret that carries the endpoint also
>    carries the token.
>
> Repro: `HF_ENDPOINT=http://localhost:8765 duckdb -c "select count(*) from
> 'hf://datasets/ns/repo/data.parquet'"` hits `https://huggingface.co/...`
> (`HTTP 401`) and nothing reaches localhost.

### pola-rs/polars: honour HF_ENDPOINT

> **Title:** `hf://` paths ignore `HF_ENDPOINT`
>
> `HFRepoLocation::new`
> (`crates/polars-io/src/path_utils/hugging_face.rs:48-55`) hard-codes
> `https://huggingface.co`. Please read `HF_ENDPOINT` (strip a trailing
> `/`, default unchanged) there, as `huggingface_hub` does
> (`constants.py`), and add a `storage_options` key for per-call override.
> `polars` already reads `HF_TOKEN` and `HF_HOME`
> (`crates/polars-io/src/cloud/options.rs:826-835`), so this is the
> missing third environment variable.
>
> Repro: `HF_ENDPOINT=http://localhost:8765 python -c "import polars as
> pl; pl.scan_parquet('hf://datasets/ns/repo/data.parquet').collect()"`
> contacts huggingface.co.

## Method notes

- Scratch venv with `duckdb 2.0.0`, `polars 2.0.0`, `huggingface_hub 2.2.0`,
  `pandas 3.1.0`, `pyarrow 26.0.0`; everything that read downloaded data
  ran with `python -I`.
- Not tested: `models` repos through `HfFileSystem`, the real gateway's
  redirect to presigned object-store URLs, `Authorization` forwarding on
  cross-origin redirects (G13), performance (G16), the fsspec private-CA
  route, and Windows/Linux trust-store behaviour (the HTTPS run was on
  macOS). Each is "Not run" or labelled unverified above.
