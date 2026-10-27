# OpenXet design

<!-- description: The approved design: scope, architecture, security requirements, milestones and the hosted service, and the source of truth for what OpenXet builds. -->

Date: 2026-10-23 · Status: approved; reconciled with the M0 spike results

## 1. Intent

Hugging Face's Xet storage gives chunk-level deduplicated storage for large,
versioned files. Models, datasets and checkpoints are the common examples, not
the definition. The client side is open source
(`huggingface/xet-core`, Apache-2.0) and the protocol is published
(Xet Protocol Specification v1.1.0, https://huggingface.co/docs/xet). The server
is not. Nobody can run Xet on their own infrastructure.

**OpenXet is the open-source, self-hostable server for the Xet protocol.** The
official, unmodified Hugging Face clients (`hf_xet`, `huggingface_hub` /
`hf` CLI, `git-xet`, `@huggingface/hub`) work against it. Git repos can live
on any Git host (GitHub, GitLab, Gitea and others) and keep their large files
in OpenXet (§6.1). It runs on storage
and identity systems people already own.

It is not a Hugging Face copy. It is plug-and-play infrastructure: small surface,
clear seams, no opinions forced on the operator.

**Two storage primitives** *(decided by the user, 2026-10-24;
[ADR 0010](../../adr/0010-two-storage-primitives.md))*:
- A **repository** is versioned and Git and Hub compatible: commits,
  refs, and large files as LFS pointers to Xet files.
- A **bucket** holds mutable objects under keys, with no Git history.
  Its objects are still Xet files, chunked and deduplicated.

Both are addressed by `{namespace}/{name}` and carry generic,
owner-defined **labels** (none, one or several, such as `checkpoint` or
`eval-logs`). Hugging Face's repo types (`model`, `dataset`, `space`,
`kernel`) are just the labels the Hub layer reads and writes: its typed
routes (`/api/models/…`, `/api/datasets/…`) are views of the same
repository, so stock clients work unchanged. OpenXet serves anyone
storing large versioned or deduplicated files, so it doesn't mirror one
field's artefact taxonomy.

### Who it is for

- Organizations that cannot or will not put data on huggingface.co (regulated,
  sovereign, air-gapped, cost-sensitive) but want HF-style workflows.
- Teams who today keep large files in MinIO + Git LFS / DVC and get no
  cross-version deduplication.
- Platforms that host other people's large files, such as models and
  datasets (aqora is a likely first user), and want Xet storage under their
  own product, with their own naming, permissions and UI.

### What people store with it

