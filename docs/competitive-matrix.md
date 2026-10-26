# Competitive matrix

Where OpenXet stands against other Xet-protocol servers, re-evaluated every
minor release (design §16). Every entry states how it was verified.
Unverified claims are marked as such.

## 2026-10-23: initial evaluation

Method:
- a code audit of each project
- a hands-on run of every project against stock clients: `huggingface_hub`
  2.2.0, `hf_xet` 1.7.0, `git-xet` 0.2.1, `git-lfs` 3.5.1, git 2.39.5. Each
  run follows the same protocol of 18 checks (setup, `hf` and Python API,
  Xet transfer, Range, dedup, Git, crash, authorization, storage,
  throughput, code, maintenance), each graded with the labels in the
  legend below, with the command and its output as evidence.

OpenXet has no server release yet, so this records the bar to beat and the
mistakes to avoid.

Legend: **PASS**, **FAIL**, **PARTIAL** as observed; **N/A** the project
doesn't claim the feature; **NOT RUN** not covered by that run; **Info**
recorded for context, not graded.

Authorization is not graded or published for any project.

| Project | Size / activity | Protocol core | Stock-client result | Notes |
|---|---|---|---|---|
| [Shardline](https://github.com/STEXS-Technologies/shardline) @ `ede71ea4` | ~504k lines of Rust, 984 commits, 978 by one author, all PRs self-merged | Own reimplementation (`shardline-xet-core`), not `xet-core` | **Fails** for model-sized files with `hf` and git (details below) | Broad scope (OCI, S3, Bazel, DuckDB); strong ops docs; strict clippy config |
| [ggoggam/openxet](https://github.com/ggoggam/openxet) @ `fc8cd642` | ~12k LOC, 33 commits, quiet since 2026-07 | Depends on `xet-core-structures`/`xet-data` `=1.5.2` | **Fails** with current clients: a CAS only (no Hub API, Git or LFS), so `hf` can't use it; `hf_xet` 1.7.0 downloads work but uploads fail on `/v2/shards` (1.5.2 uploads work) | Real reference-file tests; HF's own crates for formats and hashing; ~2% storage overhead; crash-safe and fast; closest to our approach |
| [kebyn/xet-server](https://github.com/kebyn/xet-server) @ `e9c4624f` | ~43k LOC, 330 commits | Own reimplementation | **Fails** over Xet in both directions; `hf upload` works only for a repo's first commit, over plain LFS, below 512 MiB, without a README; plain `git-lfs` works | Careful Hub layer: clean errors; no reference-file tests |
| [guilt/xet-server](https://github.com/guilt/xet-server) (Go) @ `22c4aa1` | ~28k LOC, 16 commits, quiet since 2026-09-16 | Go port checked against captured `hf_xet` traffic | **Partly works:** `hf upload`/`hf download` of 1 GiB over Xet with auth off on one machine; whoami, Range, cross-machine dedup, Git and auth-on transfers fail | `xet-proxyd` HF pull-through cache; `docs/PROTOCOL.md` documents real-client quirks; fast and lean |
| [Taytay/xet-server](https://github.com/Taytay/xet-server) branches on guilt @ `dc82516` | 7 stacked PRs inside the fork, none upstream | As guilt | **Fixes what it targets:** cross-machine dedup, auth-on Xet downloads, `git-xet` push; guilt's other gaps remain | Only checks 2, 5, 6, 8, 9 and 10 were re-run |

### Shardline: observed against stock clients

| Area | Observed |
|---|---|
| Setup | 42 min image build. The documented compose stack crash-loops until an undocumented secret is set. |
| `hf auth whoami` | `KeyError: 'orgs'`: response lacks `orgs` |
| `hf upload` (large files) | `preupload` always answers `regular`, so files go base64 in the commit body and fail above 16 MiB; Xet is never used |
| `hf download` (LFS-backed) | Relative 307 `location`, no `X-Repo-Commit` / `X-Xet-Hash` → rejected by `hf` |
| Refs/branches/tags/paths-info/commits APIs | 404 (not implemented); `model_info().sha` is `None` |
| Xet download URLs | Proxied through the server and need an extra header, so stock `hf_xet` gets 401 |
| Range reads | Ignored on `/resolve`; a 1000-byte reconstruction range fetches a whole xorb |
| Global dedup across machines | Every `/v1/chunks/...` lookup 404s; a 1 MB change re-sends ~315 MB |
| `git-xet` via Hub LFS | Wrong header names (`X-Xet-Content-CAS-*`, expiry `"0"`) → "didn't provide a CAS URL" |
| Plain `git-lfs` | Relative `href` → `missing protocol`; 64 MiB PUT cap |
| SIGKILL mid-upload | Content permanently blocked (`409 Conflict`, upload intent stuck in `failed`) |
| `fsck` | 105,576 false-positive hash mismatches on healthy data; exits 0 |
| Throughput (1 GiB, local) | Upload ~18 MB/s, download ~60 MB/s; ~2× storage overhead (xorbs plus per-chunk objects) |

### ggoggam/openxet: observed against stock clients

Tested `fc8cd642` (2026-10-01), native `cargo build --release`, filesystem
backend, SQLite index. Clients: `hf_xet` 1.7.0 and `huggingface_hub` 2.2.0;
`hf_xet` 1.5.2 and 1.6.0 to find which versions work. It is a CAS-only
server, so checks that need a Hub API, Git or LFS are N/A. OpenXet is not
affiliated with ggoggam/openxet; the names collide.

| Check | Result | Evidence |
|---|---|---|
| Setup | PARTIAL | ~5 min from clone to a running server (4m20s cold build). `examples/README.md` claims stock `huggingface_hub` works via `endpoint` (it can't: `405` on `/api/repos/create`); `demo.sh` installs an unpinned `hf_xet`, gets 1.7.0, and its own demo fails |
| Upload via `hf_xet` | FAIL on 1.7.0 and 1.6.0, PASS on 1.5.2 | 1.7.0: `405 Method Not Allowed` on `/v2/shards`; only `/v1/shards` is routed. 1.5.2: 5 xorbs, shard 200 |
| Download via `hf_xet` | PASS (1.5.2 and 1.7.0) | sha256 matches for all 3 files |
| Downloads with no extra headers | PASS | Stock `hf_xet` 1.7.0 and 1.5.2 download with no extra headers; sha256 matches. |
| Range | FAIL | A 1000-byte `/v2/reconstructions` range returns chunks `0..1030`, ~67 MB: term, not chunk, granularity |
| Dedup, same machine | PASS | A 1 MB edit sends 1,134,889 B; an identical copy sends 0 B of xorbs |
| Dedup, second machine (bytes sent) | FAIL | 314,611,672 B re-sent for a 1 MB edit: every `GET /v1/chunks/default/<h>` is 404; only `default-merkledb` is routed |
| 1 GiB incompressible | PASS | 17 xorbs of ~64 MiB accepted; the body cap is derived from the protocol limit |
| SIGKILL mid-upload of 512 MB | PASS | The original client finished on retry after restart; the download is byte-identical |
| GC (no fsck) | PASS | `POST /v1/gc` removed 2 orphan xorbs and 1 superseded shard; all live files still downloaded intact |
| Storage overhead | PASS | 1 GiB stored as 1.0001× xorbs, plus 0.8 MB shards and a 20 MB index: ~2% in total |
| Throughput (1 GiB, local) | PASS | Upload ~88 MB/s (1.5.2), download ~307 MB/s; peak RSS 327 MiB |
| Code | Info, one gap | Uses HF's crates for formats and hashing; 12 reference-file tests run against the HF dataset and pass; 93 tests, ignored ones explicitly `#[ignore]`; no `unsafe`. Gap: `xetcore_compat.rs` replays request patterns instead of running real `hf_xet`, which likely explains why the `/v2/shards` and `/v1/chunks/default` mismatches went unnoticed |
| Maintenance | Inactive | 33 commits, 1 contributor, 32 of 36 PRs self-merged; last code change 2026-07-10 |

The protocol core is the strongest of the projects evaluated: HF's own
crates, real reference tests, lean storage, crash safety and speed. It is a
bare CAS, not a Hub, and is about one `hf_xet` release behind.

**Security, disclosed with the maintainer's agreement.** We reported these
privately on 2026-10-23. The maintainer reports all of them fixed on `main`
at `ccc3591` (merged 2026-10-23) and agreed to public mention.
At `fc8cd642` we found:
- With auth on and the filesystem backend, `GET /v1/xorbs/default/{hash}`
  served a xorb without a credential [observed].
- Any valid OIDC token mapped to write access on all data, so one user
  could read another user's files [observed].
- Any writer could run `POST /v1/gc` with a zero grace period [observed].

The maintainer's fix:
- Xorb reads need a signed, range-bound, expiring URL or an admin token.
- OIDC roles map to read, write and admin scopes, and a file is readable
  only by its owners and admins.
- GC is admin-only with a minimum 1-hour grace period.
- Shard registration now requires verification entries.

Our re-test of `ccc3591` is pending; this note changes to observed once it
runs.

### kebyn/xet-server: observed against stock clients

Tested `e9c4624f` (2026-10-10) in a Linux container (`rust:1.94.1-bookworm`),
README configuration and default limits. Clients: `huggingface_hub` 2.2.0,
`hf_xet` 1.7.0, git 2.39.5, `git-lfs` 3.5.1, `git-xet` 0.2.1.

| Check | Result | Evidence |
|---|---|---|
| Setup | FAIL | macOS build fails (`xet-common/src/lib.rs:352`: `expected u64, found u32`), no Dockerfile; 13.5 min to the first small `hf upload`. README uses `hf repo create` (should be `repos`) and a Git URL that doesn't exist |
| `hf auth whoami` | PASS | `{"name":"admin","orgs":[],…}` |
| `hf upload` 300 MB | FAIL | Xet is chosen, but every xorb gets `400 Could not find xorb footer`. A folder with `README.md` fails earlier: `/api/validate-yaml` is an empty 404, so `JSONDecodeError`. Works only with `HF_HUB_DISABLE_XET=1` |
| Second commit | FAIL | `409 No parent specified but repository already has HEAD`; its 64-hex commit IDs are rejected by `huggingface_hub` as `parent_commit`, so stock clients get one commit per repo |
| `hf download` and `resolve` headers | PARTIAL | sha256 matches; `X-Repo-Commit`, `X-Linked-Size`, `X-Linked-Etag` present; no `X-Xet-Hash`, no `xet-auth` link, so downloads never use Xet |
| Xet download via `hf_xet` | FAIL | `error decoding response body`: the reconstruction response is a custom schema keyed by sha256 |
| Range | FAIL | Ignored on `resolve` (200, full body) and on reconstruction |
| Dedup, same machine | FAIL | A 1 MB edit sends 315 MB and writes a new 316 MB xorb; an identical copy is re-sent (1.26 GB over 4 attempts, three 504s) |
| Dedup, second machine (bytes sent) | FAIL | `/v1/chunks/default/<h>` with a write token: `403 'read' required`; with a read token: `200 {"found":false}`, not a shard |
| Python API | FAIL | `list_repo_tree` and `hf_hub_download` at a commit pass; `paths-info`, refs, commits, branch, tag 404; delete fails (``unknown variant `deletedFile` ``); `model_info().sha` is `None` |
| `git-lfs` + `git-xet` | PARTIAL | Push and clone work, sha matches, but the batch answers `"transfer":"basic"`, so `git-xet` is never used |
| Plain `git-lfs` | PASS | 50 MB round trip; absolute `href`, `expires_in` |
| 1 GiB upload | FAIL | Default `413 max 536870912`; with limits raised, a 504 at 600 s, then 401 on retry (5-minute proxy token expired) |
| SIGKILL mid-upload of 512 MiB | PASS* | Re-upload after restart succeeds, sha matches, no leftover temp files. *The kill landed during Hub staging, before the CAS write |
| Storage overhead | FAIL | 1,619 MB stored for ~983 MB of unique data (+65%); one xorb per file, above the 64 MiB maximum |
| Throughput | FAIL | 512 MiB upload 121 s, download ~24 MB/s; CAS peak RSS 1.06 GB (whole-file conversion buffer) |
| Code | Info | Own formats; no `unsafe` outside tests; `cargo test` stops at its first failing binary (201 pass, 1 fail, later binaries not run) |
| Maintenance | Single-maintainer | MIT, created 2026-06, last push 2026-10-10, one main author |
| Configuration | Info | `casUrl` is taken from the Hub's `HUB_CAS_BASE_URL`, so unless that is set to a public address, clients get a CAS address they can't reach |

The Hub layer is careful work: clean errors, snapshot commits, crash
recovery. The Xet layer isn't wire-compatible with
stock `hf_xet` 1.7.0 in either direction.

### guilt/xet-server and Taytay's branches: observed against stock clients

Tested guilt `main` `22c4aa1` (v1.1.0) and Taytay's `gc` branch `dc82516`,
which contains every other Taytay branch (`dedup-*`, `multi-client-tests`,
`git-lfs-bridge`, `signed-xorb-urls`). Native `go build` (Go 1.27.1).
Clients: `huggingface_hub` 2.2.0, `hf_xet` 1.7.0, git 2.39.5, `git-lfs`
3.5.1, `git-xet` 0.2.1. Taytay was re-run on checks 5, 8 and 10, with spot
checks of 2, 6 and 9; the other checks read "not re-run".

| Check | guilt main | Taytay | Evidence |
|---|---|---|---|
| Setup | PARTIAL | not re-run | 51 s build, first `hf upload` ~2.5 min after clone. A folder with `README.md` fails (`/api/validate-yaml` is a plain-text 404). The README's description of `HF_TOKEN` doesn't match the server's behaviour |
| `hf auth whoami` | FAIL | FAIL | `404 Not Found` on `/api/whoami-v2` |
| `hf upload` over Xet | PASS | not re-run | Server log: 5 xorb POSTs, then `/v1/shards` after a `/v2/shards` 404 |
| `hf download` and `resolve` headers | PARTIAL | not re-run | sha256 matches. HEAD returns 200 with `X-Xet-Hash`, `X-Repo-Commit`, `X-Linked-Size`, `X-Linked-Etag`, but no `xet-auth` link; plain GET is `501 direct GET not supported` |
| Downloads with no extra headers | PARTIAL | PASS | Main: stock `hf_xet` downloads with no extra headers with auth off; with auth on, every transfer gets 401. Taytay: a full `hf` round trip works with auth on, with no extra headers |
| Range | FAIL | FAIL | `resolve` with Range is 501; a 1000-byte reconstruction range returns ~67 MB |
| Dedup, same machine | PASS | not re-run | A 1 MB edit sends one 1,046,210 B xorb; an identical copy sends 0 xorbs |
| Dedup, second machine (bytes sent) | FAIL | PASS | Main: every `/v1/chunks/default/<h>` is `400 unsupported chunk-dedup prefix`; 300,037,856 B re-sent. Taytay: answers on `default`; 1,130,754 B sent for the 1 MB edit |
| Python API | FAIL | FAIL | `list_repo_tree` and branch upload pass; `paths-info`, refs, commits, tag, `model_info` and download at an older SHA 404. A new branch starts empty instead of from main; `create_commit` with a delete answers 200 but the file stays |
| Git | FAIL | PARTIAL | Main: the LFS batch answers `{"transfer":"xet"}` with no actions even to plain `git-lfs`, so `git push` exits 0 having uploaded nothing (silent data loss); pull fails. Taytay: `git-xet` push of 300 MB and `git-lfs` pull, sha256 match, exactly `X-Xet-Cas-Url`, `X-Xet-Access-Token`, `X-Xet-Token-Expiration` (epoch), absolute `href`; plain `git-lfs` push gets a clear `422 … install git-xet` |
| 1 GiB incompressible | PASS | not re-run | Accepted, no 413 |
| SIGKILL | FAIL | not re-run | Mid-upload kill recovers on retry. Acknowledged commits are lost after SIGKILL within the 1-minute snapshot window (`tree` is `[]`) |
| Integrity tool | N/A | N/A | None |
| Storage overhead | PARTIAL | not re-run | +7% over unique data (L13 allows ≤ 5%), all from the failed cross-machine dedup re-sending the file; ~0 on one machine; no per-chunk copies |
| Throughput (1 GiB, local) | PASS | not re-run | Upload ~50 MB/s, download ~182 MB/s, peak RSS 55 MiB |
| Code | Info | Info | Go reimplementation with one dependency; reference tests replay 3 captured `hf_xet` binaries; 686 tests pass; the real-`hf` integration test skips without pipenv |
| Maintenance | Single-maintainer | Single-maintainer | guilt: 16 commits, 1 author, last push 2026-09-16. Taytay: 7 PRs open inside the fork, last push 2026-09-16 |

**`xet-proxyd`** (guilt main's HF pull-through cache), run against
huggingface.co on a public repo: `hf download` of
`hf-internal-testing/tiny-random-gpt2` works with matching sha256, but all
three Xet files were relayed live (`file not cacheable (partial xorb
citation)`), and after a restart with an unreachable upstream even
`config.json` returned 404, so offline serving did not hold for this repo
(the README hedges that claim).

guilt main moves large files over real Xet quickly and with little memory,
with auth off and on one machine. Taytay's branches fix exactly what they
target: cross-machine dedup, Xet downloads with auth on and a working
`git-xet` push.

### Cross-project comparison

Rows are the hands-on protocol's checks, with the lesson each one tests.
Shardline's column comes from its run above; NOT RUN marks checks that run
didn't cover. Labels follow the legend at the top. Row 5 grades function
only: stock `hf_xet` downloads with no extra headers. Row 8 is graded on bytes
sent from a fresh cache for a 1 MB edit. Maintenance is our
judgement, not a measurement: **Active** (several maintainers, recent
code), **Single-maintainer** (one author, code changes within 60 days), or
**Inactive** (one author, no code change for over 60 days).

| Check | Shardline | ggoggam | kebyn | guilt main | Taytay |
|---|---|---|---|---|---|
| 1 Setup from README (L14) | FAIL | PARTIAL | FAIL | PARTIAL | NOT RUN |
| 2 `whoami` (L2) | FAIL | N/A | PASS | FAIL | FAIL |
| 3 `hf upload` uses Xet (L1, L16 to L18) | FAIL | FAIL (`hf_xet` 1.7.0) | FAIL | PASS | NOT RUN |
| 4 `hf download`, `resolve` headers (L3) | FAIL | N/A | PARTIAL | PARTIAL | NOT RUN |
| 5 Downloads with no extra headers (L4, function only) | FAIL | PASS | FAIL | PARTIAL | PASS |
| 6 Range (L5) | FAIL | FAIL | FAIL | FAIL | FAIL |
| 7 Dedup, same machine | NOT RUN | PASS | FAIL | PASS | NOT RUN |
| 8 Dedup, second machine: bytes sent for a 1 MB edit (L15) | FAIL | FAIL | FAIL | FAIL | PASS |
| 9 Python Hub API (L19) | FAIL | N/A | FAIL | FAIL | FAIL |
| 10 Git: `git-xet`, `git-lfs` (L7, L8, L20) | FAIL | N/A | PARTIAL | FAIL | PARTIAL |
| 11 1 GiB incompressible (L9, L24) | NOT RUN | PASS | FAIL | PASS | NOT RUN |
| 12 SIGKILL (L10, L21) | FAIL | PASS | PASS* | FAIL | NOT RUN |
| 13 Integrity tool (L11) | FAIL | N/A | N/A | N/A | N/A |
| 15 Storage overhead (L13) | FAIL | PASS | FAIL | PARTIAL | NOT RUN |
| 16 Performance (1 GiB up / down) | PARTIAL (18 / 60 MB/s) | PASS (88 / 307 MB/s) | FAIL (1 GiB fails) | PASS (50 / 182 MB/s) | NOT RUN |
| 17 Code | Info | Info | Info | Info | Info |
| 18 Maintenance | Single-maintainer | Inactive | Single-maintainer | Single-maintainer | Single-maintainer |

\* kebyn's kill landed during Hub staging, before the CAS write, so the run
didn't exercise a crash inside the CAS.

## 2026-10-24: OpenXet at M1

OpenXet's first measured column, at the end of milestone M1. M1 is the
storage-only CAS, so the checks that need the Hub API, Git or LFS name
the milestone that brings them instead of a grade, and a check that only
part of M1 can answer is PARTIAL. Every grade is measured, by hand or by
a test that runs in CI.

Method: release build of `openxet serve` on local disk, dev mode (auth
on), Apple M3 laptop with 8 cores, macOS. Stock `hf_xet` 1.7.0 through its
`XetSession` API, each client run with a fresh Xet cache unless noted.
Rows 6, 11, 15 and 16 come from three runs of
[`scripts/measure-storage-only.py`](../scripts/measure-storage-only.py)
`--size-mib 1024` (1 GiB of `os.urandom`, so incompressible), which prints
each figure below; the others cite the test that asserts them.

| Check | OpenXet M1 | Evidence |
|---|---|---|
| 1 Setup from README (L14) | PARTIAL (storage only; `hf upload` via the Hub API in M2) | README → [quickstart](quickstart-storage-only.md): 1 min 43 s from `cargo install` with an empty Cargo cache to the first upload with `hf_xet` (99 s build, 3 s `pip install hf_xet`, 0.5 s for the walkthrough), measured by hand. CI runs the walkthrough on every PR. L14's `hf upload` of a ≥ 1 GiB file needs the Hub API |
| 2 `whoami` (L2) | M2 | No Hub API yet |
| 3 `hf upload` uses Xet (L1, L16 to L18) | M2 | Stock `hf_xet` uploads over Xet already (checks 5 to 8, 11); `hf upload` needs the Hub API |
| 4 `hf download`, `resolve` headers (L3) | M2 | No Hub API yet |
| 5 Downloads with no extra headers (L4, function only) | PASS | `hf_xet` 1.7.0 downloads with auth on and no extra headers: every data URL is signed for one byte range (`compat/tests/test_storage_only.py`) |
| 6 Range (L5) | PASS (reconstruction) | A 1000-byte range in the middle of the 1 GiB file plans one or two chunks: 13,743 to 131,080 bytes to fetch over three runs. A 300 MiB download is planned in two or more ranged blocks (`test_range_and_empty_file`). Range on `resolve`: M2 |
| 7 Dedup, same machine | PASS | A second upload of the same file sends no xorb; the test asserts no `POST /v1/xorbs` (`test_reupload_global_dedup`). That run even uses a fresh cache, so it holds across machines too |
| 8 Dedup, second machine: bytes sent for a 1 MB edit (L15) | PASS | 1,141,116 B of xorbs sent by stock `hf_xet` for a 1 MiB edit in the middle of a 100 MiB file, fresh cache (`test_fresh_cache_edit_bytes_recorded`, recorded). The bound of at most 2 MiB is asserted with the OpenXet client on the same client core (`fresh_cache_edit_dedups_against_the_server`, `crates/openxet/tests/transfer_cmd.rs:433`) |
| 9 Python Hub API (L19) | M2 | No Hub API yet |
| 10 Git: `git-xet`, `git-lfs` (L7, L8, L20) | M2 | No Git or LFS yet |
| 11 1 GiB incompressible (L9, L24) | PASS | 1 GiB uploaded in one run and downloaded byte-identical, three times |
| 12 SIGKILL (L10, L21) | PASS | `test_sigkill_mid_upload_never_blocks_reupload` (CI): stock `hf_xet` uploads 512 MiB; after the first xorb is accepted and before any shard is registered, the server or the client is SIGKILLed; on a restarted server the same client and cache re-upload, and a fresh client downloads byte-identical. Related: `acked_write_survives_sigkill` (`crates/openxet/tests/serve.rs`) keeps an acknowledged xorb and shard across a server SIGKILL |
| 13 Integrity tool (L11) | M4 | `verify` comes with M4 |
| 15 Storage overhead (L13) | PASS | 1 GiB stored as about 1,074,545,000 B of xorbs (+0.07%), plus 0.8 MB of shards and a 1.35 MB index: +0.27 to +0.28% in total; no per-chunk objects |
| 16 Performance (1 GiB up / down) | PASS (437 / 716 MB/s) | Medians of three runs, local: upload 409 to 449 MB/s, download 619 to 1,048 MB/s; server peak RSS 532 to 728 MiB |
| 17 Code | Info | `xet-core`'s crates for formats and hashing; golden tests on the reference files; fuzzing of the xorb and shard parsers; real `hf_xet` in CI, never a replay (L23); each §5.9 security requirement mapped to its test ([security tests](reference/security-tests.md)) |

Maintenance (check 18) is a judgement we don't make about ourselves.

## Lessons we turn into tests

Each item becomes a case in our compatibility suite or a unit test. The plans
reference this list.

1. `preupload` must route large files to Xet; `hf upload` of a ≥ 1 GiB file
   must use the Xet path. Check this via `hf_xet` activity, not just success,
   with a file name that matches no `.gitattributes` pattern, so the test
   proves routing by size (S3).
2. `whoami-v2` returns every field `huggingface_hub` reads, including `orgs`.
3. `resolve` returns `X-Xet-Hash`, `X-Repo-Commit`, `X-Linked-Size`,
   `X-Linked-Etag` and an `xet-auth` link, with **absolute** redirect URLs.
4. Xet transfer URLs work for stock `hf_xet` with no extra headers: signed,
   self-authorizing URLs.
5. Range reads return only the requested range, at chunk granularity, on both
   the reconstruction API and the HTTP gateway.
6. Cross-machine global dedup:
   - A second machine with an empty cache uploading a 1 MB-modified copy
     transfers ≈ 1 MB.
   - Dedup responses are shards **with a footer** and HMAC-keyed hashes.
   - They cover every xorb of the files containing the chunk.
7. LFS batch responses use exactly `X-Xet-Cas-Url`, `X-Xet-Access-Token`,
   `X-Xet-Token-Expiration` (a real epoch), and absolute `href`s. `ref` is
   optional.
8. The `git-xet` push and `git-lfs` basic download round trip, and plain
   `git-lfs` push and pull, all work against an OpenXet-hosted repo.
9. Incompressible data: xorbs up to the protocol maximum (64 MiB plus
   framing) are accepted. Body limits derive from protocol limits, never
   below them.
10. A crash (SIGKILL) at any point in an upload never blocks re-uploading the
    same content.
11. `verify` (our fsck) has zero false positives on healthy data and a
    non-zero exit on real corruption.
12. Repo creation is authorized per namespace; scoped tokens can't create
    elsewhere. Unauthorized requests get a clean 403 that `hf` renders well.
13. No per-chunk object duplication: storage overhead over unique compressed
    data stays ≤ 5%, measured.
14. From a cold machine to the first successful `hf upload` of a ≥ 1 GiB
    file, following only the README, in under 2 minutes, measured per
    release (design §1a).

Added from the 2026-10-23 hands-on runs of ggoggam, kebyn, guilt and
Taytay. Lesson 5 is reconfirmed: in two projects a 1 KB range fetched
~67 MB.

15. Global dedup answers at `GET /v1/chunks/default/<hash>`, the path stock
    `hf_xet` 1.5.2 and 1.7.0 send, and also at the spec's
    `default-merkledb`. Lesson 6's second-machine test runs against both.
    (ggoggam routes only `default-merkledb`, so cross-machine dedup never
    worked with a real client.)
16. Shard upload is served at `POST /v2/shards` and `POST /v1/shards`;
    uploads pass with `hf_xet` 1.5.2 and 1.7.0. (ggoggam answers `/v2/shards`
    with 405 and every upload from `hf_xet` ≥ 1.6.0 fails.)
17. Xorbs are accepted without a footer, exactly as stock `hf_xet` sends
    them, covered by a reference-file round trip. (kebyn rejects them.)
18. `POST /api/validate-yaml` answers, and every 4xx carries a JSON body.
    Tested by an `hf upload` of a folder containing `README.md`. (kebyn and
    guilt fail it.)
19. Commits from stock `huggingface_hub` work beyond the first. One test
    each:
    - Two consecutive `hf upload`s to the same repo both succeed. (kebyn
      allows one commit per repo.)
    - A commit with `parentCommit` set to the current head is accepted, and
      commit IDs are 40-hex. (kebyn's 64-hex IDs are rejected by the
      client.)
    - `create_commit` with a delete removes the file from the next `tree`.
      (guilt answers 200 and keeps the file.)
    - `create_branch` starts the branch from its base revision, so `tree`
      on the new branch equals `tree` on the base. (guilt starts it empty.)
20. The LFS batch never answers `transfer: xet` to a request that didn't
    offer `xet`, and every object in a response has actions or an error. A
    test pushes with plain `git-lfs` and checks the object arrives. (In
    guilt, git-lfs reads an action-less object as already stored, so the
    push exits 0 having uploaded nothing.)
21. An acknowledged commit survives a SIGKILL immediately after the
    acknowledgement. (guilt loses commits within its 1-minute snapshot
    window.)
22. A shard that references xorbs the server doesn't hold is rejected at
    upload with a clear error, so the client re-sends the xorbs and no file
    is registered that can't be reconstructed.
23. Compatibility tests drive the real, pinned client binaries, never a
    replay of their requests, across a version matrix (oldest supported,
    pinned, latest). A 404 or 405 seen by a newer client fails the suite.
    (ggoggam's replayed tests likely hid both of its fatal path bugs.)
24. Large transfers use idle timeouts, not whole-transfer timeouts, and
    signed URLs and tokens outlive client retries: a 1 GiB upload at
    10 MB/s with one retry succeeds. (kebyn's 1 GiB upload hit a 504, then a
    401 on retry.)
