# Competitive matrix

Where openxet stands against other Xet-protocol servers, re-evaluated every
minor release (design §16). Every entry states how it was verified.
Unverified claims are marked as such.

## 2026-10-23: initial evaluation

Method:
- a code audit of each project
- a hands-on run of Shardline against stock clients: `huggingface_hub` 2.2.0,
  `hf_xet` 1.7.0, `git-xet` 0.2.1, `git-lfs` 3.5.1, git 2.39.5

openxet itself has no code yet, so this records the bar to beat and the
mistakes to avoid.

| Project | Size / activity | Protocol core | Stock-client result | Notes |
|---|---|---|---|---|
| [Shardline](https://github.com/STEXS-Technologies/shardline) @ `ede71ea4` | ~504k lines of Rust, 984 commits, 978 by one author, all PRs self-merged | Own reimplementation (`shardline-xet-core`), not `xet-core` | **Fails** for model-sized files with `hf` and git (details below) | Broad scope (OCI, S3, Bazel, DuckDB); strong ops docs; strict clippy config |
| [ggoggam/openxet](https://github.com/ggoggam/openxet) | ~12k LOC, 33 commits, quiet since 2026-07 | Depends on `xet-core-structures`/`xet-data` `=1.5.2` | Not run | Reference-file tests; HMAC-keyed dedup responses; closest to our approach |
| [kebyn/xet-server](https://github.com/kebyn/xet-server) | ~43k LOC, 319 commits | Own reimplementation | Not run | No reference-file tests |
| [guilt/xet-server](https://github.com/guilt/xet-server) (Go) | ~28k LOC, 16 commits | Go port checked against captured `hf_xet` traffic | Not run | `xet-proxyd` HF pull-through cache; `docs/PROTOCOL.md` documents real-client quirks |

### Shardline: observed against stock clients

| Area | Observed |
|---|---|
| Setup | 42 min image build. The documented compose stack crash-loops until an undocumented secret is set. |
| `hf auth whoami` | `KeyError: 'orgs'`: response lacks `orgs` |
| `hf upload` (large files) | `preupload` always answers `regular`, so files go base64 in the commit body and fail above 16 MiB; Xet is never used |
| `hf download` (LFS-backed) | Relative 307 `location`, no `X-Repo-Commit` / `X-Xet-Hash` → rejected by `hf` |
| Refs/branches/tags/paths-info/commits APIs | 404 (not implemented); `model_info().sha` is `None` |
| Xet download URLs | Proxied through the server and require a bearer token → stock `hf_xet` gets 401 |
| Range reads | Ignored on `/resolve`; a 1000-byte reconstruction range fetches a whole xorb |
| Global dedup across machines | Every `/v1/chunks/...` lookup 404s; a 1 MB change re-sends ~315 MB |
| `git-xet` via Hub LFS | Wrong header names (`X-Xet-Content-CAS-*`, expiry `"0"`) → "didn't provide a CAS URL" |
| Plain `git-lfs` | Relative `href` → `missing protocol`; 64 MiB PUT cap |
| SIGKILL mid-upload | Content permanently blocked (`409 Conflict`, upload intent stuck in `failed`) |
| `fsck` | 105,576 false-positive hash mismatches on healthy data; exits 0 |
| Throughput (1 GiB, local) | Upload ~18 MB/s, download ~60 MB/s; ~2× storage overhead (xorbs plus per-chunk objects) |
| Authorization | Token scoped to one repo could create repos in any namespace |

## Lessons we turn into tests

Each item becomes a case in our compatibility suite or a unit test. The plans
reference this list.

1. `preupload` must route large files to Xet; `hf upload` of a ≥ 1 GiB file
   must use the Xet path. Check this via `hf_xet` activity, not just success.
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
   `git-lfs` push and pull, all work against an openxet-hosted repo.
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
    file, following only the README, in under 2 minutes, measured in CI.