Dedup saves bytes only where versions share content. These numbers are
from [spike S12](../../research/S12-s3-api-and-dedup-economics.md)
(chunk-level, with `xet-data` 1.7.0's chunker) unless marked as
published by Hugging Face. They are upper bounds, since stock clients
skip very short matches (§1a).

- **ML models and datasets.** Hub compatibility means `hf`,
  `huggingface_hub` and `git-xet` work unchanged. A quantization family
  can save about half: 29 GGUF quantizations of one model stored 191 GB
  in about 97 GB (published by HF). Fine-tunes dedup where they keep base
  weights unchanged.
- **Checkpoints from training runs.** Consecutive dense checkpoints dedup
  about 0%, because every weight changes (two consecutive saved
  checkpoints of Pythia-70m, 1,000 steps apart). Compression saves about
  15% on fp16. What helps is resuming, re-uploads and partially frozen
  weights, not the next checkpoint.
- **Dataset versions and Parquet appends.** An appended log's second
  version adds about 10% new bytes, so about 90% of it dedups. Small
  Parquet files dedup less: an append added about 46% new bytes with
  content-defined chunking and 52% without, on 5 MB files.
- **Artefacts and binaries from CI.** Uncompressed source tarballs share
  chunks across versions: a CPython point release shared 23% of its
  `.tar`. Compressed artefacts (`.tar.gz`, `.tar.xz`) dedup about 0%.
  Build outputs and binaries were not measured.
- **Backups and logs.** Append-only logs dedup well, as above.
  Uncompressed snapshots dedup partly (23% for a CPython point release). Compressed or client-side encrypted backups (restic,
  Borg) dedup about 0%; for those OpenXet is plain storage.
- **Agent-generated outputs.** Agents re-run, retry and rewrite. Outputs
  that repeat content dedup like any version series; S12 measured no
  agent workload, so we claim no number. The agent surfaces of §7.1 let
  them store without a human in the loop.
- **Platforms embedding storage** (aqora first). Storage-only mode and the
  authorizer webhook put Xet storage under their own naming and
  permissions (§5.4), with metering for their own billing (§5.7).

Where dedup doesn't help, OpenXet still gives Hub and Git compatibility,
the user's own object-store bucket and identity, and honest numbers:
every repository and bucket page shows logical and stored bytes (§10.10).

### What success looks like

- From zero to a working `hf upload` against OpenXet in under two minutes:
  one command, one environment variable.
- Uploading a file again after a small in-place edit transfers megabytes,
  not gigabytes: in M1, a 1 MiB edit of a 1 GiB file
  sent about 1.2 MB with an empty client cache
  ([S11](../../research/S11-global-dedup-response.md)). A retrained
  checkpoint is not a small edit: every weight changes, and it transfers
  in full.
- It scales by adding nodes, with no redesign, from a laptop to petabytes.
- Operators recommend it because it is boring to run.
- People who don't want to run anything get the same thing, done perfectly, on
  **openxet.com** (§15).

### Visitors become fans *(decided by the user, 2026-10-24)*

"First time visitors must become loyal fans that love us. This must hold
for the open source project as much as for the hosted service." Every
change, doc and decision is judged by what a first-time visitor, human
or agent, experiences. It holds through these commitments, each with
its proof in §16:
- **Time to wow:** an upload in under 60 s hosted and under 2 min
  self-hosted, with the dedup savings shown right away.
- **No dead ends:** every error says the next step, and every doc example
  runs in CI.
- **Honesty:** claim only what is measured, and publish the method.
- **No lock-in:** full export. Hub compatibility makes leaving easy, so
  people stay by choice.
- **Fair, predictable pricing** on openxet.com: no surprise bills.
- **Community:** published issue and PR response times, a mentored
  first-PR path, credit for every contributor, a public roadmap, and
  feedback visibly acted on.
- **Parity:** openxet.com runs the open-source build, so self-hosters get
  the same quality (§15).

## 1a. User value model *(decided by the user)*

What users get from OpenXet, and how each value is measured. The targets
are initial, like §5.8's, and revised with data. Each is checked by the
benchmark suite or the compatibility suite (§8), and the measured numbers
are published with every release. "Checked in" says where: targets that
fit a CI runner gate every PR, and those that need hosted infrastructure,
10 GbE hardware or an external score (OpenSSF Scorecard) run on the
release benchmark rig or as a release check.

| Dimension | What it means to the user | How OpenXet delivers it | Target | Checked in |
|---|---|---|---|---|
| Speed | Uploads, downloads and listings never wait on OpenXet | Direct presigned downloads (ADR 0002), stateless nodes, a fast index, the cache tier (§10.5) | At 1,000 requests/s per node, against an index of 10⁷ files and 10⁷ xorbs: reconstruction p99 < 50 ms for files up to 100k terms; token, head and resolve p99 < 10 ms (§5.8). ≥ 9 Gbit/s sustained xorb ingest on one node | Release rig |
| Dedup and efficiency | Only changed bytes move or get stored | Client-side chunking with global dedup; server-side chunking for every other client (§10.3); our client's Git LFS download agent (§6.1) | A 1 MB edit of a 10 GB file transfers ≤ 2 MB up with stock `hf_xet`, also from a second machine with an empty cache; ≤ 2 MB down with our client when the previous revision is in the local Xet chunk cache (§10.8 can seed it from an existing LFS checkout); storage overhead over unique compressed data ≤ 5% | Per-PR CI |
| Compatibility | The tools they already use just work, in both directions with huggingface.co | Byte-exact formats on `xet-core`'s crates (ADR 0001); the Hub and LFS surfaces of §6 | 100% of the compatibility suite passes for the oldest supported, pinned and latest clients; zero divergence on the golden vectors | Per-PR CI; latest clients also nightly |
| Reliability and integrity | Acknowledged data is never lost, corrupted or silently dropped | The object-store bucket is the source of truth (§5.3); every xorb and shard is verified on ingest (§5.9); deletion only through GC (§5.6) | Zero acknowledged writes lost under SIGKILL fault injection; `reindex` from the object-store bucket alone restores every file, with refs at most 60 s stale (RPO); `verify` has zero false positives and exits non-zero on real corruption | Per-PR CI |
| Time to value | Working in minutes, with nothing to learn first | One binary, zero-config dev mode (§7), copy-ready commands | First `hf upload` of a ≥ 1 GiB file < 2 min from a clean machine self-hosted, < 60 s from the landing page on openxet.com (§15) | Release check (rig for self-hosted, the hosted service for openxet.com) |
| Control and sovereignty | Their object-store bucket, region and identity system; no lock-in | Storage and auth adapters (§5.3, §5.4); pools and residency (§10.7); bring-your-own object-store bucket on openxet.com | No byte crosses a pool boundary (residency test); data written by OpenXet reads with stock clients and huggingface.co tooling, and the reverse | Per-PR CI |
| Security and privacy | Only the right people read or write, and nothing leaks | The requirements of §5.9, each with its test | Every §5.9 test passes; OpenSSF Scorecard ≥ 9 (§16) | Per-PR CI; Scorecard as a release check |
| Reproducibility | A revision always returns the same bytes | Content-addressed files and Git commit OIDs; data keyed by commit is immutable | A download by commit OID is byte-identical across nodes, cache states and consecutive releases (sha256-checked in the upgrade tests) | Release check (upgrade tests) |
| Cost transparency | Users see what they store, what dedup saved and what it costs | Logical and physical metering per scope and tenant (§5.7), shown in the web UI and `--json` | The sum of metered physical bytes across tenants is within 1% of object-store bucket bytes, excluding objects pending GC; per-tenant physical bytes are exact for dedicated-bucket and `per-tenant` tenants; savings shown on every repository and bucket page | Per-PR CI |
| Agent UX | Agents use every surface without a human in the loop | §7.1: `--json` with versioned schemas, stable exit codes, structured errors with a hint, MCP | The agent leg passes with zero prompts, zero unparseable outputs and zero schema breaks | Per-PR CI |

**Where OpenXet beats huggingface.co:**
- Dedup for every client. Our client fetches only missing chunks on
  `git clone` / `git pull` (§6.1), and server-side chunking dedups uploads
  from clients that don't speak Xet (§10.3).
- Your own object-store bucket and region, plus caches near your GPUs (§10.5, §10.7).
- Mount and stream any revision (§10.8).
- Savings shown to the user, in bytes and money (§10.10, §15).

**Explicit trade-offs:**
- **Dedup vs read fragmentation.** The more a file dedups against others,
  the more xorbs a read touches. Stock clients already skip dedup of very
  short ranges (`xet-data` 1.7.0, `deduplication/defrag_prevention.rs`),
  the cache tier offsets the rest, and the benchmark suite tracks ranges
  per GB read.
- **Global dedup vs privacy.** Global dedup reveals whether a chunk
  already exists, even through HMAC-keyed answers. `per-tenant` dedup
  (§5.4) removes that signal and costs cross-tenant savings.
- **Direct vs proxied downloads** (ADR 0002). Direct presigned URLs save
  node bandwidth but need a store that binds a signature to one range.
  Proxied reads work everywhere and cache, at the cost of node bandwidth.
- **Client-side vs server-side chunking.** Client-side chunking saves
  bandwidth; server-side chunking covers every client but costs server
  CPU and a full transfer.

## 2. Goals, non-goals, anti-goals

### Goals (v1)

1. **Protocol-exact Xet CAS server.** It implements the spec's CAS API and
   produces/accepts byte-identical hashes, xorbs and shards.
2. **Hub-compatibility layer.** It implements just enough of the Hub HTTP API
   that `huggingface_hub` and the `hf` CLI work unmodified via `HF_ENDPOINT`:
   create repo, upload, commit, download, list, branches and tags.
3. **Bring your own everything.** Storage, database and identity are adapters.
4. **Horizontal scalability from day one.**
5. **Fast.** Speed is a measured target, with benchmarks gating CI.
6. **Easy to operate.** One binary, one config file, sane defaults, metrics,
   and recovery from the object-store bucket alone.

### Non-goals (v1)

- No HF-style product features: model cards, discussions, likes, search,
  running Spaces or inference. The only web UI is minimal self-service
  (§10.10).
- No user accounts, passwords or user database. OpenXet never owns users.
- No custom distributed object store (§5.3). This is a principle, not a
  deferral: durability is what object stores are for.

Scope rule *(decided by the user)*: be ambitious. Nothing in this design is
deferred for convenience. Everything below the principles above ships in 1.0.

### Anti-goals (failure even if it technically works)

- **Slow**, at upload, download or metadata operations.
- **Hard to set up**: more than one command and one variable to first upload,
  confusing config, or mandatory extra services.
- **Not listening to users**: feedback that has nowhere to go or is not acted on.
- Opinionated all-in-one: forcing its own auth, naming, UI or storage on people.
- Silent incompatibility: anything that makes data written by OpenXet unreadable
  by stock Xet clients, or vice versa.
- Losing data. Deletion happens only through explicit, auditable garbage
  collection with a grace period.
- AI slop: speculative abstractions, comment noise, dead code, untested paths,
  docs that drift from behaviour.

## 3. Constraints and verified facts

These were checked against primary sources on 2026-10-23
(spec at huggingface.co/docs/xet; `huggingface_hub` main @ `03b2828`;
`xet-core` main), and by the M0 spikes against `huggingface_hub` 2.2.0 and
`hf_xet` 1.7.0 (§11). Re-verify before relying on them; the compatibility
test suite (§8) is the lasting source of truth.

- **CAS API** (spec "api" page): `GET /v2/reconstructions/{file_id}` (preferred;
  clients fall back to v1 on 404/501), `GET /v1/reconstructions/{file_id}`
  (deprecated), `POST /v1/reconstructions` (batch), `GET
  /v1/chunks/default-merkledb/{hash}` (global dedup, returns a shard),
  `POST /v1/xorbs/default/{hash}`, `HEAD /v1/xorbs/default/{hash}`,
  `POST /v1/shards`, `POST /v2/shards` (NDJSON progress stream; clients fall
  back to v1 on 404 or 501), `HEAD /v1/files/{file_id}`, `GET
  /v2/file-chunk-hashes/{file_id}` (delta uploads). Hash-to-hex conversion
  reverses bytes within each 8-byte group, which is a classic source of
  bugs.
- **What stock `hf_xet` 1.7.0 actually sends** (S6, source and observed):
  - Global dedup queries go to `GET /v1/chunks/default/{hash}`, not the
    spec's `default-merkledb`. OpenXet serves both.
  - It sends `POST /v1/telemetry`. OpenXet accepts it and drops the body:
    nothing is stored or forwarded (no telemetry, §7).
  - It tries `POST /v2/shards` first and falls back to `/v1/shards` on 404
    or 501.
  - An empty file is a file entry with 0 segments, an all-zero file hash
    and an all-zero sha256.
  - The wasm build uploads shards **with** lookup tables and a footer,
    although the spec says the footer must be omitted. OpenXet accepts a
    consistent footer and discards the tail (S6 rule 3).
- **The CAS wire contract** (S10, `hf_xet` 1.7.0, source and observed):
  - A storage-only upload and download uses seven routes: dedup query,
    xorb upload, `/v2/shards` and `/v1/shards`, `/v2/` and `/v1/`
    reconstruction, and telemetry. Nothing else is needed.
  - Every write is retried as a whole on `5xx`, `408`, `429` and
    timeouts, up to 6 attempts, so xorb and shard uploads must be
    idempotent.
  - `404` and `501` on a `/v2/` route make the client fall back to `/v1/`.
    A content failure on `/v2/shards` is therefore an in-stream `error`
    event, never a status.
  - A reconstruction whose range starts at or past EOF answers `416`.
  - Data URLs are fetched with a single `Range` and no credential.
  - The client never reacts to `401`; it refreshes tokens only by clock.
  - Stock clients cache shards for 21 days and dedup against them
    without asking the server, which sets the GC grace floor (§5.6).
- **Shard sha256** is stored in the hash layout: the raw bytes are the
  digest with each 8-byte group reversed. It is client-asserted; the
  server can't check it without the file's bytes, so the sha256 → Xet
  hash mapping is scoped to the repo whose writer uploaded it (§5.4).
- **Shard verification** (S6): twelve rules, all checkable from the chunk
  hashes and lengths recorded at xorb ingest, never by reading a xorb
  back. `verify_shard` caps the referenced chunks per shard
  (`MAX_REFERENCED_CHUNKS`, default 2^24, about 1 TiB of logical data) and
  the distinct referenced xorbs (`MAX_REFERENCED_XORBS`, default 4096,
  which bounds the chunk tables the caller loads), and rejects shards that
  reference missing xorbs. Xorbs the CAS info section lists but no segment
  references are ignored. Reference and real
  `hf_xet` shards pass.
- **Auth:** clients obtain `{accessToken, exp, casUrl}` from
  `GET {HF_ENDPOINT}/api/{repo_type}s/{repo_id}/xet-{read|write}-token/{revision}`.
  The CAS token is scoped to repo and ref, and `write` supersedes `read`.
  The token routes must return the token **twice**: in the JSON body and
  as headers `X-Xet-Cas-Url`, `X-Xet-Access-Token`,
  `X-Xet-Token-Expiration`. `huggingface_hub` reads only the headers and
  `hf_xet` only the body (S3). Clients refresh about 30 s before `exp`.
- **Download discovery:** `HEAD/GET /{repo}/resolve/{rev}/{path}` returns
  `X-Xet-Hash` plus a `Link: <…>; rel="xet-auth"` (or `X-Xet-Refresh-Route`)
  header. If the redirect is followed, plain HTTP content comes back.
- **Upload flow in `huggingface_hub`**, when `hf_xet` is installed (the
  default):
  1. `POST /api/{type}s/{id}/preupload/{rev}` decides "regular" vs "lfs"
     per file.
  2. The Xet upload runs with a write token; `hf_xet` computes sha256 during
     chunking.
  3. `POST /api/{type}s/{id}/commit/{rev}` sends NDJSON that references large
     files by **sha256** oid.

  So the Hub layer must map sha256 to Xet file hash. The shard's file metadata
  carries the sha256.
- **Uploads go through the server today; direct xorb uploads are coming.**
  Stable clients up to `hf_xet` 1.7.0 POST xorb bytes to the CAS, which
  verifies them. Upstream is adding **xorb upload grants**
  (`XorbUploadGrant`, researched 2026-10-23):
  - **Shape.** `POST /v1/xorb-grants/{prefix}/{hash}` with
    `{"length", "checksum": {"algo": "crc64nvme", "value": "<decimal>"}}`
    answers `201` with a grant `{id, method, url, headers}`, or `200` if
    the xorb exists. The client sends the bytes to `url` with exactly
    those headers and no CAS credential, then commits with
    `POST /v1/xorb-commits/{prefix}/{hash}/grant/{id}`. A `404` commit
    means no data arrived, and the client asks for a new grant.
  - **Fallback.** A `404` on the grant request makes the client use
    `POST /v1/xorbs` instead, and so does running out of re-grants
    (`HF_XET_CLIENT_MAX_XORB_GRANT_ATTEMPTS`, default 3). Any other error
    fails the upload.
  - Browser (wasm) clients use grants too, so grant URLs need CORS.
  - **Who uses it.** `xet-core` `main` has only the types. The client
    flow is on tag `v1.8.0-dev0`, shipped as the PyPI pre-release
    `hf-xet` 1.8.0.dev0 (2026-10-20), where it is on by default
    (`HF_XET_CLIENT_LEGACY_DIRECT_XORB_UPLOAD=1` turns it off). No stable
    release uses it; `hf-xet` 1.7.0 has no grant code (checked in the
    published wheels).

  OpenXet answers `/v1/xorb-grants/*` with `404` until it supports grants,
  never `501`, so these clients fall back. How it will support them is
  in §5.5.
- **Downloads can bypass the server** (verified by spike S1,
  `docs/research/S1-S2-direct-downloads.md`; decided in ADR 0002). v2
  reconstruction returns signed URLs, which `hf_xet` treats as opaque.
  `hf_xet` 1.7.0 splits multi-range fetch entries into single-range GETs by
  default. With `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` on, it sends a
  multi-range header. Presigned MinIO answered that with the whole object
  (observed with `hf_xet`). Anonymous public-object GETs on AWS S3 and Azure
  also returned the whole object, and GCS rejected them; presigned behaviour
  on those three is unverified. OpenXet therefore emits **one range per
  fetch entry, each with its own range-bound URL**. Stock `hf_xet`
  downloads correctly from such SigV4 presigned URLs on MinIO.
- **Global dedup:** only "eligible" chunks are queryable: the first chunk of
  each file, plus chunks whose hash's last 8 bytes (LE u64) % 1024 == 0.
  Responses are shards whose chunk hashes are HMAC-keyed. This keeps the
  global dedup index about 1/1000th of the chunk count, which is what makes
  scale tractable.
- **HF Storage Buckets** (mutable, no Git revisions) have their own API
  in `huggingface_hub` 2.2.0, not a repo type:
  `/api/buckets/{namespace}/{name}`, its `tree`, `paths-info` and `batch`
  routes, `/buckets/{id}/resolve/{path}`, and
  `/api/buckets/{id}/xet-{read|write}-token` with no revision. `bucket` is
  a URI type there, not one of `REPO_TYPES`. OpenXet serves them in v1 as
  its bucket primitive (§10.4).
- **Repo types in `huggingface_hub` 2.2.0** are route prefixes, which
  OpenXet serves as typed views of one repository (§6, ADR 0010):
  - `/api/{type}s/…` for `model`, `dataset`, `space` and `kernel`, and
    the `datasets/`, `spaces/` and `kernels/` prefixes of resolve and Git
    URLs (`REPO_TYPES_URL_PREFIXES`).
  - A missing repository is `404` with `X-Error-Code: RepoNotFound`,
    which `hf_raise_for_status` turns into `RepositoryNotFoundError`
    (`utils/_http.py`; S3 fixture `24-repo-info-not-found`).
  - A repository that exists answers `POST /api/repos/create` with `409`
    and `{"error", "url"}`; `create_repo(exist_ok=True)` treats it as
    success and returns the `url` (`hf_api.py` `create_repo`; S3 fixtures
    `05` and `06`).
  - `kernel` repositories are created, read and downloaded, but the
    commit and upload functions accept only `REPO_TYPES`, which has no
    `kernel`.
- `xet-core` crates (`xet-core-structures`, `xet-data`, `xet-client`) are
  Apache-2.0 and contain the reference merklehash, xorb, shard and chunking
  code.

## 4. Decisions made with the user

- Build both the storage core and the Hub-compat layer in v1, as two cleanly
  separated parts of one product. The storage core is usable alone.
- Plug-and-play over all-in-one; not an HF clone.
- OpenXet does not own users; identity and permissions come from the
  operator's existing systems.
- Scaling is designed in from the beginning.
- Storage is an adapter over existing object stores, not a built-in
  distributed store. Local disk is a first-class backend for single-node and
  dev.
- Rust. Apache-2.0.
- Quality bar: excellent `AGENTS.md`, best practices, useful tests kept
  current, current docs, check git history before changes to avoid regressing
  past decisions, deliberate distribution, no AI slop, user-obsessed.
- Our own client is in v1, and must stay 100% compatible with the official
  tools and huggingface.co (§10.8).
- `git-xet` and Git over SSH are core, v1 (§6.1, §10.1).
- Build straight to 1.0 with no alpha pause (§12).
- Be ambitious: nothing is deferred for convenience. That brings in
  server-side chunking, both storage primitives (§10.4; this replaced
  "all repo types including `bucket`" in ADR 0010), the cache tier, the
  HF pull-through mirror, multiple storage pools and residency, and the full
  distribution list (§9, §10).
- OpenXet issues revocable access tokens (and SSH keys) bound to external
  identities (§5.4).
- The name is OpenXet.

## 5. Architecture

### 5.1 Shape

```
   git · git-lfs · git-xet · hf CLI / huggingface_hub / hf_xet · huggingface.js · openxet client · browser
        │ Git (HTTP/SSH)  │ Hub API · LFS batch   │ CAS API          │ xorb ranges        │ web
        ▼                 ▼                       ▼                  ▼                    ▼
 ┌──────────────────────────────── openxet (stateless nodes, one binary) ─────────────────────────┐
 │ git + ssh │ hub layer          │ cas core           │ transfer               │ web (optional) │
 │ upload/   │ repos/refs/commits │ xorb/shard verify, │ signed-range reads,    │ tokens, keys,  │
 │ receive-  │ tree/resolve/      │ index, reconstruct,│ HTTP gateway,          │ browse, usage  │
 │ pack, LFS │ tokens, LFS batch  │ server-side chunk  │ cache tier, HF mirror  │                │
 └─────┬─────┴─────────┬──────────┴─────────┬──────────┴───────────┬────────────┴────────────────┘
       │ auth adapters │ metadata index     │ object store pools   │
       ▼               ▼                    ▼                      ▼
  OIDC/JWT · webhook · Postgres | SQLite   S3 · GCS · Azure · MinIO/R2/Garage · local disk
  static · access tokens · SSH keys
```

- One binary, `openxet`. Roles (`git`, `ssh`, `hub`, `cas`, `transfer`, `web`) are enabled by config.
  Default is all-in-one; large deployments split and scale roles separately.
- Nodes hold no state that matters. Any node can serve any request. Scale by
  adding nodes behind a load balancer.

### 5.2 Components (crate-level seams; exact layout is the builder's call)

- **protocol**: hash-string conversion, xorb and shard parsing, serialization
  and verification. Prefer depending on `xet-core-structures` for the
  reference implementation; wrap it so upstream churn is contained in one
  place. Spike S5 confirmed it covers everything the server needs, with an
  exact version pin (ADR 0001).
- **cas**: the CAS endpoints. Xorb ingest: stream, parse, recompute chunk
  hashes and xorb hash, reject on mismatch, write to the object store, record
  the chunk table. Shard ingest: verify that every referenced xorb exists and
  that verification entries match stored chunk hashes, then register files,
  their reconstruction terms and sha256 metadata, and eligible chunks for
  global dedup. Reconstruction, batch reconstruction, global dedup query,
  head endpoints, `/v2/shards` progress streaming, `/v2/file-chunk-hashes`.
- **index** (trait plus implementations): xorbs (hash, size, chunk byte
  offsets), files (hash, size, compact term list, sha256), dedup entries
  (eligible chunk → shard/xorb), scope membership (§5.4), GC metadata.
  Postgres for production, partitioned by hash prefix. SQLite for single
  node. The trait leaves room for a distributed KV later without touching
  callers.
- **storage** (adapter over the `object_store` crate or equivalent): S3
  and S3-compatible stores, GCS, Azure, local filesystem. Presigned-URL
  generation where the backend supports it.
- **transfer**: serves xorb byte ranges for backends without presigned URLs
  (local disk), for backends that cannot bind a range, and for an
  optional NVMe or CDN cache tier. Signed, short-lived, range-bound URLs.
- **hub** (optional): the Hub-compat API (§6).
- **auth**: authenticators, authorizers and CAS token minting (§5.4).
- **gc**: garbage collection (§5.6).
- **server**: wiring, config, roles, health, metrics, tracing.

### 5.3 Storage

Building our own distributed object store means taking on replication,
repair and durability: years of work, and the one place a bug destroys user
data. Xet already stores data as large objects (xorbs up to 64 MiB) read via
range requests, so per-request object-store overhead is small. Speed comes
from:

- presigned direct downloads
- an optional cache tier
- a fast index

**The object-store bucket is the source of truth.** Xorbs, uploaded shards, Hub-layer
commit objects and storage-only pins are written to the bucket, and so is
who may read what *(ruling M1-R37)*:
- a **registration record** per shard registered to a scope
  (`registrations/{scope}/{tenant}/{shard}`, holding the scope, the tenant
  and a sequence number), written before the index records the
  registration and never changed after;
- an **ownership record** per xorb a tenant uploaded
  (`xorb-owners/{tenant}/{xorb}`).

A bucket primitive's keys are written there too (§10.4, ADR 0010):
- a **bucket log record** per write request
  (`bucket-log/{scope}/{seq}`, holding the request's key operations),
  created only if absent and before the write is acknowledged;
- a periodic **bucket snapshot** (`bucket-snapshot/{scope}/{seq}`,
  holding every key's file and sequence number as of `seq`), which bounds
  what `reindex` replays.

And so are labels (§10.4): a **label record** per repository and per
bucket, holding its labels, rewritten on each change before the change
is acknowledged.

Scopes and tenants appear in keys only as the SHA-256 of their name. The
database is an index that `openxet reindex` can rebuild from the bucket:
every scope table follows from these records with the stored shards and
xorbs. Losing the database must never lose data, nor anyone's access to
it.

Durability details *(builder-check calls)*:
- Commit objects and storage-only pins are written to the bucket
  **synchronously**, before they are acknowledged.
- Branch and tag pointers live in the database, which is authoritative while
  it exists.
  - Every ref change also inserts a row into the database ref log in the
    same transaction (ADR 0004).
  - The ref log in the bucket is derived from those rows within seconds
    (documented RPO ≤ 60 s).
  - So after total database loss, `reindex` may roll a branch back to an
    earlier commit, but no data or commit object is lost. Newer commits it
    cannot place on a branch, and annotated tags no ref reaches, are
    listed as recoverable, by `reindex` and by `verify`.
  - A pack that holds no object reachable from a rebuilt ref goes to a
    `recovered` state (ADR 0004). It is never served and GC never
    collects it.
  - Unplaced commits, unplaced tags and recovered packs are persisted in
    the database. `verify` keeps listing them, and repacks keep everything
    they need, so nothing restorable is dropped. Full repacks are never
    blocked.
  - Nothing is discarded automatically. A metric and an alert flag
    unresolved repos, and an operator, after review, runs `openxet
    reindex restore` or `openxet reindex discard` (per repo or bulk,
    with `--dry-run`). Until then the data stays stored.
  - `discard` is **immediate and irreversible** for unplaced objects in
    committed packs: their mtime is their pack's creation time, so the
    next full repack drops every one whose pack is older than the grace
    period, with no further grace. Recovered packs still get
    GC's grace period. `--dry-run` and the docs say so (ADR 0004 item 4).

### 5.4 Identity, authorization and tenancy

OpenXet never stores users. A request carries a credential; an
**authenticator** turns it into a principal (subject plus claims); an
**authorizer** decides read or write on a repo or scope.

Authenticators (v1):
- **OIDC/JWT**: validate tokens from any OIDC issuer via JWKS (Keycloak,
  Auth0, Okta, Entra, Google, GitHub Actions OIDC and others).
- **Static tokens**: from config, for dev, CI and small setups.
- **OpenXet access tokens** *(decided by the user)*: long-lived, revocable
  tokens for tools like `hf auth login`, each bound to an external identity.
  - A user proves who they are once through the operator's identity system,
    for example `openxet login` (OIDC device or browser flow), a minimal
    login page, or the platform's backend calling the admin API.
  - OpenXet then issues a token. It stores only a hash plus the bound subject,
    claims snapshot, scopes, expiry and last use.
  - Authorization is re-evaluated against the authorizer on use, so revoking
    access upstream takes effect even if the token itself is still valid.
  - OpenXet still owns no users: no passwords, profiles or sign-up.
  - `whoami-v2` answers from the bound identity, with names and orgs mapped
    from claims via config or the webhook.
- **Who may create repos:** decided by the authorizer, as a `create` action on
  the namespace.

Authorizers (v1):
- **Config rules** on claims and repo patterns (e.g. group `ml-team` may
  write `org/*`).
- **Webhook**: OpenXet asks the operator's backend "may subject X read/write
  repo Y?", with a short-TTL cache. This is how platforms like aqora plug in.

**User credentials** *(ruling M2-R3,
[ADR 0012](../../adr/0012-repository-identity-and-authorization-rules.md))*:
a credential is a CAS credential (scope, tenant, mode, as in M1), a user
credential (a subject plus claims, string lists under free names such as
`orgs` and `groups`), or both. A static token without `scope` is a user
credential, one with `scope` is a CAS credential, and one with a `scope`
and user fields (`name`, `email`, `claims`) is both, as the dev token is.
CAS routes take the CAS credential; Hub, Git and LFS routes take the user
credential and ask the authorizer for `read` or `write` on the repository,
or `create` on the namespace. `whoami-v2` answers `name` from the subject
(or the static token's `name`) and `orgs` from the `orgs` claim. Until
OIDC and OpenXet access tokens arrive in M3, static tokens are the user
credential; both plug into the same seam.

**Config rules** *(ruling M2-R3, ADR 0012)*: `[[auth.rules]]` entries
match a principal by `subjects` and/or `claims` (a listed subject, or one
listed value under a named claim), name `repos` by glob over
`{namespace}/{name}` (`*` stays within one path segment, `*` alone
matches everything, and matching ignores ASCII case) and grant `actions`
(`read`, `write`, which includes `read`, and `create`, granted only by a
pattern covering the whole namespace: `org/*` or `*`). Subjects match
exactly; `*` is not a wildcard there. Rules only grant:
a request is allowed if any rule allows it, with no order and no deny
rule, so a server's access is audited by reading the rules that name a
repository. A rule naming the pseudo subject `anonymous` grants everyone,
signed in or not, so an operator can make repositories public; no rule
names it by default. Dev mode installs `subjects = ["dev"], repos = ["*"], actions
= ["read", "write", "create"]`.

CAS tokens are short-lived EdDSA JWTs (ADR 0007), scoped to
(scope, read|write, exp), with no revision, because Xet reads follow scope
membership (R25) and a revision would enforce nothing. The Hub token route
still accepts a revision in its URL for compatibility (M2). Every node
verifies them statelessly against a key set of public keys, so keys rotate
without downtime, and a platform can sign them itself. `exp` is honoured
exactly. Verification is algorithm-agile: each key is bound to one `alg`,
and a token's header must match its key's, so ML-DSA (RFC 9964) can be
added by rotation when platforms can sign with it
([ADR 0011](../../adr/0011-post-quantum-readiness.md)).

**Storage-only mode:** a platform with its own naming layer gets CAS tokens
for its users either through an admin endpoint or by signing them itself
with a configured key. It then stores Xet file hashes in its own database.

**Scopes and tenants (builder-check call, ruling M1-R38):**
- A **scope** is the unit of access. In the Hub layer a scope is a
  repository, matching HF's repo-scoped Xet tokens, or a bucket. A
  repository is addressed by `{namespace}/{name}` and its scope is
  `repo:{id}` (ADR 0012, below). A bucket's scope string is M5's to
  define; `bucket/{namespace}/{name}` is a placeholder until then, and
  whatever M5 picks can't start with the reserved `repo:`, so the two
  never collide. Labels are not part of a scope. Scope strings are
  persisted (their SHA-256 keys every registration record, §5.3), so
  both kinds map to an immutable scope id and a rename never orphans
  their data. In storage-only mode it is an opaque string the platform
  chooses. Files are read and registered per scope.
- A repository's **scope id** *(ruling M2-R2, ADR 0012)* is a UUIDv7
  assigned at creation, rendered as 32 lowercase hex characters; its CAS
  scope is `repo:{id}`, and the bucket keys for its data carry the id
  (`repos/{id}/…`). The scope prefix `repo:` is reserved for
  repositories: a configured static token whose `scope` starts with it
  is a configuration problem, and a CAS token a platform signs with such
  a scope is accepted, because the platform holding a signing key is
  trusted with any scope.
  Namespace and name live in the repository's metadata record, so a
  rename rewrites one record. Namespace and name each match
  `^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$` and contain no `..`, the name
  doesn't end in `.git`, and `{namespace}/{name}` is unique
  case-insensitively, stored as written, across labels (ADR 0010).
  `models`, `datasets`, `spaces`, `kernels`, `buckets`, `api`, `v1`,
  `v2`, `openapi.json`, `healthz` and `readyz` can't be namespaces,
  because they are route prefixes.
- A **tenant** is the unit of isolation for `per-tenant` dedup and
  metering. Every principal carries one. In the Hub layer it is the
  namespace, the first path segment, and the Hub and LFS token routes
  mint CAS tokens with scope `repo:{id}` and that tenant (ADR 0012). In
  storage-only mode it is the CAS token's optional `tenant` claim (ADR
  0007) or a static token's configured `tenant`. When none is given it
  is `scope:` followed by the scope, and no explicit tenant may start
  with `scope:` *(ruling M1-R43)*, so a scope named like a tenant never
  joins it. So the scopes of one tenant share dedup under `per-tenant`:
  an org's fine-tune dedups against its base model in another repo.
- A commit in repo B may only reference files registered to repo B. A file
  already in repo A is registered to B by uploading it to B; dedup makes
  that nearly free, because only a shard is sent.
- A bucket `batch` names files by Xet hash, so each reference must prove
  the caller can read it. `addFile` needs the hash registered to the
  bucket's own scope. `copyFile` needs a read credential on its source
  repository or bucket and the hash registered there, and then registers
  the file to the bucket's scope. Under `per-tenant`, the hash must also
  be visible to the caller's tenant. Anything else answers the same
  `404` as an unknown hash, so a `batch` is never an existence oracle
  (the pattern of M1-R17 and M1-R23).

**File access scoping:** files registered by a shard upload are associated
with the token's scope, and so is the sha256 → Xet hash mapping from the
shard (never global, since the sha256 is client-asserted). Reconstruction
and head-file only succeed for files registered to the caller's scope.
Xorbs remain globally content-addressed and shared.

**Xet reads by hash follow scope membership, not Git reachability**
*(ruling R25)*:
- Readers of a scope may read any file registered to it until GC collects
  it. That includes a file that only force-pushed-away commits referenced,
  although Git fetches and Hub reads by revision refuse those commits
  (R22, ADR 0004).
- Why: a Xet hash is an unguessable 256-bit content hash. A reader can
  only have it from history they could see while it was current.
- The trade-off is for users: reads stay stable, so in-flight downloads
  and caches don't break when a branch moves.
- **Security implication:** force-pushing a secret out of history does not
  revoke access to it. Anyone in the scope who saw the hash can still
  download the file. Removing a secret takes a force-push **plus** GC, or
  `openxet purge <file-hash>` (M2+). Purge unregisters the file from every
  scope at once and logs it. New reconstructions stop immediately;
  already-issued URLs expire within their lifetime (at most 60 min,
  ADR 0002, ruling M1-R42). Purge also evicts the file's ranges from the cache tier. The
  bytes then leave storage through GC (§5.6), once no other file
  references their xorbs. Removing a reader from the scope revokes their access once their
  current CAS token expires.

**Global-dedup visibility** is configurable:
- `global`, the default, follows HF's model with HMAC-protected responses.
- `per-tenant` is for multi-tenant hosting, where revealing whether a
  chunk already exists across tenants is unacceptable.

A dedup answer offers every xorb of the newest visible shard that
references the matched xorb, up to 16 MiB, so one hit lets a client with an
empty cache dedup a whole earlier upload (rulings M1-R30, M1-R32, S11); under
`per-tenant` that shard must be registered for the caller's tenant and
only the tenant's xorbs are offered.

**Existence checks in `per-tenant` mode** answer per tenant too, or they
would reveal what dedup answers hide. Every check that says whether a xorb
exists is filtered by the caller's tenant, and every check that says
whether a file exists by the caller's scope:
- `HEAD /v1/xorbs/…` and dedup lookups (`GET /v1/chunks/…`), by tenant
- the xorb lookups of shard verification (`verify_shard`'s
  `XorbChunkLookup`), by tenant
- `HEAD /v1/files/…` and reconstructions, by scope, in either mode

A tenant owns a xorb once it was uploaded under the tenant or a file
registered for the tenant references it *(ruling M1-R39)*. The second is
recorded in every mode, so switching `global` to `per-tenant` leaves every
registered file readable and its upload dedupable by its own tenant. A
xorb the caller's tenant doesn't own answers exactly as if it were absent
(`404`, no dedup match, `UnknownXorb`), so the client uploads it, which
makes the tenant an owner. Storage stays shared: the upload of a xorb
already stored writes only its small ownership record. Tests:
`per_tenant_existence_checks_hide_other_tenants`,
`per_tenant_scopes_of_one_tenant_share_dedup`,
`switching_global_to_per_tenant_keeps_reads_and_dedup`.

### 5.5 Data paths

- **Upload:** client → CAS (`POST xorbs` streamed and verified into the object
  store; `POST shards` verified and registered). This scales with node count.
  Today every xorb byte goes through OpenXet (§3).
  - **Xorb upload grants** are a tracked future item, not an ADR yet, since
    no stable client uses them (§3). When clients ship them, OpenXet
    supports them with **verification before registration**: the grant
    URL is a presigned PUT to a staging key, bound to the declared length
    and CRC64-NVMe checksum where the store can enforce them. The commit
    call reads the staged object back, recomputes the chunk hashes and
    xorb hash exactly as `POST /v1/xorbs` does, and only then moves it to
    its content-addressed key and records its chunk table. A mismatch
    deletes the staged object and fails the commit. Unverified bytes are
    never readable and never count for dedup or shard verification.
    Staging keys expire with their grant, and GC sweeps the ones left
    behind. Grant URLs allow CORS for browser clients.
  - Until then `/v1/xorb-grants/*` answers `404`, so grant-aware clients
    fall back to `POST /v1/xorbs`.
- **Download:** client → CAS reconstruction (small JSON). Then either:
  - **(a)** direct from the object store via native presigned URLs, one
    range-bound URL per fetch entry (S1, ADR 0002); or
  - **(b)** via the transfer role, with OpenXet-signed range URLs (also one
    range per entry, so no `multipart/byteranges`), for local disk, caching
    setups and any pool that fails the range-binding probe. The URL form is
    `{public_url}/v1/transfer/xorbs/{xorb}?range={start}-{end}&exp={unix}&sig={b64url}`,
    where `sig` is an HMAC-SHA256 over the xorb, range and expiry, keyed with
    the transfer key. The request must carry exactly the signed `Range`: a
    missing header is a `400`, and a different range, an expired URL or a bad
    signature is a `403`.

  Mode (a) is gated per deployment and pool by a **range-binding
  enforcement probe**, not by a backend list (ADR 0002). It runs at
  startup and in `openxet doctor` / `openxet config check`. OpenXet
  presigns a GET for one range of a probe object and requires all three:
  the signed range returns `206`, a different range returns `403`, and no
  `Range` header returns a `4xx`. Only then does the pool serve direct
  URLs; otherwise it falls back to (b) and logs why. This applies to every
  backend (S3, R2, Garage, MinIO, and GCS or Azure once their signers can
  bind a range). CI runs the probe against MinIO. URL lifetime defaults to
  60 min, with a 30 min minimum as derived in ADR 0002 and a 60 min maximum
  (ruling M1-R42). v1 reconstruction
  is served too, for older clients.
- **Plain HTTP read gateway:** `GET /{repo}/resolve/{rev}/{path}`, when the
  redirect is followed, streams the reconstructed file with HTTP `Range`
  support. This makes OpenXet readable by anything that speaks HTTP (curl,
  browsers, pandas, polars, DuckDB via the plain resolve URL) or `hf://`
  through fsspec `HfFileSystem`. DuckDB and polars hard-code
  huggingface.co in their own `hf://` handlers (spike S4,
  `docs/research/S4-analytics-clients.md`), so they read OpenXet through
  `https://{host}/{repo_type}/{repo}/resolve/{rev}/{path}`. The gateway
  must pass the conformance checklist in that note (HEAD, all three
  `Range` forms, `416`, `ETag`/`If-Range`, redirects, auth); private
  repos need a DuckDB `http` secret, and polars needs an fsspec file
  object. Private-CA https works in DuckDB but not in polars on macOS
  (observed; `docs/research/S4-analytics-clients.md`). It is served by the transfer role so
  it scales and caches independently.

### 5.6 Garbage collection and retention

- **References:** Hub-layer commits reference files, and so does each
  key of a bucket (its current object, plus kept versions); in
  storage-only mode the platform pins files through an admin API,
  optionally with a TTL. Files reference xorbs. Overwriting or deleting
  a bucket key only drops a reference.
- **Mark and sweep with a grace period:** default 30 days, minimum 28
  (21-day client shard cache + 7-day dedup key), and `gc` rejects anything
  lower. Stock clients dedup against cached shards without asking the
  server (§3), so a xorb collected earlier fails their next upload. The
  grace also covers in-flight uploads, because xorbs land before the shard
  that references them.
- **Storage-only default is safe:** files are never collected unless the
  operator enables pin-based retention for that tenant. An integrator who
  never pins loses nothing. *(builder-check call)*
- Running `openxet gc --dry-run` reports what would be deleted. Every deletion is
  logged. Branch and commit deletion in the Hub layer only drops references;
  data goes only via GC.
- **Git pack side files leave with what they describe** (ADR 0004 item 5):
  a pack's `.idx`, `.rev`, `.bitmap` and `.mtimes` are deleted together
  with the pack, and a `multi-pack-index` generation (with its `.bitmap`
  and `.rev`) once a newer generation is committed and the grace period
  has passed. Neither is ever left behind as an orphan.

### 5.7 Metering

Logical bytes (what users uploaded) and physical bytes (what dedup actually
stored) are tracked per scope and per tenant. They are exposed via the admin
API and metrics, because platforms need them for quotas and billing.
*(builder-check call)*

### 5.8 Scale targets (initial; validated by benchmarks, revised with data)

- **Database topology for v1** *(builder-check call)*: OpenXet nodes scale
  out; Postgres scales up. That means one primary, read replicas for
  reconstruction and listing reads, and native partitioning by hash prefix.
  The index trait does not assume a single instance, so a sharded backend
  (Citus, or a distributed KV) can come later without touching callers.
- **Write rate target:** 2,000 xorb registrations/s and 200 shard
  registrations/s per deployment.

- **Index sizing at 1 PB of unique data:** about 1.6×10¹⁰ chunks, about
  1.6×10⁷ xorbs and about 1.6×10⁷ dedup-eligible entries plus first-chunks.
  That fits partitioned Postgres; the chunk-to-xorb mapping lives in per-xorb
  chunk tables, not per-chunk rows.
- **Server-side metadata latency:** reconstruction p99 < 50 ms for files up
  to 100k terms; token, head and resolve p99 < 10 ms (excluding the
  authorizer webhook).
- **Ingest:** a single node sustains ≥ 9 Gbit/s of xorb upload; throughput
  scales roughly linearly with nodes.
- **Time to first upload:** under 2 minutes from a clean machine, measured on
  every release.
- **Memory per node is bounded, whatever clients do** (ruling M1-R40).
  Buffered upload bodies and built reconstruction answers reserve their
  bytes from one process-wide budget (`server.memory_budget`, a quarter
  of the host's memory by default) before they are buffered, and hold
  them until the work they serve has ended or the answer has been sent.
  Uploads past the budget queue, unread, for up to 120 s, so a stock
  client's 64 concurrent xorbs never turn into retries; answers that fit
  pass the queue, and others wait 5 s. Then the request gets
  `503 memory_busy`, which clients retry. One answer may hold at most an
  eighth of the budget, else a final `413 answer_too_large`. Answers are sent
  under a deadline of 30 s plus 1 s per MiB. CPU work is bounded by the
  `validate`, `commit` and `dedup` permits (M1-R8, M1-R24, M1-R32), and
  connections by `server.max_connections`. Measured peaks are in
  [benchmarks](../../reference/benchmarks.md#memory-bounds).

### 5.9 Security requirements

Each requirement is a release blocker and has a named test. The tests are
planned for the milestone that builds the surface (M1 for CAS, M2 for Hub
and Git, later for caches and the mirror); a surface doesn't ship without
its test. [Security tests](../../reference/security-tests.md) maps each
test to its file and milestone, and CI keeps that map and this table in
step.

| # | Requirement | Tests |
|---|---|---|
| 1 | **No unauthenticated reads when auth is on.** Every data URL (reconstruction, xorb ranges, the HTTP gateway, LFS `href`s) needs a credential, or is a signed URL bound to one range and a short lifetime (ADR 0002). | `no_unauthenticated_reads` walks every route and every URL OpenXet emits without a credential; `signed_url_bound_to_range` |
| 2 | **Credentials are scoped to one repository or bucket.** CAS tokens and capability `href`s are scoped to one repository or bucket; signed URLs are bound to one xorb range and an expiry (ADR 0002) and are issued only after the caller's scope check; user credentials are authorized per repository or bucket on every Hub and CAS route. A foreign repository or bucket answers `403`, or `404` where the caller can't read it, never revealing a private one's existence. Repository and bucket creation is authorized per namespace (§5.4). A bucket `batch` that adds or copies a file by Xet hash names a file the caller can read, or gets the `404` of an unknown hash (§5.4). | `credential_scoped_to_repo`, a matrix of every route × read, write and create with another repository's or bucket's credential, bucket routes included; `repo_create_authorized_per_namespace`; `bucket_create_authorized_per_namespace`; `bucket_add_file_hash_must_be_readable`; `bucket_copy_file_requires_source_read`; `wrong_type_without_read_access_matches_unknown_repo`; `create_collision_authorized_first` |
| 3 | **Caches and proxies are keyed by the caller's credential** for anything that depends on authorization. They never share tokens or authorized responses, and they fail closed. The only exception is stale serving of content whose last upstream check was public and ungated (§10.6). | `cache_keyed_by_caller`, `cache_fails_closed` |
| 4 | **Dedup answers reveal no raw chunk hashes.** They are HMAC-keyed shards with a footer, and per-tenant dedup (`per-tenant`, §5.4) is available. In `per-tenant` mode every existence check (`HEAD` of xorbs, dedup lookups, shard xorb lookups) answers as absent for any xorb the caller's tenant doesn't own, and `HEAD` of files for any file not registered to the caller's scope. | `dedup_response_hmac_keyed`, `dedup_answer_never_contains_raw_hashes_or_file_info`, `per_tenant_dedup_isolates_tenants`, `per_tenant_multi_xorb_answer_hides_foreign_xorbs`, `per_tenant_existence_checks_hide_other_tenants`, `per_tenant_scopes_of_one_tenant_share_dedup` |
| 5 | **Uploads are verified.** Xorbs are re-hashed on ingest. Shards are checked against the stored xorb chunk hashes by `verify_shard`, with its work cap (S6), and a shard that references a missing xorb is rejected. Verification errors reveal no value derived from stored data, so they can't stand in for holding the chunks (M1-R23). | the S6 rule tests; `shard_with_missing_xorb_rejected`, `shard_verify_work_capped`, `verification_errors_reveal_no_server_derived_values`, `foreign_writer_cannot_forge_shard_under_global` |
| 6 | **Credentials are never echoed or logged.** LFS download actions carry no CAS token (ADR 0003), no action `header` or response carries the client's own credential, and fixtures and docs pass the redaction guard. | `lfs_download_action_has_no_token`, `responses_never_echo_credential`, `credentials_never_logged` (scans all logs of a suite run), `test_fixture_redaction` |
| 7 | **No silent no-op success.** Every acknowledged write has its effect, and survives a SIGKILL right after the acknowledgement. An acknowledged bucket key write also survives the loss of the database: `reindex` restores it (§10.4). | `acknowledged_xorb_write_is_visible`, `acknowledged_shard_write_is_visible`, `acked_write_survives_sigkill`, `acknowledged_bucket_key_write_is_visible` |
| 8 | **Git reads check reachability on every path** (R22), and **every ref writer is fenced** (ADR 0004). | ADR 0004's `v2_want_unreachable_refused`, `hub_read_by_sha_obeys_r22`, `writer_cannot_make_unreachable_reachable`, `hub_ref_write_fenced`, `ref_log_in_cas_transaction` |

Xet reads by hash follow scope membership (R25, §5.4); that is by design,
not a gap in requirement 8.

## 6. Hub-compatibility layer (v1 surface)

Only what `huggingface_hub` / `hf` need for the workflows below. Exact
request and response shapes come from `huggingface_hub` source and recorded
fixtures, pinned by the compatibility suite. Spike S3 captured them for
`huggingface_hub` 2.2.0 (`compat/fixtures/hub/`,
[S3](../../research/S3-hub-api-shapes.md)).

**Workflows:**
- `hf auth login`
- create and delete repo
- `hf upload` / `upload_file` / `upload_folder` / `create_commit`, including
  deletes
- `hf download` / `hf_hub_download` / `snapshot_download`
- `list_repo_files` / `list_repo_tree` / `get_paths_info`
- repo info
- refs and commits listing
- create and delete branch and tag

**Endpoints (verified to be used by `huggingface_hub`):**
- `GET /api/whoami-v2`
- `POST /api/repos/create` (a `409` carries `url`, which
  `create_repo(exist_ok=True)` reads), `DELETE /api/repos/delete` with a
  JSON body
- `GET /api/{type}s/{id}[/revision/{rev}]`
- `GET /api/{type}s/{id}/refs`, `GET /api/{type}s/{id}/commits/{rev}`
- `GET /api/{type}s/{id}/tree/{rev}/{path}`, `POST /api/{type}s/{id}/paths-info/{rev}`
- `POST /api/{type}s/{id}/preupload/{rev}`
- `GET /api/{type}s/{id}/xet-{read|write}-token/{rev}`, returning the
  token in both the JSON body and the `X-Xet-*` headers (§3)
- `POST /api/validate-yaml`: every commit that adds a `README.md` posts it
  here first, and any non-2xx fails the upload. It answers
  `200 {"errors":[],"warnings":[]}` or real validation, never `501`.
- `POST /api/{type}s/{id}/commit/{rev}` (NDJSON)
- branch and tag create/delete
- `HEAD|GET /{[datasets/|spaces/|kernels/]}{id}/resolve/{rev}/{path}`

**Addressing and typed views**
([ADR 0010](../../adr/0010-two-storage-primitives.md)):
- A repository is addressed by `{namespace}/{name}` only. Its labels are
  generic (§10.4); `model`, `dataset`, `space` and `kernel` are the ones
  the Hub layer reads and writes.
- The typed routes are views of the same repository through one label:
  `/api/{type}s/…`, and the prefixed resolve and Git URLs
  (`REPO_TYPES_URL_PREFIXES` in `huggingface_hub` 2.2.0: `datasets/`,
  `spaces/`, `kernels/`, none for models), e.g.
  `https://openxet.example/datasets/{namespace}/{name}` and its implicit
  LFS endpoint. The unprefixed resolve route is the `model` view, as on
  huggingface.co; the unprefixed Git URL `/{namespace}/{name}.git` is
  OpenXet's own address and serves the repository whatever its labels.
- `POST /api/repos/create` with a `type` adds that type's label. A
  create with no `type` is the `model` view and adds the `model` label,
  as on huggingface.co: `create_repo` in `huggingface_hub` 2.2.0 sends
  `type` only when `repo_type` isn't `None` (`hf_api.py` line 4641), so
  a default `hf upload foo/bar` creates a model. Listing through a typed
  route (`/api/models?…`) returns only repositories with its label.
- Reading or writing a repository through a typed route whose label it
  lacks answers `404` with `X-Error-Code: RepoNotFound` (S3 fixture
  `24-repo-info-not-found`), so stock clients raise
  `RepositoryNotFoundError`, as huggingface.co makes them do for a wrong
  `repo_type`. A caller who can read the repository gets an actionable
  message in `X-Error-Message` and the body's `error`: `foo/bar` exists
  without the `dataset` label; add it with
  `openxet label add foo/bar dataset` (or the label API), or choose
  another name. `huggingface_hub` 2.2.0 appends both to the raised
  error's text (`utils/_http.py`, `_format`). Any other caller gets
  exactly an unknown repository's response: the same status,
  `X-Error-Code`, `X-Error-Message` (`Repository not found`) and body
  (the constant body of M2-R6 under **Errors** below), with no other
  header differing apart from per-request ids and dates. Any difference
  would reveal that the repository exists (the M1-R17 pattern).
- Names are unique across labels, unlike huggingface.co. Creating
  `datasets/foo/bar` while `foo/bar` exists answers `409` in the shape
  `create_repo` reads (S3 fixtures `05` and `06`): `{"error", "url"}`,
  with the message in `X-Error-Message`. With `exist_ok=True` that reads
  as success, and the next call through the `dataset` view gets the
  `404` above, whose hint says how to add the label.
  - The create is authorized on the namespace before the name is looked
    up, so a caller who may not create there gets the same `403` whether
    or not the name exists, and only callers allowed to create in the
    namespace ever see the `409`.
  - For a caller who can't read the existing repository, the `409`'s
    `error` and `X-Error-Message` name only the requested name and
    carry no labels.
  - `url` is the requested view's own URL,
    `{public_url}/{type prefix}{namespace}/{name}`, as in the fixtures.
    `create_repo(exist_ok=True)` returns `RepoUrl(d["url"],
    endpoint=…)` (`hf_api.py` line 4723), and `RepoUrl` parses it with
    `parse_hf_uri` against the client's endpoint, raising `HfUriError`
    on a URL it can't parse (`hf_api.py` lines 545 to 571); `hf upload`
    then uses its `repo_id`. So `public_url` must be the endpoint
    clients use.
- Labels are changed through OpenXet's own API and CLI, never the Hub
  API.
- `huggingface_hub` 2.2.0 creates, reads and downloads `kernel`
  repositories but won't commit or upload to them (its commit and upload
  functions accept only `REPO_TYPES`), so kernels are written over Git
  with `git-xet`.
- Compatibility tests planned for M2, with stock `huggingface_hub`:
  `untyped_create_is_model` (`hf upload foo/bar` with no `--repo-type`,
  then `repo_info` and `snapshot_download` both work),
  `typed_route_view_filters_by_label`,
  `wrong_type_route_is_repo_not_found`,
  `wrong_type_with_read_access_shows_label_hint`,
  `wrong_type_without_read_access_matches_unknown_repo` (compares
  status, headers apart from per-request ids and dates, and body with an
  unknown repository's) and
  `create_collision_answers_409_with_url`.

**Model:**
- Repositories and buckets are separate primitives; buckets have their
  own routes (§3, §10.4).
- Every commit is a **real Git commit**, just as on huggingface.co. Small
  files are Git blobs. Large files are Git LFS pointer files whose sha256 maps
  to a Xet file hash. Commits made through the Hub API (`hf upload`) and
  through `git push` are the same objects, so either can be cloned, pulled
  or downloaded by the other (§10.9).
- Refs are branches and tags with compare-and-swap updates, so concurrent
  commits are safe.
- The Hub API obeys the Git engine's safety rules (ADR 0004 item 6), so
  it can't be used to get around them:
  - Every Hub read and write runs on the same per-request snapshot of
    committed packs as a Git request.
  - A revision given as a commit OID must be reachable from the current
    refs, under the same rule that Git fetches use (R22).
  - Hub ref creates, updates and deletes and Hub commits use the push's
    fenced commit transaction. Hub commits get a connectivity check
    against the snapshot first, and a branch or tag can only be created
    at an object the current refs already reach.

- **The initial commit** *(ruling M2-R5)*. `POST /api/repos/create`
  writes a server-generated `initial commit` on branch `main` holding
  huggingface.co's default `.gitattributes` (the S3 capture's content),
  authored and committed by `openxet <noreply@openxet.com>`, so
  `commits`, `refs` and `git clone` of a new repository behave as on
  huggingface.co.
- **Hub commits** *(ruling M2-R7)* are real commits built from the
  parent's tree: `file` operations become blobs, `lfsFile` operations
  canonical LFS pointers whose sha256 must be registered to the
  repository's scope, and `deletedFile` and `deletedFolder` remove
  entries. The author is the subject
  (`{subject} <{subject}@users.noreply.openxet.com>` unless the
  credential carries an `email` claim), the committer is
  `openxet <noreply@openxet.com>`, and the message is `summary`, a blank
  line and `description`. A
  `parentCommit` mismatch answers `412 precondition_failed`; `commitUrl`
  is `{public_url}/{prefix}{ns}/{name}/commit/{oid}`.
- **Listings paginate deterministically** *(ruling M2-R8)*. `tree`,
  `commits` and the repository listings answer at most 1,000 entries per
  page, ordered by path (tree) or commit order (commits), with
  `Link: <…?cursor=…>; rel="next"` when more follow. The cursor is the
  last entry's key, base64url-encoded. `X-Total-Count` is set where S3
  observed it.
- **Downloads** *(ruling M2-R9)*. `resolve` answers a small file inline
  from its Git blob, with `ETag: "{blob sha1}"`. A Xet file answers `302`
  to the file gateway,
  `{public_url}/v1/transfer/files/{repo id}/{xet hash}?name={filename}&commit={commit}`,
  which serves it with the caller's credential (Bearer, or Basic with the
  token as the password; a `401` challenges Basic first) and HTTP `Range`
  (all three forms, `416` past the end), `ETag: "{sha256}"` (the Xet hash
  for a file registered without one, which `resolve` never redirects
  to), `If-Range`, `HEAD`, `Content-Disposition` and `Accept-Ranges`.
  `commit` is there because `huggingface_hub` follows a same-origin
  redirect with its `HEAD` and reads `X-Repo-Commit`, `X-Xet-Hash` and
  the `xet-auth` link from where it lands (Task 13); the gateway repeats
  them, and echoes the `commit` the caller chose, checked only as a
  commit id, so it is informational, never authoritative. LFS `basic`
  download `href`s are the same URL without `name` or `commit` and with
  no `authenticated` field, so `git-lfs` attaches the credential it holds
  for that host. A gateway body holds its plan's memory reservation until
  it is sent or dropped, or until it goes `hub.gateway_idle_timeout`
  (30 s) without sending a byte, so a slow but steady client always
  finishes and a stalled one holds nothing (M1-R40).

**Errors** *(ruling M2-R6)*. A Hub error carries huggingface.co's `error`
field and OpenXet's structured fields:
`{"error": <message>, "schema": "openxet.error.v1", "code", "message",
"hint"}`, with `X-Error-Code` (huggingface.co's value, such as
`RepoNotFound`, `RevisionNotFound` or `EntryNotFound`, where it defines
one) and `X-Error-Message`. CAS routes keep the M1 body.
- The unknown-repository answer is one constant: `404`,
  `X-Error-Code: RepoNotFound`, `X-Error-Message: Repository not found`
  and the body
  `{"error":"Repository not found","schema":"openxet.error.v1","code":"repo_not_found","message":"Repository not found","hint":"Check the repository id and the token's access: a repository the token may not read answers exactly like one that doesn't exist."}`.
  It answers a missing repository, a repository the caller may not
  read, and a typed view whose label the repository lacks when the
  caller may not read it.
- A missing credential on a repository route is `401` with
  `WWW-Authenticate: Bearer realm="openxet"` and `X-Error-Message:
  Authentication required: pass an openxet access token as the Bearer
  token (hf auth login)`. An invalid one is `401` with `X-Error-Message:
  Invalid credentials in Authorization header`, which `huggingface_hub`
  reports as an authentication error rather than a missing repository
  (S3 answer 6).
- The JSON body is for API routes. Git's smart HTTP routes answer the
  same status and headers with `Content-Type: text/plain; charset=utf-8`
  and the message and hint as two lines, which git shows its user as
  `remote:` lines; their unknown-repository text is one constant too.
  Their `401` offers `Basic realm="openxet"` before the `Bearer`
  challenge, as git asks its credential helper only for Basic.

**Small files** *(ruling M2-R4)*: `preupload` decides whether a file goes
inline in the commit (stored as a content-addressed blob) or through Xet.
It answers `lfs` when the file's size is at least `hub.lfs_threshold`
(default `1MiB`, bounds `1KiB` to `10MiB`) or its path matches an LFS
pattern of the target revision's `.gitattributes` (for a new repository,
the generated one of M2-R5), and `regular` otherwise. A regular file's
decoded content is at most `hub.lfs_threshold`; a commit's NDJSON body is
at most `hub.max_commit_bytes` (default `64MiB`) and 25,000 operations.
The M2 benchmark may revise the default, with the number. Answering
`"uploadMode": "lfs"` is the server's only lever; `hf_xet` then takes the
Xet path (S3). The compatibility test for this (competitive-matrix lesson
1) uploads a file of at least 1 GiB whose name matches no `.gitattributes`
pattern, so it proves OpenXet routes by size, not by pattern.

**Explicitly not implemented:** model cards, search, likes, discussions,
Spaces, inference, collections, org settings. Unimplemented endpoints
return a clear `501` that names the endpoint, never a confusing `404`. The
exception is a route where stock clients fall back only on `404`, such as
`/v1/xorb-grants/*` (§3).

### 6.1 Git LFS endpoint for `git-xet` *(decided by the user: core, v1)*

The point is Xet dedup for any Git repo, wherever it is hosted. A repo on
GitHub, GitLab, Gitea or Bitbucket sets
`git config lfs.url https://openxet.example/{namespace}/{repo}.git/info/lfs`
(or commits it in `.lfsconfig`), and `git push` / `git pull` move large
files through OpenXet with chunk-level dedup.

How it works (verified in `xet-core/git_xet` source, 2026-10-23):
- `git-lfs` sends the Git LFS Batch API request with its registered transfer
  agents.
- OpenXet answers uploads with transfer `xet`. Each object's `action`
  carries `href` (the token refresh route) and headers `X-Xet-Cas-Url`,
  `X-Xet-Access-Token`, `X-Xet-Token-Expiration` and optionally
  `X-Xet-Session-Id`. huggingface.co does exactly this, and its `href` is
  the Hub token route `…/xet-write-token/{url-encoded ref}` (S7, observed).
- `git-lfs` hands off to `git-xet`, which talks to the CAS directly.

Server work is small because it reuses pieces v1 needs anyway:
- the Batch API endpoint
- the sha256 → Xet file hash mapping (LFS pointers are sha256 oids)
- the token route

Rules for this endpoint:
- Auth uses Git's normal credential flow: an OpenXet access token as the
  HTTP password.
- LFS repos are scopes like Hub repos. Creating one is an authorizer
  `create` decision, so the first push can create it.
- **Downloads always answer `basic`.** `git-xet` 0.2.1 implements uploads
  only ("custom transfer for download is not implemented yet"). But
  `git xet install` registers it for both directions, so `git-lfs` offers
  `xet` in download batches. A server that chooses `xet` there breaks
  `git lfs pull` with no fallback (S7, observed). OpenXet therefore answers
  every download with `basic`, as huggingface.co does:
  - absolute `href`s, never relative, served by the HTTP read gateway
    (§5.5)
  - only `X-Xet-Hash` in the action's `header`, which is not a secret. Xet
    download agents get the CAS token from `GET {lfs-url}/xet-read-token`,
    so it never appears in batch responses or `GIT_TRACE` output.
  - No action `header` or response ever carries the client's own
    credential (§5.9 req 6).
  ([ADR 0003](../../adr/0003-git-xet-credential-strategy.md))
- **Closing the Git download gap** *(decided by the user, v1)*. With plain
  `basic`, `git clone` / `git pull` fetch whole files. Two complementary
  fixes:
  1. **Our client is a Git LFS transfer agent for downloads** (§10.8).
     The `openxet setup git` command registers it under its own transfer name (not
     `xet`, which `git-xet` owns and can't download with), so Git fetches
     only the chunks missing from the local Xet cache. The cache is shared with `hf` and `hf_xet`
     (same `HF_XET_CACHE`). Uploads keep going through `git-xet` when it is
     installed, or through our agent when it isn't.
  2. **Upstream:** contribute download support to `git-xet` in
     `huggingface/xet-core`, so every Git user benefits, not just ours. The
     credential-sourcing change (§10.2) goes in the same effort.
- Clients **without** `git-xet`:
  - **Downloads work** the same way. Collaborators who only clone don't need
    anything extra.
  - **Uploads work** via `basic`/`multipart`, with server-side chunking
    (§10.3, M4), so the data is still deduplicated in storage. The response
    suggests installing `git-xet` for faster uploads.
  - Until server-side chunking ships *(ruling M2-R10)*, an upload batch
    that doesn't offer `xet` answers each object with an LFS object
    error `{"code": 501, "message": "…"}` whose message names the
    `git-xet` install command, never a `basic` `href` that would fail
    later.
- There is no `git-xet` download handshake to match. `git-xet` 0.2.1
  doesn't download, and huggingface.co's `basic` download actions carry no
  `X-Xet-Hash` (S7, `docs/research/S7-git-lfs-xet.md`). The handshake for
  our agent and an upstream `git-xet` is ours to define: the hash in the
  `basic` action's `header`, per ADR 0003.

**Xet-native clients** (chunk first, oids later) use the LFS-scoped token
routes `GET {lfs-url}/xet-read-token` and `GET {lfs-url}/xet-write-token`.
They return the Hub token shape `{accessToken, exp, casUrl}`, and tokens are
scoped to (scope, read|write), never to objects. A shard upload registers
each file's xet hash and its sha256 to the scope, and a read token allows
reconstruction of any file registered to it. Pointers are always the
canonical 3-key form. The full contract is
[ADR 0005](../../adr/0005-xet-native-client-contract.md).

## 7. Operability and user experience

- **Zero-config dev mode:** `openxet serve` runs with SQLite and a local
  directory, prints a static token and the exact `export HF_ENDPOINT=… HF_TOKEN=…`
  lines, and works immediately.
- **Config:** one TOML file, with every key overridable as `OPENXET_*`.
  The `openxet config check` command validates and explains.
- **Errors are actionable:** they say what happened and what to do next.
- **Observability:**
  - Prometheus `/metrics` and OpenTelemetry traces.
  - Structured logs. Tokens are never logged.
  - `/healthz`, `/readyz`.
- **Admin CLI:** `reindex`, `gc [--dry-run]`, `verify` (integrity scan),
  `token mint`, `migrate`, `purge <file-hash>` (§5.4, M2+).
- **Feedback loop:**
  - GitHub Discussions, issue templates and a public roadmap.
  - A docs feedback link.
  - **No telemetry**, by default or ever without explicit opt-in. Client
    telemetry sent to `POST /v1/telemetry` is accepted and dropped unread
    (§3).

### 7.1 Humans and agents as first-class users *(decided by the user)*

OpenXet is built for people **and** AI agents, and for the two working
together. Each gets the best possible experience, and neither is a
degraded mode of the other.

**For humans:**
- Readable output, progress bars and helpful prompts. Safe defaults with
  confirmation before destructive actions.
- The web UI, and docs that teach.

**For agents:**
- **Every surface is machine-usable:**
  - Every CLI command has `--json` output with a documented, versioned
    schema.
  - Exit codes are stable and documented.
  - `--yes` / non-interactive mode never blocks on a prompt.
  - Admin, Hub and CAS APIs are described by a published OpenAPI
    document that is generated from code and checked in CI.
- **Errors are structured:** a stable code, a message, and a `hint` with
  the next action, in both CLI JSON and HTTP responses. An agent can recover
  without parsing prose.
- **Predictable and safe to retry:**
  - Commands and API writes are idempotent where possible, with
    idempotency keys where not.
  - `--dry-run` on everything that changes state.
  - Explicit, deterministic pagination.
- **An MCP server** (`openxet mcp`, in the client) exposes repos, files,
  upload, download, search over paths, usage and admin operations as tools
  with tight schemas. It respects the caller's token scope.
- **Agent-readable docs:** `llms.txt` and per-page plain Markdown on the docs
  site. Every CLI command's help matches the docs, and both are generated
  from one source.

**For humans and agents together:**
- **Delegated, attenuated tokens.** A human mints a token for an agent that
  is narrower than their own:
  - specific repos
  - read-only or write
  - expiry measured in hours
  - optional limits on bytes or operations

  It is revocable from the web UI.
- **Every action is attributed** in the audit log as human, or as agent on
  behalf of a human. The web UI shows what an agent did.
- **Approval gates:** operators can require human approval for
  agent-initiated destructive or costly actions (deleting a repo or branch,
  force-moving refs, GC runs, large egress). The agent gets a pending state
  it can poll; the human approves in the UI or CLI.
- **Handoff:** any CLI or UI action can produce the equivalent command or
  API call, so a human can hand a task to an agent and back.

**Quality bar:** the compatibility suite has an "agent leg." It runs scripted,
non-interactive workflows using only `--json`, exit codes and the MCP
server, and fails on any prompt, unparseable output or unstable schema.

## 8. Engineering practice

**Language and tooling:**
- Rust stable with a stated MSRV; tokio and axum, or equivalents chosen in
  planning with reasons.
- `#![forbid(unsafe_code)]` unless an ADR justifies an exception.
- rustfmt, clippy with warnings denied in CI, `cargo-deny` for licenses and
  advisories, `cargo-nextest`.

**Tests**, each layer earning its place:
- **Unit and property tests** (proptest) on hashing, hash-string conversion,
  xorb, shard and term math.
- **Golden vectors** from `xet-team/xet-spec-reference-files`, plus vectors
  generated by `xet-core`.
- **Fuzzing** (`cargo-fuzz`) of every parser that reads untrusted bytes
  (xorb, shard, NDJSON commit). The xorb and shard targets
  (`read_upload_shard` and the xorb parsers) are an M1 task.
- **Compatibility suite**, the most important test. CI runs real `hf_xet`,
  `huggingface_hub`/`hf` CLI, `huggingface.js` and `git` + `git-lfs` +
  `git-xet` with a Gitea remote (pinned and latest) against
  OpenXet, across a matrix:
  - SQLite with local disk
  - Postgres with MinIO

  It covers upload, re-upload dedup, range download, branch workflows and
  token refresh. A "third-party Xet client" leg replays the expectations of
  jj-xet's `tests/openxet.rs` mock against OpenXet (ADR 0005). A nightly job
  against latest upstream catches protocol drift early.
- **Fault injection:** object store errors, DB failover, killed nodes
  mid-upload.
- **Benchmarks** with regression thresholds in CI; published numbers in
  docs.

**Decisions and history:**
- Architecture Decision Records live in `docs/adr/`.
- Conventional Commits with a scope per component, so `git log` can be read
  per work package.
- Before changing a component, read its ADRs and `git log -- <path>` (this
  rule goes into `AGENTS.md`).

**`AGENTS.md`**, written first and kept current:
- architecture map
- exact commands
- invariants (protocol exactness, bucket is truth, no user ownership, never
  delete outside GC)
- definition of done (tests, docs, ADR if a decision changed)
- history-check rule
- anti-slop rules: no speculative abstraction, no narrating comments, no dead
  code, no unverified claims in docs
- how to verify against upstream

**Docs** live in the repo and are built into a docs site. Examples are run in
CI so they cannot rot. Docs cover: quickstart, operator guide per backend,
auth recipes (Keycloak, Auth0, GitHub OIDC, webhook), storage-only
integration guide for platforms, upgrade notes, and the compatibility
matrix.

**Release:** SemVer and a changelog. The protocol compatibility statement
says which spec version and client versions are supported.

## 9. Distribution

- **v1:**
  - static binaries for linux/macOS on amd64 and arm64, via GitHub Releases
    (cargo-dist or equivalent), a Homebrew tap and `cargo install`
  - multi-arch container images on GHCR
  - a Helm chart
  - `docker compose` quickstart with MinIO. Per the S2 outcomes
    (`docs/research/S1-S2-direct-downloads.md`):
    - `http://` on loopback by default
    - TLS docs per platform: on macOS `hf_xet` ignores `SSL_CERT_FILE`, so
      a private CA must be trusted in the system store
    - OpenXet presigns with a store endpoint the client can reach, not the
      compose-internal service name
  - Terraform examples for AWS, GCP and Azure
  - library crates on crates.io for embedding the storage core
  - **The `git` role needs `git` ≥ 2.37 on `PATH`** (ADR 0004).
    - OpenXet runs git as a separate program. That is aggregation, so
      OpenXet stays Apache-2.0 (inferred, not legal advice).
    - Container images ship the distro's unmodified git package, which is
      GPL-2.0, and list it in their licence notice.
    - Each image release publishes the exact git source package next to
      the image, or includes a written offer for it (GPL-2.0 §3).
    - The static binaries don't bundle git. They check for it at start-up
      when the `git` or `ssh` role is enabled, and fail with an actionable
      error.
- Also v1:
  - one-click templates (Railway, Fly, Render)
  - a Nix package
  - cloud marketplace listings (AWS, GCP, Azure). These need publisher
    accounts, which is a non-code task for the user.
  - A Kubernetes operator, for multi-role deployments, key rotation and GC
    scheduling. It is built only if Helm proves insufficient for these; the
    plan states which way it went and why.
- **Discoverability:**
  - listing in awesome-selfhosted
  - a "self-host Xet" guide that ranks for the obvious queries
  - published benchmarks
  - a clear "works with the official HF tools" message

## 10. Further v1 scope *(decided by the user: ambitious, nothing deferred)*

### 10.1 Git over SSH

- OpenXet runs an SSH endpoint, enabled by the `ssh` role. Identities are SSH
  public keys bound to an external identity, the same way access tokens are
  (§5.4). Keys are registered via `openxet login` or the admin API. No
  passwords and no users are stored.
- **Post-quantum key exchange first:** `mlkem768x25519-sha256`, the
  default since OpenSSH 10.0, then `curve25519-sha256` (ADR 0011).
- **`git-lfs-authenticate`:** returns `{href, header.Authorization,
  expires_in}`, all three always. The LFS spec makes them optional, but
  `git-xet` 0.2.1 fails without `href` or `expires_in` (S7: Gitea omits
  `expires_in` and `git-xet` breaks against it). `git-lfs` uses it when the
  LFS endpoint is SSH. `git-xet` uses it when the Git remote is SSH, on
  every token refresh.
- **`git-lfs-transfer`** (the pure-SSH LFS protocol, git-lfs ≥ 3.0): lets
  plain `git-lfs` upload and download over SSH with no HTTPS at all. Uploads
  go through server-side chunking (§10.3).
  - It can't negotiate `xet`: the client always sends `transfer=ssh` and
    uses its built-in `ssh` adapter (S7, source and observed).
  - `git-lfs` prefers it whenever the LFS endpoint is SSH. So for
    OpenXet-hosted repos over SSH, `openxet setup git` sets
    `lfs.<url>.sshtransfer=never` when `git-xet` is installed, which sends
    those users through `git-lfs-authenticate` plus the HTTPS batch, where
    `xet` is offered (ADR 0003).
- Git repos whose *remote* is on another host over SSH (e.g.
  `git@github.com:…`) work as long as `lfs.url` points at OpenXet and
  `openxet setup git` has written the `access` key (§10.2).

### 10.2 `git-xet` with third-party Git hosts

When `lfs.url` points at OpenXet and the remote lives elsewhere, OpenXet's
policy is: **capability routes never authenticate by header, and `openxet
setup git` configures access explicitly**
([ADR 0003](../../adr/0003-git-xet-credential-strategy.md), S7). All three
parts are v1:
- **Server side:**
  - The `href` refresh route OpenXet returns is a short-lived, signed,
    single-repo capability URL, so the refresh needs no credential.
  - CAS tokens issued through the LFS path last for the transfer session.
  - OpenXet never authenticates by, logs or echoes an `Authorization`
    header on that route.
- **Client setup:** `openxet setup git` writes the clone's git config:
  - `lfs.url`
  - `lfs.<endpoint derived from the remote>.access=none`, so `git-xet`
    calls the capability `href` without a credential; the `href` alone
    completes the refresh (S7, observed)
  - the OpenXet token in the credential helper under the OpenXet host, for
    `git-lfs`'s batch

  The `access` key must be in git config, because `git-xet` doesn't read
  `.lfsconfig`.
- **Upstream:** contribute a lazy credential lookup, an optional
  `expires_in` and download support to `git-xet` in `huggingface/xet-core`,
  coordinated with Hugging Face.

### 10.3 Server-side chunking for non-Xet clients

Plain `git-lfs` (basic transfer, HTTPS or SSH), older `huggingface_hub`
without `hf_xet`, and any LFS client can **upload**. OpenXet chunks,
deduplicates and writes xorbs and shards on ingest, using the same `xet-core`
chunking, so the hashes are identical to client-side uploads.
- Users get dedup in storage even if their client never heard of Xet. They
  don't get the bandwidth savings; the docs say so.
- Ingest scales with the `cas` role. CPU cost is benchmarked.
- `preupload` and the LFS batch endpoint offer `basic` and `multipart`
  alongside `xet`.

### 10.4 Repositories and buckets

The two storage primitives of §1
([ADR 0010](../../adr/0010-two-storage-primitives.md)).

**Labels**, on repositories and buckets alike, are free-form and
owner-defined: none, one or several, such as `model`, `dataset`,
`checkpoint` or `eval-logs`. A label is a lowercase slug (`a` to `z`,
`0` to `9` and `-`) of at most 64 bytes, at most 32 per object. Labels are
added and removed through OpenXet's API and CLI, each change with an
audit entry, and reach the object store before the change is
acknowledged, so `reindex` restores them. They never change the address
or any behaviour: a `space` label runs nothing. The Hub layer reads and
writes the four Hugging Face types as labels (§6); stock
`huggingface_hub` can't commit to a `kernel`, so kernels are written over
Git.

**Buckets** (mutable, non-versioned) serve the HF Storage Buckets API
(§3). Keys map to Xet files with last-writer-wins and optional object
versioning. Bucket tokens are not revision-scoped, matching
`huggingface_hub`. An overwritten or deleted object only loses its
reference; its bytes leave through GC (§5.6).

**Writes reach the object store before the acknowledgement.**
`huggingface_hub` 2.2.0 uploads all of a call's files in one Xet session,
then sends their keys in one NDJSON `POST /api/buckets/{id}/batch`
(`hf_api.py`, `batch_bucket_files`). Each such write request is one
entry in the bucket's log:
1. The node checks the request: the credential (§5.9 req 2) and every
   hash it names (`addFile` and `copyFile`, §5.4).
2. An index transaction allocates the request's `seq`, the bucket's next
   sequence number, and records it as pending.
3. The node creates `bucket-log/{sha256(scope)}/{seq}` in the object
   store, only if absent, holding the request's key operations. If the
   record already exists, another node holds that number (after an index
   restore, for example), so the node allocates a new one and tries
   again. Two nodes can never both win a number.
4. An index transaction applies the operations. A key takes an operation
   only if its `seq` is higher than the key's current one, and a deleted
   key keeps a tombstone with its `seq`, so the index and `reindex` reach
   the same last writer whatever order records are replayed in.
5. The write is acknowledged.

Retries stay idempotent:
- Applying a record again changes nothing, because its `seq` is not
  higher than the ones it already set.
- A node that crashes after step 3 leaves a pending `seq` with a durable
  record, and any node finishes it by applying the record. A write can
  thus become visible without its acknowledgement, never acknowledged
  without being durable.
- A sweeper aborts a `seq` still pending past the request deadline by
  creating that same key, `bucket-log/{sha256(scope)}/{seq}`, only if
  absent, holding an abort record. Writer and sweeper race on one
  create-only key, so exactly one wins. A stalled writer whose create
  loses reports the write as failed, so the client's failure matches
  the log, and the client's retry gets a new `seq`.

- A client that retries a whole request writes a new record with the
  same operations, which leaves the keys as one write would.

The invariant: a write takes effect if and only if its record, not an
abort, owns its `seq`, and it is acknowledged only then.

The record is written synchronously. Spike S12 suggested an asynchronous
ordered log with up to 60 s of staleness, like repository refs (§5.3).
Buckets can't take that: a repository's commit objects reach the object
store before its acknowledgement, so a reindex can place them again, but
a bucket's key mapping lives nowhere else. An acknowledged key lost with
the database would leave its data unaddressable (non-negotiable 2, §5.9
req 7).

Periodically a node writes `bucket-snapshot/{sha256(scope)}/{seq}`, only
at a watermark: the highest `seq` W such that every `seq` up to and
including W is applied or aborted, so a snapshot covers exactly a complete prefix of the log.
`reindex` replays from the latest snapshot, skips abort records, and
sets the bucket's next `seq` above the highest record it found, aborts
included. Its work is bounded by the snapshot interval, not by the
bucket's history. Records a snapshot covers leave through GC like any
other object.

Cost: one extra object-store PUT per request, next to the xorb, shard and
registration writes the upload already makes, so small objects stay
fast. M5 measures it (§12).

Where the bucket primitive and an object-store bucket could be confused,
say "object-store bucket" or "bucket primitive".

An S3-compatible API over buckets is under evaluation in
[spike S12](../../research/S12-s3-api-and-dedup-economics.md), pending the owner's decision; it is not scheduled.

### 10.5 Cache tier

- The `transfer` role can run as a **regional read-through cache**: local
  NVMe, LRU over xorb ranges, and signed-URL compatible.
- Reconstruction responses point clients at the nearest cache, configured by
  region or header.

### 10.6 Pull-through mirror of huggingface.co

- Self-hosted deployments can mirror public, ungated HF repos. Gated and
  private repos are served only to callers whose own HF credential passes
  an upstream check. The first download fetches from HF; later ones are
  served locally.
- Xet data is stored natively, so mirrored content dedups with local data.
- Air-gapped teams can pre-seed a mirror and carry it across.

*Decided (R19); evidence in `docs/research/S8-hf-mirror.md`; ingest
mechanics to be recorded in ADR 0006:*
- **Ingest verified chunks; never relay HF's xorbs or URLs.**
  huggingface.co signs only the ranges a file needs, and files often use
  part of a shared xorb. Its xorb URLs expire 3600 s after issue, are
  range-contained, and carry `xip` and, for private repos, the token
  owner's `user_id`. Read tokens live 900 s and are scoped per repo
  (observed). So the mirror:
  - fetches term ranges promptly with a per-repo upstream token;
  - decompresses and hashes every chunk;
  - checks the file hash against `X-Xet-Hash` and sha256 against
    `X-Linked-ETag`;
  - re-packs the verified chunks into local xorbs with a shard registered
    to the mirrored repo's scope.

  Re-packing keeps the file hash, which depends only on chunk hashes and
  sizes. Clients only ever see OpenXet URLs and tokens.
- **No unverified chunks.** Until the window-merge proof test passes, a
  file's shard is registered only after the whole-file `X-Xet-Hash`
  check. Chunks fetched earlier stay staged and unreferenced.
- **Ingest goes through normal acceptance.** Re-packed xorbs and the
  generated shard go through the normal upload acceptance:
  - xorb hash recomputation;
  - the `verify_shard` S6 rules, including verification entries;
  - the 64 MiB body limit;
  - the `MAX_REFERENCED_CHUNKS` cap, splitting shards as needed.

  Ingest has no trusted bypass.
- **Licence files are kept.** The mirror serves every file of a mirrored
  revision, including `LICENSE`, `NOTICE` and the README, and never
  filters them out of `tree` or `resolve`.
- **Authorise per caller.**
  - Public, ungated repos: the OpenXet authorizer.
  - Gated and private repos: also an upstream check with the caller's own
    HF credential, failing closed. That credential (for example
    `X-HF-Upstream-Token`):
    - is never persisted or logged;
    - is used only for that caller's check.

    The check cache is keyed by (caller, repo, revision) and holds only
    yes or no, with a TTL.

  Gated and private content is never fetched for or served to one caller
  using another caller's or the operator's HF credential. There is no
  override (R19).
- **Freshness and removal.**
  - Refs are re-resolved upstream on a short TTL; data keyed by commit is
    immutable.
  - While upstream is down, stale serving covers only content whose last
    successful upstream check was public and ungated. A later gated,
    private or 404 answer hides it.
  - Repos HF removes are hidden, and their data is reclaimed only by GC.
- **Mirrored namespaces are read-only.**
- **Colliding names.** OpenXet names are unique across labels (§6), so a
  mirror or import that meets two Hub repositories of different types
  under one name maps the second to `{name}-{type}` and reports it.
- **Seeding.** `openxet mirror pull` pre-seeds through the same ingest
  path. Mirrored commit metadata lives in the bucket, so moving bucket
  objects plus `reindex` carries a mirror across an air gap.
- **The mirror is disabled on openxet.com**, for every tenant, including
  their own private repos, until counsel review and ideally a written
  agreement with HF. HF's licence to users covers "through our
  Services", per-model licences put duties on distributors, and the
  Content Policy names proxies.

### 10.7 Multiple storage pools and data residency

- One deployment can hold several storage pools (buckets or regions). Pools
  are assigned per tenant or per repo by policy.
- Dedup is per pool; data never crosses pools.
- Optional per-pool encryption keys via the cloud KMS (SSE-KMS or
  equivalent).
- GC, `reindex` and metering are pool-aware.

### 10.8 Our own client *(decided by the user: v1, 100% compatible)*

OpenXet ships a client: a CLI and a Rust library, with Python bindings.
It is better to use than the stock tools and never incompatible with them.

**The compatibility contract is non-negotiable:**
- Byte-identical chunking, hashes, xorbs and shards. The client builds on
  `xet-core`'s `xet-data` / `xet-core-structures` rather than reimplementing
  them. Any divergence is a release-blocking bug.
- It works against **huggingface.co itself** as well as OpenXet, via the same
  Hub and CAS APIs. Data uploaded by our client is readable by `hf`,
  `hf_xet`, `git-xet` and `huggingface.js`, and vice versa.
- It reads and writes the standard HF cache layout (`HF_HOME`, `HF_XET_CACHE`)
  and honours `HF_ENDPOINT` / `HF_TOKEN`, so it coexists with `hf` on the
  same machine.
- CI proves it in both directions: our client against OpenXet and against
  huggingface.co (nightly, using a dedicated test org), plus cross-tool round
  trips (upload with ours, download with `hf`, and the reverse).

**What makes it worth installing** (the extra mile for users):
- **Resource URIs:** `xet://` is the chosen future resource scheme
  ([ADR 0009](../../adr/0009-xet-resource-uris.md)). The authority names the
  actual deployment host and optional port, equally for hosted and self-hosted
  servers. HTTPS remains the canonical browser/share link. M2 specifies and
  tests the shared parser; M6 integrates CLI/SDKs and adapters. Full grammar
  and legacy compatibility are still to be specified, not current support.
- **`sync`:** rsync-style, two-way sync of a directory with a repo or
  bucket. Only changed chunks move.
- **`mount`:** read-only FUSE (Linux) / FSKit or macFUSE (macOS) mount of any
  repo revision, with lazy chunk fetch. Train directly off a mounted dataset.
- **Storage-only mode:** upload and download by Xet file hash with no Hub
  layer, for platforms.
- **Resumable everything:** interrupted uploads and downloads resume, and
  progress output is human and `--json`.
- **Chunk-cache seeding:** `openxet` can seed the local Xet chunk cache
  from an existing Git LFS checkout, so the first `git pull` or download
  after switching to OpenXet fetches only changed chunks (§1a).
- **`login`:** the OIDC device/browser flow that issues OpenXet access tokens
  and SSH key registration (§5.4, §10.1).
- **`setup git`:** one command configures `lfs.url`, credentials and
  `git-xet` for a repo (§10.2). It also registers `openxet` as the Git LFS
  transfer agent, so `git clone` / `git pull` download only missing chunks
  (§6.1). It works against OpenXet and huggingface.co alike.
- **Diagnostics:** `doctor` checks connectivity, auth, clock skew and
  versions, then says what to fix.

**Distribution:** the same channels as the server (§9), plus PyPI (Python
bindings), npm via WASM if the bindings allow it, `winget`, and Windows
binaries. The client must run on Windows; the server need not.

### 10.9 Git hosting *(decided by the user: v1)*

`git clone https://openxet.example/org/model` (and `git@openxet.example:org/model`)
works as it does on huggingface.co. The prefixed URLs
(`https://openxet.example/datasets/org/data`) are typed views of the same
repository (§6). This sits alongside §6.1, where the repo
lives elsewhere and OpenXet is only its LFS store; both modes are supported.

**Behaviour:**
- **Protocols:** Git smart HTTP and SSH, both `upload-pack` and
  `receive-pack`, protocol v2 where clients support it. The LFS endpoint is
  implicit at `{repo}.git/info/lfs`, so `git-xet` and `git-lfs` work with no
  configuration. §10.2 doesn't apply, because the Git remote *is*
  OpenXet.
- **Large files land in Xet automatically:**
  - Pushes with `git-xet` take the Xet path.
  - Plain `git-lfs` pushes go through server-side chunking (§10.3).
  - Large blobs pushed **without** LFS are kept as ordinary Git objects. They
    don't silently corrupt history. OpenXet reports them and offers
    `openxet migrate-lfs`. Optionally, the operator can reject them above a
    size limit, with a clear message.
- **One history:** Hub API commits and Git pushes write into the same Git
  history under the same ref compare-and-swap rules.
- **Stateless nodes still hold:** Git objects live in the bucket (packfiles
  plus index) and refs live in the database, with the §5.3 durability rules.
  Nodes may keep a local read cache of packs.
- **Hooks for integrators:** post-receive webhooks (repo, ref, old/new sha)
  so platforms can react to pushes, e.g. to trigger CI.

**Engine** *(decided by S9, ADR 0004)*:
- **The `git` binary serves the wire protocol.**
  - Each request runs `git upload-pack` / `receive-pack` on a throwaway
    snapshot repo, built from one database snapshot:
    - `packed-refs`
    - hard links to exactly the committed packs in the node's cache,
      with no alternates
  - The snapshot lives on the cache's filesystem, which start-up checks.
  - OpenXet answers the protocol v2 capability advertisement and `ls-refs`
    in-process.
  - OpenXet enforces reachability for every upload-pack want, v0 to v2
    (R22), because git's v2 `upload-pack` serves any object in the odb.
    It interposes on the request stream and checks wants before
    forwarding them to git.
    - Ref tips pass.
    - All other wants share one gix walk per request, within a configured
      commit, time and concurrency budget.
    - Anything not found is refused (fail closed).
  - Objects are never shared across repos.
  - Ref updates go through a hardened `proc-receive` hook back into
    OpenXet:
    - an inherited socketpair fd (HTTP and SSH) and a single-use token
      bound to repo, principal and request
    - pack paths confined to the request's snapshot
    - policy re-checked in OpenXet
    - a sanitised git environment
  - OpenXet then publishes the pushed pack to the bucket as `pending`.
    Next it runs one transaction that does four things:
    - the ref CAS
    - marks the pack `committed`, only while it is still `pending`
    - checks `full_repack_gen`
    - inserts the database `ref_log` rows

    Losing pushes mark their pack `lost` (again only while it is
    `pending`). Then it acknowledges.
  - The `ref_log` table is written by every ref writer, Hub API
    included, in its CAS transaction. The bucket ref log (§5.3) is
    derived from it.
  - Every ref writer uses that same transaction, `full_repack_gen` check
    included: pushes, Hub ref creates, updates and deletes, and Hub
    commits. Any failure, the fence included, leaves the writer's pack
    `lost`.
  - A writer may only make reachable what was already reachable from its
    snapshot's refs, or what the client itself sent. Bases that git's
    `--fix-thin` appends don't count as sent, so every appended base must
    be reachable from the snapshot's refs. Path-guided lookups in the
    trees of boundary commits and ref tips settle most pushes; the rest
    take a full walk under its own budget, failing closed. That stops a
    push or a Hub `create_branch` from resurrecting, or copying out, a
    force-pushed-away object. Until the `git-reachability` bitmap
    follow-up, some honest thin pushes can be refused (ADR 0004 item 3);
    the refusal hints `git -c pack.window=0 push`, which sends no thin
    bases (over smart HTTP git ignores `--no-thin`).
  - The v2 interposer allows only `ls-refs` and `fetch`. git 2.37 to 2.44
    serves `object-info` for any object, so OpenXet refuses it, along
    with `bundle-uri` and unknown commands.
  - Each request reads refs and its pack set in one database snapshot,
    from the primary.
    - Only `committed` packs are exposed to new snapshots.
    - `pending`, `lost`, `superseded` and `recovered` packs never are,
      so a push's connectivity check can't lean on another in-flight
      pack or on a repack input.
    - In-flight requests keep their links.
- **Repacking** takes committed packs only, on one leaseholder per repo:
  - geometric, with a multi-pack-index bitmap, which leaves cruft packs
    alone
  - plus a periodic full `repack -adb --cruft --cruft-expiration=<grace>`
  - Temporary keep refs hold every `ref_log` tip from the grace window.
    Cruft expiry goes by object mtime, so hydrated packs keep their
    creation mtime, the leaseholder re-pins mtimes before each repack,
    and `.mtimes` files travel through the bucket.
  - A full-repack publish increments `full_repack_gen`, which fails any
    push that read the old generation. The publish aborts if `ref_log`
    advanced during the repack. Geometric repacks drop nothing and need
    no fence.
  - The pack lifecycle is not prototyped yet. M2 prototypes it, with
    property-based and fault-injection tests, before production use
    (ADR 0004).
  - A repack publish checks the lease epoch, so a stale leaseholder
    can't publish.
  - Superseded, lost and expired pending packs go to GC (§5.6), with
    their side files; superseded `multi-pack-index` generations go too.
    `recovered` packs never do. Commits and annotated tags that reindex
    could not place stay protected as keep roots, without blocking
    repacks, until an operator restores or discards them.
- **gitoxide stays in-process off the wire path,** for Hub API object
  reads, the reachability check and Hub commit writes. It only ever
  opens a request snapshot, never the node cache.
- **git ≥ 2.37 is a runtime dependency** (§9). Container images ship it
  from the distro (GPL-2.0, run as a separate program).
- **Measured** (`docs/research/S9-git-engine.md`):
  - A full clone of a 10k-commit repo cost git 0.18 s CPU, against 2.86 s
    for an in-process gix server. gix 0.90 has no server side, no delta
    compression and no bitmaps.
  - Concurrent pushes across two nodes produced exactly one winner in
    60/60 rounds.

This is the hardest new component; budget for it accordingly.

### 10.10 Minimal self-service web UI *(decided by the user: v1)*

An optional `web` role. Small, fast and accessible, served by the same
binary.

**What it does:**
- Sign in through the operator's identity system (OIDC). No local accounts.
- Create, list and revoke access tokens; add and remove SSH keys.
- Browse repositories, branches, commits and files, and buckets and their
  keys; download files; copy clone,
  `hf` and `openxet` commands.
- See storage usage per repository, bucket and tenant: logical bytes, stored bytes, and the
  dedup savings ratio.
- Admin view (for operator-designated groups): health, GC status and dry-run
  reports, pools, mirror status.

**Explicitly not included:** model cards, README rendering beyond plain
text, discussions, social features, previews of model or dataset contents.

Easy to restyle and disable; platforms like aqora will typically run their
own UI and leave this role off.

## 11. Spikes (verify, don't assume)

S1 to S11 are answered: S1 to S9 in M0, S10 and S11 for M1. S12 and S13 are
open: their measurements are a record, and their recommendations await
the owner's decision. Each note records versions, method and what was
observed versus inferred.

- **S1** *(answered: yes, on MinIO;
  [S1 and S2](../../research/S1-S2-direct-downloads.md), ADR 0002)*: `hf_xet`
  downloads correctly when v2 reconstruction returns native S3 presigned
  URLs with one range per fetch entry.
- **S2** *(answered: `http://` works for Hub, CAS and store; a private CA
  works via `SSL_CERT_FILE` on Linux but not for `hf_xet` on macOS;
  [S1 and S2](../../research/S1-S2-direct-downloads.md))*: `hf_xet` and
  `huggingface_hub` accept `http://` CAS and Hub endpoints for local dev, or
  what TLS setup the quickstart needs.
- **S3** *(answered: 99 exchanges captured as fixtures in
  `compat/fixtures/hub/`; corrections applied to §3 and §6: `DELETE` for
  repo delete, `validate-yaml` on the upload path, tokens in body and
  headers; [S3](../../research/S3-hub-api-shapes.md))*: the exact
  `whoami-v2`, `preupload`, `commit` and `tree` response shapes
  `huggingface_hub` requires, captured as fixtures.
- **S4** *(answered: no for DuckDB and polars `hf://`, yes for fsspec
  `HfFileSystem`; plain HTTPS `resolve` URLs work for all;
  [S4](../../research/S4-analytics-clients.md))*: whether DuckDB `hf://`
  (and polars or fsspec) can target a custom endpoint; if not, document
  the plain-HTTPS URL route.
- **S5** *(answered: yes; everything the server needs is public in
  `xet-core-structures` 1.7.0, at the cost of a heavy dependency graph,
  sync-only parsers and API churn within 1.x, contained by an exact pin
  behind `openxet-protocol`;
  [S5](../../research/S5-xet-core-structures-server-side.md), ADR 0001)*:
  whether `xet-core-structures` is usable server-side, or which pieces to
  vendor or reimplement, and its versioning on crates.io.
- **S6** *(answered: twelve rules, all checkable from per-xorb chunk hashes
  and lengths stored at xorb ingest, with work caps per shard; reference
  and real `hf_xet` shards pass; what stock clients actually send is in
  §3; [S6](../../research/S6-shard-verification.md))*: the shard upload
  verification rules (spec "shard" page) implemented against the
  reference files.
- **S7** *(answered: `git-xet` 0.2.1 has no download handshake, and
  downloads must answer `basic`; with `lfs.url` on another host, a
  signed `href` plus `access=none` makes the refresh work over HTTPS and
  SSH (§10.2);
  `git-lfs-authenticate` must include `href` and `expires_in`;
  `git-lfs-transfer` can't carry `xet`;
  [S7](../../research/S7-git-lfs-xet.md), ADR 0003)*: the `git-xet`
  download handshake, plus `git-lfs` behaviour with a separate `lfs.url`
  host (credentials, `basic` fallback), against a real Gitea in CI (the CI
  leg is specified in ADR 0003). Includes credential sourcing (§10.2)
  over both HTTPS and SSH, and whether `git-lfs-transfer` (pure SSH) can
  negotiate the `xet` custom transfer.
- **S8** *(answered: the Hub read surface plus CAS reconstruction, with
  every URL and token re-issued by OpenXet; HF reconstructions can be
  fetched with per-repo 900 s read tokens, but their 3600 s,
  range-contained, `xip`-tagged URLs must not be cached or handed out, so
  the mirror ingests verified chunks into local xorbs; the legal texts
  best support self-hosted mirroring of public repos, while a hosted
  third-party mirror and shared gated access are high-risk and need a
  lawyer; policy decided in R19; [S8](../../research/S8-hf-mirror.md),
  §10.6)*: HF pull-through mirroring. Which HF endpoints, and what Xet
  read-token behaviour, a mirror depends on, and the terms-of-use
  constraints for caching HF content.
- **S9** *(answered: the `git` binary serves the protocol on per-request
  snapshot repos, over OpenXet-owned database refs and bucket packs, with
  gix in-process off the wire path; gix 0.90 has no upload-pack or
  receive-pack server, no delta compression and no bitmaps, and cost 16x
  git's CPU to serve a 10k-commit clone; database CAS gave exactly one
  winner in 60/60 concurrent-push rounds across two nodes; JGit DFS,
  go-git, libgit2, Gitaly and Mononoke were assessed and rejected;
  [S9](../../research/S9-git-engine.md), ADR 0004)*: Git hosting engine:
  gitoxide server-side capabilities (upload-pack/receive-pack, protocol
  v2) against the `git` binary on a cached bare repo. Measure throughput
  and latency, and check that bucket-backed packs keep nodes stateless.
- **S10** *(answered: stock `hf_xet` 1.7.0 uses seven CAS routes in a
  storage-only upload and download; writes are retried whole, `404` on a
  `/v2/` route means fall back, tokens refresh only by clock, data URLs
  carry no credential, and client shard caches live 21 days;
  [S10](../../research/S10-cas-wire-contract.md))*: the exact CAS wire
  contract that stock clients rely on, and the `xet-data` API our own
  client calls (ADR 0006).
- **S11** *(answered: a dedup answer offers every xorb of the newest
  visible shard that references the matched xorb, as `xet-core` and
  huggingface.co do; a 1 MiB edit of a 1 GiB file then sends about
  1.2 MB with an empty cache; ruling M1-R30;
  [S11](../../research/S11-global-dedup-response.md))*: what a global
  dedup answer should offer.
- **S12** *(open: measurements recorded, recommendation pending;
  [S12](../../research/S12-s3-api-and-dedup-economics.md))*: whether an S3-compatible API over buckets is feasible,
  what dedup saves on multi-tenant data, and the cost and billing
  consequences.
- **S13** *(open: draft PR #59, pending the owner's approval of pricing
  and positioning)*: hosted openxet.com's features, pricing and
  first-visit journey (M8).

**Client version floor** *(builder-check call)*: the oldest `huggingface_hub`,
`hf_xet` and `huggingface.js` versions that use the v2 reconstruction and
current token flow. They are stated in the compatibility matrix and tested
as the "oldest" leg of the suite. M0 did not pin them. The M1 plan sets
`hf_xet` 1.7.0 as the floor (S6, S10).

## 12. Milestones (v1 = all of them)

Release cut *(decided by the user)*: build M0 through M7 straight to `1.0`,
with no pause between milestones to wait for outside feedback. Real-world
feedback takes time to arrive, so waiting on it would only stall the work.
Until `1.0`, the compatibility suite, benchmarks and the measured
time-to-first-upload stand in for users. The repo is public while it is
built, so anyone can try it early, but no milestone waits on that. The work
is built by the user with coding agents, so the plan sequences milestones
but sets no calendar dates. M8 is the hosted service on openxet.com, not a
release of the open-source build: it runs the released build (§15).


- **M0:**
  - repo, `AGENTS.md`, CI skeleton, ADR template
  - spikes S1 to S9
- **M1:** CAS core on SQLite with local disk and static tokens.
  - Storage-only mode passes `hf_xet` upload and download, plus the golden
    vectors.
  - The stock-client paths of §3: `/v1/chunks/default/{hash}`,
    `/v1/telemetry` dropped, `/v1/xorb-grants/*` answering `404`.
  - Fuzz targets for the xorb and shard parsers.
  - The M1 rows of the security tests (§5.9).
- **M2:** Hub-compat layer and Git hosting
  ([plan](../plans/2026-10-25-m2-hub-git-lfs.md); rulings M2-R1 to M2-R17
  in §17).
  - `hf auth login`, `hf upload`, `hf download` and listing work unmodified.
  - `git clone` / `git push` over HTTPS against OpenXet-hosted repos, sharing
    one history with Hub API commits (§10.9).
  - The Git LFS endpoint works: `git push` / `git clone` with `git-xet`
    against a Gitea-hosted repo, and `basic` download for clients without
    `git-xet`.
  - The pack-lifecycle prototype and the ADR 0004 test checklist.
  - Generic labels on repositories (§10.4, ADR 0010): add, remove and
    list through OpenXet's API and CLI, with validation and audit
    entries.
  - The typed-route views of §6 for `model`, `dataset`, `space` and
    `kernel`: typed create adds the label, typed listing filters by it,
    a missing label is a `404 RepoNotFound`, and a name taken under
    another label is a `409`. Each type is read with stock
    `huggingface_hub` and written with the stock client that supports
    it: Hub commits for `model`, `dataset` and `space`, Git with
    `git-xet` for `kernel`. The compatibility tests of §6 cover a
    typed-route view, a wrong-type `404` and a create collision.
  - The resolve-bytes subset of the HTTP read gateway (§5.5): the LFS
    `basic` download `href`s point at it (ADR 0003).
  - **M2-URI: resource-address contract** (ADR 0009), after the Hub resource
    naming and resolution contracts. Deliver the reviewed `xet://`
    specification, legacy Git-Xet/pyxet compatibility matrix and shared
    parser/formatter. Acceptance: explicit-host hosted/self-hosted parity,
    golden vectors, round-trip properties, fuzzing, and two-endpoint
    resolution/credential-isolation tests. Full grammar requires its own
    reviewed specification and implementation plan before coding; see the
    [URI milestone handoff](../plans/2026-10-24-resource-uri-milestones.md).
  - `openxet purge <file-hash>` (§5.4).
  - The OpenAPI document for the Hub and CAS routes, generated from code
    and checked in CI (§7.1).
  - One router: the CAS and Hub routes composed at the builder level under
    a single fallback, since axum can't merge two routers that each carry
    one (issue #46).
- **M3:** production backends.
  - Postgres; S3, GCS and Azure; presigned downloads.
  - The range-binding probe against real AWS S3, R2, Garage, GCS and
    Azure, and the presigned-URL signing benchmarks (ADR 0002).
  - OIDC and webhook auth.
  - Delegated, attenuated agent tokens (§7.1): chosen repositories or
    buckets, read or write, an expiry in hours, optional byte and
    operation caps, revocable. Their web UI comes in M6.
  - Per-tenant rate limits and quotas, and per-credential shares of the
    memory budget (M1-R40, §15), verified under abuse load with the load
    harness (issues [#55](https://github.com/stubbi/openxet/issues/55) and [#57](https://github.com/stubbi/openxet/issues/57)).
  - **Load testing** (issue [#55](https://github.com/stubbi/openxet/issues/55)), a deliverable rather than a one-off
    proof:
    - a reproducible harness in `loadtest/`, with stock `hf_xet` where
      client behaviour matters and a native generator for higher rates
    - scenarios: the §5.8 steady mix, a 10× spike, a 24 h soak, a
      thundering herd, many tenants with one noisy, slow clients, and
      dedup-heavy and dedup-free uploads; the reconstruction p99 under
      load (issue [#43](https://github.com/stubbi/openxet/issues/43)) is one of them
    - multi-node: near-linear scaling from 1 to N stateless nodes on the
      production backends, and a node killed mid-run with no
      acknowledged write lost
    - a release gate on the release rig, a nightly short run, and results
      published with machine, method and raw data in the
      [benchmarks](../../reference/benchmarks.md)
  - A migration compatibility policy, so nodes of two adjacent versions
    share one database through a rolling upgrade (issue #47).
  - Index round trips outside the CPU permits, once the index is
    networked (issue #48).
  - Hybrid post-quantum TLS key exchange preferred in every rustls client
    OpenXet builds, with a test of the negotiated group (ADR 0011, issue
    [#66](https://github.com/stubbi/openxet/issues/66)).
  - A key-rotation drill for CAS token keys and the transfer key, under
    load (ADR 0011, issue [#65](https://github.com/stubbi/openxet/issues/65)).
- **M4:** operational completeness.
  - GC, `reindex` from the object-store bucket, `verify`.
  - Readiness that checks the index and the object-store bucket (issue #49).
  - Metrics, tracing, and the rest of the HTTP read gateway.
  - Metering (§5.7) and the admin API that exposes it.
  - The audit log, attributing each action to a human, or to an agent on
    behalf of a human (§7.1); the web UI shows it in M6.
  - Server-side chunking for non-Xet clients.
  - Git over SSH (`git-lfs-authenticate`, `git-lfs-transfer`), with
    post-quantum key exchange first (ADR 0011, issue [#67](https://github.com/stubbi/openxet/issues/67)).
  - The `git-xet` setup of §10.2: the server-side capability route, plus
    the upstream `git-xet` contributions listed there.
  - Upstream: Xet download support in `git-xet` (§6.1). This is contributed
    alongside the other `git-xet` contributions and coordinated with
    Hugging Face before any PR is filed.
- **M5:** reach.
  - The bucket primitive (§10.4): the HF Storage Buckets routes, mutable
    keys with last-writer-wins and optional versioning, and GC, `reindex`
    and metering for bucket keys, with the generic labels of §10.4.
  - The bucket log and snapshots of §10.4, with `reindex` restoring every
    acknowledged key, and a small-object write benchmark. Its stated
    target, to be confirmed by the M5 benchmark: at least 200 one-key
    `batch` commits per second per deployment (§5.8's shard registration
    rate), with a one-key commit's p99 within 20 ms of the object store's
    own PUT p99.
  - Full export of repositories and buckets, the first milestone with
    both primitives: an operator or tenant can take everything out, tested
    by restoring the export into a fresh deployment (§1, no lock-in).
  - The bucket rows of the security tests (§5.9 requirements 2 and 7),
    including the `addFile` and `copyFile` checks of §5.4.
  - Cache tier.
  - HF pull-through mirror.
  - Multiple storage pools, residency and KMS.
- **M6:** our own client (§10.8).
  - `GET /v2/file-chunk-hashes/{file_id}`, which `sync` needs (until then
    it answers `404`; no stock client calls it).
  - `sync`, `mount`, storage-only mode, `login`, `setup git` (including the Git LFS download agent), `doctor`.
  - Python bindings.
  - **M6-URI: resource-address delivery**, consuming M2-URI. CLI, Rust/Python
    SDKs and the explicit fsspec adapter use the shared contract; test real
    client round trips on two deployments and hosted staging before launch.
    Publish the versioned support matrix and human/agent recipes; add separate
    resource-URI and HTTPS-copy actions to the UI. Legacy-handler coexistence,
    endpoint isolation and unchanged stock-client paths must pass; third-party
    native support is never assumed. The URI milestone handoff defines the
    acceptance checks and dependencies on implemented resource kinds.
  - The minimal self-service web UI (§10.10), with OIDC sign-in, the
    delegated-token and audit-log views.
  - `openxet mcp`, the MCP server, and approval gates for agent-initiated
    destructive or costly actions (§7.1).
  - Budgets and alerts on metered bytes and egress, per tenant and per
    token, with writes stopping at a hard budget (reads never do).
  - The two-way compatibility suite, including nightly runs against
    huggingface.co.

  The client's core (upload/download on `xet-data`) starts as early as M1,
  because it doubles as our own test driver.
- **M7:** distribution and docs.
  - Release pipeline, images, Helm (operator only if needed), compose,
    one-click templates, Nix, Homebrew, winget, PyPI, marketplace listings.
  - Docs site.
  - Publish the `xet://` specification, tested client/adapter support matrix,
    migration notes and accurate registration status with the M6 clients;
    verify packaged versions execute the documented recipes (ADR 0009).
  - Published benchmarks.
  - Time-to-first-upload check.
- **M8: openxet.com hosted service** (§15). Features, pricing and the
  end-to-end experience per spike S13 (draft PR #59), pending the owner's
  approval of pricing and positioning. Everything it needs from the core
  ships in the open-source build first: delegated tokens and per-tenant
  rate limits and quotas in M3, the audit log in M4, full export in M5,
  and the web UI, MCP, approval gates, budgets and alerts in M6. M8 adds
  only the hosted parts:
  - the 60 s journey from the landing page to a working `hf upload`,
    with sign-in through an identity provider only (GitHub, Google, SSO).
    OpenXet never owns users, so this is the same OIDC and authorizer
    seams self-hosters use;
  - billing on metering (§5.7), under the rule in §15;
  - load and capacity tests with the M3 harness against a
    production-size staging stack, before launch and before each scale
    step (issue [#55](https://github.com/stubbi/openxet/issues/55)).

## 13. Left to the builder

- Exact crate layout and names, HTTP framework, DB access library.
  Decided in M1: axum, sqlx (M1-R1); EdDSA JWT tokens (ADR 0007).
- Small-file threshold, cache design, partitioning scheme details.
- Global-dedup response shards are decided (M1-R4, M1-R30): synthesized
  from the xorbs of the newest visible shard that references the xorb
  holding the queried chunk, with a fresh random HMAC key and a
  `shard_key_expiry` of now + 7 days.
- Docs site tooling.

## 14. Name

The project starts as **OpenXet** *(decided by the user)*. Because it uses the
"Xet" name, there is some trademark risk. Keep brand-specific strings in a few
places (binary name, image name, docs domain) so a rename stays cheap if it
is ever needed.

Prior art found on 2026-10-23: another repository already uses the name
(`ggoggam/openxet`, Apache-2.0, 8 stars). The GitHub organization `openxet`
exists (created 2025-08-15, empty) and belongs to someone else, so our GitHub
organization needs a different handle; that is the user's choice. For now the
repository lives at `github.com/stubbi/openxet` *(decided by the user)*. The user
owns the openxet.com domain.

The future resource URI scheme is **`xet://`** *(decided by the user,
2026-10-24; ADR 0009)*. This is a name and endpoint-boundary decision, not
implemented syntax or ownership of the Xet protocol. M2-URI and M6-URI in §12
schedule the specification, compatibility audit and client delivery.

## 15. Hosted openxet.com *(decided by the user: we must have the perfect hosted experience)*

openxet.com is a first-class product, not an afterthought. "Perfect" is
measured by the same anti-goals as the open-source project: fast, little
effort for the user, and acting on feedback. Detailed design is its own brainstorming round (pricing, legal,
operations): spike S13 (draft PR #59) proposes it, and M8 (§12) delivers
it once the owner approves pricing and positioning. These requirements are fixed now because they shape the core:

**Experience:**
- From the landing page to a working `hf upload` in under 60 seconds: sign in
  with GitHub, Google or SSO; a token is shown with copy-ready commands; done.
- A free tier with no time limit, and no card needed to start.
- Every page shows what the user gains: stored vs. uploaded bytes and the dedup
  savings, in money where possible.
- Bring your own object-store bucket as a headline option: we operate OpenXet, and the data
  stays in the customer's S3/GCS/Azure account. An EU region is available from
  day one.
- Git hosting, `git-xet`, `hf`, our client and the web UI all work on
  openxet.com exactly as they do self-hosted.
- No dead ends: every error says what to do next. Support is reachable from
  every page, and replies come within one business day.

**Dogfooding rule:** openxet.com runs the released open-source build. There is
no private fork and no hosted-only core feature. Hosted-only parts are
operational: billing, sign-up, abuse handling, the status page. They live
behind the same seams (authorizer webhook, metering, admin API). Anything
openxet.com needs from the core is built into the core, which makes
self-hosting better too.

**What this adds to the core:**
- Hard multi-tenant isolation (per-tenant dedup visibility, pools per tenant,
  noisy-neighbour limits).
- Enforced quotas and per-tenant rate limits.
- Bounds on how much one writer's registrations shape the dedup answers
  other tenants get under `global` visibility (issue #50).
- Metering accurate enough to bill. Each tenant is billed on its own
  deduplicated bytes, as if it were alone, never crediting dedup across
  tenants: a bill that drops when another tenant holds the same chunks
  would reveal that they exist ([S12](../../research/S12-s3-api-and-dedup-economics.md);
  its ADR, S12's "stand-alone billing", is still to be written). These
  billed bytes are counted per tenant on their own, so their sum exceeds
  physical storage; §1a's 1% target concerns physical bytes attributed to
  tenants, not billed bytes.
- Self-service sign-up via the web UI (M6), through the operator's
  identity provider only.
- Admin tooling for abuse and takedowns.
- Zero-downtime rolling upgrades.

**Operations bar:**
- A published SLO with a public status page.
- Tested backups and restores.
- Cheap-egress storage and a CDN, so it can be profitable.
- Abuse reporting and DMCA/DSA takedown processes, plus terms of service and
  a DPA, before public launch.

**Truly useful, not just available:**
- Two or three design partners (aqora first) run on openxet.com before billing
  is switched on.
- We measure their time to first upload and their dedup savings, and act on
  what they report.

## 16. Best in class on every dimension *(decided by the user)*

The ambition is that no other project comes remotely close, on any
dimension. An ambition is only useful if it is measured. So each dimension
below has a bar we hold ourselves to and a way we prove it, publicly where
possible. Competitors are re-evaluated every minor release in
`docs/competitive-matrix.md`. The first entries come from the 2026-10-23
evaluation of Shardline, ggoggam/openxet, kebyn/xet-server and
guilt/xet-server.

| Dimension | Bar | Proof |
|---|---|---|
| Protocol correctness | Byte-exact with `xet-core`; every spec MUST implemented and tested | Reference-file golden tests; real-client suite (pinned, latest, oldest supported); nightly against huggingface.co; spec-section traceability (each MUST maps to a test) |
| Performance | Fastest Xet server on upload, download, dedup ratio and metadata latency | Public benchmark suite run in CI against competitors on identical hardware; regressions fail CI; results published per release; the load test suite (`loadtest/`, issue [#55](https://github.com/stubbi/openxet/issues/55)) runs in full on the release rig for every release and briefly every night, and a regression past its threshold fails the release; the harness defines the thresholds, and they are recorded in [benchmarks](../../reference/benchmarks.md) |
| Code quality | Small, readable, reviewed code with no slop | Strict clippy and `forbid(unsafe)`; mutation testing (`cargo-mutants`) on core crates; fuzzing continuously (OSS-Fuzz once eligible); lines of code tracked as a cost, not a goal |
| Architecture | Every decision recorded and reversible | ADRs for every significant choice; crate boundaries enforced (no cyclic or upward deps, checked in CI); bucket-as-truth and stateless invariants have dedicated tests |
| Security | Secure by default, with a verifiable supply chain | The §5.9 requirements, each with its test; `SECURITY.md` with a response SLA; OpenSSF Scorecard ≥ 9; signed releases with SLSA provenance; reproducible builds; `cargo-deny` + `cargo-vet`; a third-party audit before 1.0 |
| Documentation | Every user finds the answer in under a minute | Diátaxis structure (tutorials, how-tos, reference, explanation); every example runs in CI; reference generated from code (CLI, config, OpenAPI); `llms.txt`; a docs feedback link on every page |
| Governance | Trustworthy beyond one person | `GOVERNANCE.md` (roles, decision process, path to maintainer); every change merged via PR with at least one independent review (human, or an agent reviewer plus human sign-off), required by process; branch protection enforces the PR, the required checks and linear history, but not the review (0 required approvals, `enforce_admins: false`) until a second maintainer exists (`GOVERNANCE.md`); an RFC process for big changes; DCO; code of conduct; a public roadmap |
| Community | Contributors feel welcome and succeed | Labelled good-first issues; contributor guide tested by newcomers; first response to issues and PRs within two business days; credit in every release's notes |
| Release engineering | Boring, predictable releases | SemVer; changelog generated from Conventional Commits; release train cadence; upgrade tests between consecutive versions; a stated support window |
| UX (humans and agents) | Best on both (§7.1) | Time-to-first-upload measured per release; the agent leg of the suite; accessibility (WCAG 2.2 AA) checked for the web UI |
| Operations / hosted | openxet.com is the reference deployment (§15) | Public status page and SLO; chaos and failover drills in CI; disaster-recovery restore tested on a schedule |
| First visit (§1, visitors become fans) | First-time visitors, of the project and of openxet.com, become loyal fans | **Time to wow:** an upload in < 60 s hosted and < 2 min self-hosted, measured per release. **No dead ends:** every error carries a next step (checked by the agent leg) and every doc example runs in CI. **Honesty:** every published number links its measurement. **No lock-in:** a full export, tested by restoring it into a fresh deployment. **Fair, predictable pricing:** published prices, budgets and alerts. **Community:** issue and PR response times published, a mentored first-PR path, every contributor credited in release notes, a public roadmap, and a changelog that names the feedback it acts on. **Parity:** openxet.com runs the released build (§15). **Metrics,** published per release: time to first success, first-response time, contributor return rate and hosted retention |

Bus factor is a real risk while one person leads the project. Governance work
starts at M0, not after 1.0.

## 17. Rulings register

Rulings are decisions made while building, numbered in the order they were
made. Public code and docs cite them by number; this is what each one
decides, with superseded parts marked.

| Ruling | Decision | Where |
|---|---|---|
| R19 | The HF pull-through mirror ingests verified chunks only, never fetches or serves gated or private content with another caller's credential, and is disabled on openxet.com. | §10.6, S8 |
| R21 | Security issues found in third-party software are reported privately to its maintainers. | `AGENTS.md` |
| R22 | Git reads and Hub reads by revision serve only objects reachable from a current ref, on every path; ref writers obey the same rule. | §5.9 req 8, ADR 0004 |
| R24 | Objects a reindex can't place are persisted and protected, never discarded automatically, and never block repacks. | ADR 0004, S9 |
| R25 | Xet reads by hash follow scope membership, not Git reachability; removing data takes GC or `openxet purge`. | §5.4 |
| R26 | Public text never describes or hints at an unfixed third-party security weakness. | `AGENTS.md` |
| R29 | Upload validation composes upstream primitives with OpenXet's own policy checks; format logic is never reimplemented. | ADR 0001, S5 |
| R30 | Shard verification takes a synchronous chunk-table lookup, caps distinct referenced xorbs, and ignores unreferenced CAS-info xorbs. | S6 |
| M1-R1 | The CAS is axum on tokio, the index is sqlx (SQLite now, Postgres in M3), and the local-disk store is OpenXet's own fsynced module; `object_store` serves S3, GCS and Azure from M3. | M1 plan |
| M1-R2 | CAS tokens are EdDSA JWTs verified against a public key set; static tokens are compared by SHA-256 in constant time. | §5.4, ADR 0007 |
| M1-R3 | Signed transfer URLs bind xorb, range and expiry in an HMAC; a missing `Range` is `400`, any mismatch `403`. | §5.5, ADR 0002 |
| M1-R4 | Dedup responses are synthesized from the newest visible xorb containing the chunk, keyed with a fresh random HMAC key and a 7-day `shard_key_expiry`. The single-xorb part is superseded by M1-R30. | §13, §5.6 |
| M1-R5 | The GC grace defaults to 30 days and is never below 28 (21-day client shard cache + 7-day dedup key). | §5.6, S10 |
| M1-R6 | `GET /v2/file-chunk-hashes` is built in M6 and answers `404` until then. | §12, ADR 0006 |
| M1-R7 | Metering ships in M4 with the admin API. | §5.7, §12 |
| M1-R8 | Xorb validation runs on blocking threads bounded by one permit per CPU; double decompression on accept is kept and benchmarked. | M1 plan |
| M1-R9 | The client version floor is `hf_xet` 1.7.0. | §11, S10 |
| M1-R10 | The default bind is `127.0.0.1:8080`, which is also `hf_xet`'s default CAS endpoint. | S10 |
| M1-R11 | CAS tokens carry a scope, a mode and an expiry, never a Git revision: Xet reads follow scope membership (R25), so a revision binding would enforce nothing. Hub routes still accept a revision in the URL for compatibility. | §5.4, ADR 0007 |
| M1-R12 | Stored shards and dedup shards are built by composing `xet-core`'s serializers (truncating at the file lookup section, rewriting the header or footer through them), never by writing format bytes by hand. | ADR 0001 |
| M1-R13 | An upload shard listing one file hash twice with different SHA-256 values is rejected; for identical duplicates the first entry is kept, and the CAS info lists only kept references. | S6 |
| M1-R14 | Shard verification itself rejects duplicate file hashes with conflicting SHA-256 values, so registration never meets a content error after verification passed. | S6 |
| M1-R15 | New dependencies take their latest stable release unless a ruling pins one; the index uses sqlx 0.9 (amends M1-R1). | M1 plan |
| M1-R16 | A file's size and terms are stored per (file, scope), and a scope reads only its own record; another scope's file reads exactly as one never stored. | §5.4 |
| M1-R17 | Registration rejects terms naming a xorb that is missing or not visible to the caller, with one error that doesn't tell the two apart, so registration is no existence probe. | §5.4, §5.9 req 4 |
| M1-R18 | CAS tokens are checked by OpenXet's own EdDSA-only compact JWS verifier on `ed25519-dalek`'s strict verification, with a strict header and a size cap, tested against RFC 8037 appendix A.4. The dependency tree carries no ignored security advisory. | ADR 0007 |
| M1-R19 | CAS-token claims are `sub`, `scope`, `mode`, `exp`, `iat` and an optional, enforced `nbf`; any other claim is rejected, so a restriction OpenXet can't honour never fails open. Token lifetime is capped (24 h by default), `iat` may lie at most 5 min ahead, and a `sub` or `scope` that is empty, only whitespace or holds control characters is rejected, as is a `scope` over 1024 bytes. Its 24 h signed-URL cap is superseded by M1-R42. | §5.4, ADR 0007 |
| M1-R20 | Logs and spans record the matched route template, never the raw path or query, so no URL segment can leak a credential; a request is traced by its request id. | §5.9 req 6 |
| M1-R21 | An object the index names but the bucket lacks answers `500` with an integrity log, not a retryable `503`: object stores are read-after-write consistent, so it means data loss or a bug, and a retry would hide it. | §5.3 |
| M1-R22 | A refused request's body is drained up to the route's size limit before the answer, as a lingering close does, so a client sees the final `401` or `403` instead of a connection reset it would retry; the body read deadline bounds the cost. | §5.9 req 1, routes reference |
| M1-R23 | Verification errors name the rule, the file, the xorb and the chunk range the client sent, and never a value derived from stored data (expected hashes, lengths, chunk counts); those are logged only. | §5.9 req 5, S6 |
| M1-R24 | Shard commits (bucket write through index registration) are bounded per process by a commit semaphore of 4 permits per CPU, separate from the validation permits; ingest memory is at most validation permits × the verification limits plus commit permits × a stored shard and its files. | M1 plan |
| M1-R25 | Blocking work (validation, verification, reconstruction rendering, dedup builds) takes its permit inside the blocking closure, so a client disconnecting can't release a permit while its work still runs. Dedup's use of the `validate` pool is superseded by M1-R32's own pool. | M1 plan |
| M1-R26 | Reconstruction trims the first and last kept terms to the chunks covering the requested range, as `xet-core`'s reference server does, so fewer bytes are transferred. | S10 |
| M1-R27 | Ranged bucket reads are validated before the `206` and then streamed in 1 MiB blocks, so a transfer holds one block of its range at a time. | §5.5 |
| M1-R28 | Generated secrets are `oxt_` followed by 43 base64url characters encoding 32 bytes from the operating system's CSPRNG; the fixed prefix makes leaked secrets findable by secret scanners. | §5.4 |
| M1-R29 | In a configured server, a transfer key missing at its default path is generated with a warning, and one missing at an explicitly configured path is an error; remote buckets (M3) will require a configured key, since every node behind one endpoint needs the same one. | §5.5 |
| M1-R30 | A dedup answer offers every xorb of the newest shard that references the matched xorb and is visible to the caller, in the shard's term order from the matched xorb, wrapping round, up to the answer budget of M1-R32 and without lookup tables; with no such shard, the matched xorb alone. | §5.4, S11 |
| M1-R31 | Independent tasks run in parallel, each in its own worktree, with review gates unchanged; from M2 on, plans give a dependency graph with file ownership. | §8 |
| M1-R32 | Dedup answers are at most 16 MiB (about 350 typical xorbs, about 22 GB of file) and hold a `dedup` permit, a pool of a quarter of the CPUs separate from `validate`, from loading their chunk tables until their body has been sent or dropped; dedup memory per process is at most those permits × about 150 MB (measured: about 120 MB for one answer, about 300 MB with 2 permits busy). | §5.4, S11 |
| M1-R33 | A dedup query waits at most 1 s for a `dedup` permit, then answers `429 dedup_busy` with `Retry-After: 1`, which clients read as "no match"; sending an answer has a deadline of 30 s plus its length at 1 MiB/s, after which the body fails and its permit and memory are released. | §5.4, S10, S11 |
| M1-R34 | A reconstruction renders with extra blocking threads up to half of the free `validate` permits, counting the request's own permit, so one large answer never starves uploads. | benchmarks reference |
| M1-R35 | A required CI gate must pass 10 of 10 runs before it merges; a flaky gate is fixed, never loosened. | §8, benchmarks reference |
| M1-R36 | The documentation site is rendered from `docs/` with a pinned mdBook at deploy time, and nothing generated is committed; `llms.txt` and per-page Markdown are generated, and CI builds and link-checks the site and requires every reference page in its navigation. | §7.1 |
| M1-R37 | Who may read what lives in the bucket before the index. A shard registration first creates, only if absent, `registrations/{sha256(scope)}/{sha256(tenant)}/{shard hex}` holding `{"v":1,"scope","tenant","shard","seq"}`; every retry reuses that record and its number. A xorb upload puts `xorb-owners/{sha256(tenant)}/{xorb hex}` holding `{"v":1,"tenant","xorb"}` before the index records the owner. The index keeps, per file and scope, the row of the lowest `(seq, shard, tenant)`, so its tables equal what the records, stored shards and stored xorbs rebuild. | §5.3, `openxet-storage`, issue #26 |
| M1-R38 | Every principal carries a tenant: a CAS token's optional `tenant` claim, a static token's configured `tenant`, or the default of M1-R43. Xorb visibility, dedup answers, xorb existence checks and shard verification lookups key by tenant; file reads and registrations by scope. `per-scope` visibility is renamed `per-tenant`. | §5.4, ADR 0007 |
| M1-R39 | Registering a shard makes the registering tenant an owner of every xorb its files reference, in every visibility mode, in the same transaction and in the bucket record, so switching `global` to `per-tenant` never breaks a read or a dedup answer. Ownership covers the whole xorb. | §5.4 |
| M1-R40 | One process-wide memory budget bounds buffered request bodies and built reconstruction answers: `server.memory_budget`, by default a quarter of the memory the process may use (on Linux the smallest cgroup limit over it, if lower), from 256 MiB to 1 TiB, and 1 GiB where memory can't be read; a budget over half the memory is warned about. Reservations and the `commit` permit are held by the work they guard, so they outlive a client that leaves while uncancellable work runs. Uploads reserve their `Content-Length` (the route's limit if none, never more than declared) before a byte is read, until stored and registered; they queue in arrival order, unread, up to 120 s. A reconstruction reserves its decoded record from the stored term count before loading it, then its answer's bound, then its size until sent, under a send deadline of 30 s plus 1 s per MiB; an answer that fits takes free bytes at once, past queued uploads, and one that doesn't waits up to 5 s. After its wait a request gets `503 memory_busy` with `Retry-After: 1`. One answer may reserve at most an eighth of the budget, else a final `413 answer_too_large`; an upload over the whole budget gets a final `413 exceeds_memory_budget`. A batch holds at most 100,000 terms, checked from stored term counts before decoding; the count runs before any permit, not under the validation permit (accepted deviation: the reservation sized from it must precede the permit, the one order every request takes them in). Xorb commits take a `commit` permit. `accept` is capped by `server.max_connections` (default 4096); further connections wait in the listen backlog. Per-credential budget shares and connection limits are M3's, with rate limiting. | §5.8, S10 |
| M1-R41 | This register lists every ruling that tracked code or docs cite. | §17 |
| M1-R42 | Signed transfer URLs live 30 to 60 minutes (60 by default), so a leaked URL, or one issued before a purge, stops working within the hour; supersedes M1-R19's 24 h URL cap. | §5.4, §5.5, ADR 0002 |
| M1-R43 | A credential that names no tenant is in tenant `scope:` followed by its scope, and no explicit tenant may start with `scope:`, so a scope named like a tenant never joins it. Migration 0005 and the bucket records use the same default. | §5.4, ADR 0007 |
| M2-R1 | New crates `openxet-http` (the route builder, request ids, the error body and the middleware stack, extracted from `openxet-cas`), `openxet-hub`, `openxet-git` and `openxet-uri`; the authorizer lives in `openxet-auth`. `openxet serve` composes the CAS, Hub and Git routes into one router with one fallback, one state per crate and one middleware stack, and `clippy.toml` bans `Router::fallback` and `Router::fallback_service` outside the builder. | §5.2, §12 M2, issue #46 |
| M2-R2 | A repository has an immutable UUIDv7 id, rendered as 32 lowercase hex characters. Its CAS scope is `repo:{id}`, its tenant is its namespace, and bucket keys for its data carry the id (`repos/{id}/…`); namespace and name live in its metadata record, so a rename rewrites one record. The scope prefix `repo:` is reserved: a configured static token with such a `scope` is a configuration problem, and a platform-signed CAS token with one is accepted, because the platform holding a signing key is trusted with any scope. Namespace and name each match `^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$` and contain no `..`, the name doesn't end in `.git`, and `{namespace}/{name}` is unique case-insensitively, stored as written, across labels. The namespaces `models`, `datasets`, `spaces`, `kernels`, `buckets`, `api`, `v1`, `v2`, `openapi.json`, `healthz` and `readyz` are route prefixes, and the Hub refuses to create them. | §5.4, ADR 0012, ADR 0010 |
| M2-R3 | A credential is a CAS credential (scope, tenant, mode), a user credential (subject plus claims) or both: a static token without `scope` is a user credential, one with `scope` a CAS credential, one with a `scope` and user fields both, as the dev token is. The `Authorizer` decides `read`, `write` and `create` per repository or namespace; M2's config-rules authorizer reads `[[auth.rules]]` (`subjects` and/or `claims`, either of which matching the principal is enough, subjects exactly with no wildcard; `repos` globs, matched ignoring ASCII case; `actions`, where `write` includes `read` and `create` is granted only by a pattern covering the whole namespace, `org/*` or `*`), and a request is allowed if any rule allows it. A rule naming the pseudo subject `anonymous` grants everyone, signed in or not; no rule names it by default. Dev mode installs `subjects = ["dev"], repos = ["*"], actions = ["read", "write", "create"]`. `whoami-v2` answers `name` from the subject (or the static token's `name`) and `orgs` from the `orgs` claim. OIDC, the webhook authorizer and OpenXet access tokens come in M3. | §5.4, ADR 0012 |
| M2-R4 | `preupload` answers `lfs` when a file is at least `hub.lfs_threshold` (default `1MiB`, bounds `1KiB` to `10MiB`) or its path matches an LFS pattern of the target revision's `.gitattributes`, and `regular` otherwise. A regular file's decoded content is at most `hub.lfs_threshold`; a commit's NDJSON body is at most `hub.max_commit_bytes` (default `64MiB`) and 25,000 operations. The M2 benchmark may revise the default, with the number. | §6 |
| M2-R5 | `POST /api/repos/create` writes a server-generated `initial commit` on `main` holding huggingface.co's default `.gitattributes` (the S3 capture), authored and committed by `openxet <noreply@openxet.com>`. | §6, S3 |
| M2-R6 | Hub errors are `{"error", "schema": "openxet.error.v1", "code", "message", "hint"}` with `X-Error-Code` and `X-Error-Message`; CAS routes keep the M1 body. The unknown-repository answer is one constant `404` (`RepoNotFound`, `Repository not found`, body and hint fixed) for a missing repository, one the caller may not read, and a typed view the caller may not read whose label the repository lacks. A missing credential is `401` with `WWW-Authenticate: Bearer realm="openxet"` and a message naming `hf auth login`; an invalid one is `401` with `Invalid credentials in Authorization header`. Amended in M2 Task 15: the JSON body is for API routes; the Git smart HTTP routes answer the same status and headers with the message and hint as `text/plain`, which git shows as `remote:` lines, their unknown-repository text equally constant, and their `401` offers `Basic` first. | §6, ADR 0010, S3 |
| M2-R7 | A Hub commit is a real commit built with gix from the parent's tree: `file` operations become blobs, `lfsFile` operations canonical pointers whose sha256 is registered to the repository's scope, and deletes remove entries. The author is the subject (`{subject} <{subject}@users.noreply.openxet.com>` unless an `email` claim is present), the committer `openxet <noreply@openxet.com>`, the message `summary`, a blank line and `description`; a `parentCommit` mismatch is `412 precondition_failed`; `commitUrl` is `{public_url}/{prefix}{ns}/{name}/commit/{oid}`. | §6, ADR 0004 |
| M2-R8 | `tree`, `commits` and the repository listings answer at most 1,000 entries per page, ordered by path or commit order, with `Link: <…?cursor=…>; rel="next"` when more follow; the cursor is the last entry's key, base64url-encoded, and `X-Total-Count` is set where S3 observed it. | §6, S3 |
| M2-R9 | Xet files resolve with `302` to `{public_url}/v1/transfer/files/{repo id}/{xet hash}?name={filename}&commit={commit}`, served with the caller's credential (Bearer, or Basic with the token as the password; `401` challenges Basic first), HTTP `Range` (all three forms, `416` past the end), `ETag: "{sha256}"` (the Xet hash when none was registered), `If-Range`, `HEAD`, `Content-Disposition` and `Accept-Ranges`; LFS `basic` download `href`s are the same URL without `name` or `commit` and with no `authenticated` field. Small files are served inline from the Git blob with `ETag: "{blob sha1}"`, their size reserved from the memory budget before the blob is read. Amended in Task 13: `commit` added, since `huggingface_hub` reads `X-Repo-Commit` from the gateway after a same-origin redirect; the gateway echoes it as given (checked as a commit id only), so it is informational. | §5.5, §6, ADR 0003 |
| M2-R10 | Until server-side chunking (M4), an LFS upload batch that doesn't offer `xet` answers each object with an object error `{"code": 501, …}` naming the `git-xet` install command, never a `basic` `href` that fails later. | §6.1, §10.3 |
| M2-R11 | Smart HTTP is served at `/{prefix}{ns}/{name}.git/…` and without `.git`; a gzip request body is inflated, `Git-Protocol: version=2` selects v2, and the `Basic` (the token as the password) and `Bearer` schemes are accepted. The first push to a missing repository creates it, with the `model` label, if the authorizer allows `create` in the namespace. | §10.9, ADR 0004, ADR 0005 |
| M2-R12 | OpenXet's own API lives under `/api/openxet/v1/`; labels are `GET …/repos/{ns}/{name}/labels`, `PUT` and `DELETE …/labels/{label}`, and `openxet label add`, `remove` and `list`. Each change writes the repository's metadata record to the bucket, then the index, then an `audit_log` row (`actor`, `action`, `repo`, `detail`, `at`), the table M4's audit API exposes. | §10.4, ADR 0010 |
| M2-R13 | Running `openxet purge <file-hash>` (with the server config) writes `purges/{hash}` to the bucket, then deletes the file's rows from every scope, never dedup rows (those belong to the xorbs, whose bytes leave through GC), writes an `audit_log` row and prints what it removed; `reindex` (M4) skips purged hashes. It takes `--dry-run`, `--yes` and `--json` (`openxet.purge.v1`). | §5.4 |
| M2-R14 | `xet://{host}[:{port}]/[{kind}/]{namespace}/{name}[@{revision}][/{path}]`: no `{kind}` for a repository, `datasets`, `spaces` or `kernels` for a typed view, `buckets` for a bucket (resolved from M5); `@{revision}` is a ref or OID (names can't contain `@` under M2-R2, so nothing needs escaping); a percent-encoded UTF-8 path (RFC 3986); no userinfo. Hostless legacy forms and forms naming `xethub.com` are rejected with an error that says how to write the address, and nothing is sent to a default server. | ADR 0009 |
| M2-R15 | The `openxet openapi` command prints the OpenAPI 3.1 document generated from the route builders' `utoipa` annotations; it is checked in at `docs/reference/openapi.json`, served at `GET /openapi.json`, and CI fails when the two differ. | §7.1 |
| M2-R16 | Git budgets take ADR 0004's defaults (`git.reachability.max_commits` 200,000, `max_ms` 2,000, `max_concurrent_per_repo` 2, `max_concurrent_per_principal` 4; `git.thin_bases.max_objects` 2,000,000, `max_ms` 2,000), and the M2 benchmarks record the measured values that replace the thin-base placeholders. Amended by M2 Task 20: measured, `git.thin_bases.max_objects` is 1,000,000 and `max_ms` stays 2,000 (benchmarks reference, ADR 0004 item 3). The node cache is `git.cache_dir` (default `{data_dir}/git-cache`) with snapshots under it, `git.cache_max_bytes` (default `20GiB`), and `git.max_plain_blob_bytes` is unset by default. | ADR 0004 |
| M2-R17 | On a pull request the reconstruction latency gate is relative to the PR's base, the base branch's tip recorded in the event (`pull_request.base.sha`), measured in the same job on the same runner: both trees run the PR's measurement code (its gate test and fixture copied over the base's), each in a release build in its own target directory with `OPENXET_LATENCY_GATE=measure`, each printing one `openxet.latency-gate.v1` JSON line per shape (`build`, `shape`, `p50_ms`, `p99_ms`, `max_ms`, `runs`; `build` is the checkout's commit, fixed at compile time). `scripts/check-latency-gate.py` exits 2 when the head's `build` isn't the checked-out commit, both runs share a `build`, or a value isn't a finite non-negative number, and fails a shape when the PR's p99 is at or past 50 ms and more than 1.25 × the base's p99, or its p50 > max(1.25 × the base's p50, the base's p50 + 2 ms), writing both runs and the ratios to the job summary. On `main` the gate asserts p99 < 50 ms on the pushed commit alone (the default `assert` mode), reported, not required; the release rig (issue #55) enforces the absolute bar unconditionally. | §8, benchmarks reference, issue #83 |
