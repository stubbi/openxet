# openxet — Design

Date: 2026-10-23 · Status: draft for review

## 1. Intent

Hugging Face's Xet storage gives chunk-level deduplicated storage for large,
versioned files (models, datasets, checkpoints). The client side is open source
(`huggingface/xet-core`, Apache-2.0) and the protocol is published
(Xet Protocol Specification v1.1.0, https://huggingface.co/docs/xet). The server
is not. Nobody can run Xet on their own infrastructure.

**openxet is the open-source, self-hostable server for the Xet protocol.** The
official, unmodified Hugging Face clients (`hf_xet`, `huggingface_hub` /
`hf` CLI, `@huggingface/hub`) work against it. `git-xet` follows after v1 (§10). It runs on storage
and identity systems people already own.

It is not a Hugging Face copy. It is plug-and-play infrastructure: small surface,
clear seams, no opinions forced on the operator.

### Who it is for

- Organizations that cannot or will not put data on huggingface.co (regulated,
  sovereign, air-gapped, cost-sensitive) but want HF-style workflows.
- Internal ML platform teams who today use MinIO + Git LFS / DVC and get no
  cross-version deduplication.
- Platforms that host other people's models and datasets (aqora is a likely
  first user) and want Xet storage under their own product, with their own
  naming, permissions and UI.

### What success looks like

- From zero to a working `hf upload` against openxet in under two minutes:
  one command, one environment variable.
- Uploading a slightly changed 10 GB checkpoint transfers megabytes, not
  gigabytes.
- It scales by adding nodes, with no redesign, from a laptop to petabytes.
- Operators recommend it because it is boring to run.

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
   and recovery from the bucket alone.

### Non-goals (v1)

- No web UI, model cards, discussions, likes, search, Spaces or inference.
- No user accounts, passwords or user database. openxet never owns users.
- No git protocol hosting (`git clone` of repos). Git/LFS integration comes
  after v1 (§10).
- No custom distributed object store (§5.3).
- No openxet-specific client in v1. The design keeps room for one (§10).
- No legacy (non-Xet) Git LFS upload path. Clients without `hf_xet` get a
  clear error telling them to install it; the server never chunks on ingest.
  *(builder-check call)*
- No per-tenant buckets or data residency inside one deployment. v1 uses one
  storage pool per deployment, and residency is handled by running separate
  deployments. Xorb records carry a pool id so multiple pools can be added
  later; dedup is per pool. *(builder-check call)*

### Anti-goals (failure even if it technically works)

- **Slow**, at upload, download or metadata operations.
- **Hard to set up**: more than one command and one variable to first upload,
  confusing config, or mandatory extra services.
- **Not listening to users**: feedback that has nowhere to go or is not acted on.
- Opinionated all-in-one: forcing its own auth, naming, UI or storage on people.
- Silent incompatibility: anything that makes data written by openxet unreadable
  by stock Xet clients, or vice versa.
- Losing data. Deletion happens only through explicit, auditable garbage
  collection with a grace period.
- AI slop: speculative abstractions, comment noise, dead code, untested paths,
  docs that drift from behaviour.

## 3. Constraints and verified facts

These were checked against primary sources on 2026-10-23
(spec at huggingface.co/docs/xet; `huggingface_hub` main @ `03b2828`;
`xet-core` main). Re-verify before relying on them; the compatibility test suite
(§8) is the lasting source of truth.

- **CAS API** (spec "api" page): `GET /v2/reconstructions/{file_id}` (preferred;
  clients fall back to v1 on 404/501), `GET /v1/reconstructions/{file_id}`
  (deprecated), `POST /v1/reconstructions` (batch), `GET
  /v1/chunks/default-merkledb/{hash}` (global dedup, returns a shard),
  `POST /v1/xorbs/default/{hash}`, `HEAD /v1/xorbs/default/{hash}`,
  `POST /v1/shards`, `POST /v2/shards` (NDJSON progress stream; clients fall
  back to v1 on 404), `HEAD /v1/files/{file_id}`, `GET
  /v2/file-chunk-hashes/{file_id}` (delta uploads). `xet-core/openapi/cas.openapi.yaml`
  also lists `/v1/telemetry`. Hash-to-hex conversion reverses bytes within each
  8-byte group, which is a classic source of bugs.
- **Auth:** clients obtain `{accessToken, exp, casUrl}` from
  `GET {HF_ENDPOINT}/api/{repo_type}s/{repo_id}/xet-{read|write}-token/{revision}`.
  The CAS token is scoped to repo and ref, and `write` supersedes `read`.
  The same info can arrive as headers `X-Xet-Cas-Url`, `X-Xet-Access-Token`,
  `X-Xet-Token-Expiration`. Clients refresh about 30 s before `exp`.
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
- **Uploads pass through the server.** The protocol has no presigned upload;
  xorb bytes are POSTed to the CAS, which must verify them anyway. My earlier
  "data never passes through openxet" holds for downloads only.
- **Downloads can bypass the server.** v2 reconstruction returns signed URLs.
  The client sends a `Range` header, multi-range when one fetch entry has
  several ranges, and expects `multipart/byteranges`. S3/GCS/Azure do not
  serve multi-range GETs. `xet-core`'s client treats the URL as opaque (no
  parsing of `X-Xet-Signed-Range`), so emitting **one range per fetch entry
  pointing at a native presigned URL** should work. This is spike S1.
- **Global dedup:** only "eligible" chunks are queryable: the first chunk of
  each file, plus chunks whose hash's last 8 bytes (LE u64) % 1024 == 0.
  Responses are shards whose chunk hashes are HMAC-keyed. This keeps the
  global dedup index about 1/1000th of the chunk count, which is what makes
  scale tractable.
- `huggingface_hub` now knows a `bucket` repo type (HF Storage Buckets:
  mutable, no git revisions). Relevant prior art; not in v1.
- `xet-core` crates (`xet-core-structures`, `xet-data`, `xet-client`) are
  Apache-2.0 and contain the reference merklehash, xorb, shard and chunking
  code.

## 4. Decisions made with the user

- Build both the storage core and the Hub-compat layer in v1, as two cleanly
  separated parts of one product. The storage core is usable alone.
- Plug-and-play over all-in-one; not an HF clone.
- openxet does not own users; identity and permissions come from the
  operator's existing systems.
- Scaling is designed in from the beginning.
- Storage is an adapter over existing object stores, not a built-in
  distributed store. Local disk is a first-class backend for single-node and
  dev.
- Rust. Apache-2.0.
- Quality bar: excellent `AGENTS.md`, best practices, useful tests kept
  current, current docs, check git history before changes to avoid regressing
  past decisions, deliberate distribution, no AI slop, user-obsessed.
- Our own client: not in v1, keep the door open.

## 5. Architecture

### 5.1 Shape

```
           hf CLI / huggingface_hub / hf_xet / git-xet / huggingface.js
                 │ Hub API                       │ CAS API            │ xorb ranges
                 ▼                               ▼                    ▼
        ┌────────────────────────── openxet (stateless nodes) ─────────────────┐
        │  hub layer (optional)   │   cas core   │  transfer (optional role)   │
        │  repos/refs/commits/    │ xorb/shard   │  signed-range reads,        │
        │  tree/resolve/tokens    │ verify, index│  multipart, HTTP gateway,   │
        │                         │ reconstruct  │  local-disk serving, cache  │
        └────────────┬────────────┴──────┬───────┴──────────────┬─────────────┘
                     │ auth adapters     │ metadata index        │ object store
                     ▼                   ▼                       ▼
           OIDC/JWT · webhook ·    Postgres | SQLite       S3 · GCS · Azure ·
           static tokens                                    MinIO/R2/Garage · local disk
```

- One binary, `openxet`. Roles (`hub`, `cas`, `transfer`) are enabled by config.
  Default is all-in-one; large deployments split and scale roles separately.
- Nodes hold no state that matters. Any node can serve any request. Scale by
  adding nodes behind a load balancer.

### 5.2 Components (crate-level seams; exact layout is the builder's call)

- **protocol**: hash-string conversion, xorb and shard parsing, serialization
  and verification. Prefer depending on `xet-core-structures` for the
  reference implementation; wrap it so upstream churn is contained in one
  place. Spike S5 checks how usable it is server-side.
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
  (local disk), for multi-range efficiency and for an optional NVMe or CDN
  cache tier. Signed, short-lived, range-bound URLs.
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

**The bucket is the source of truth.** Xorbs, uploaded shards, and Hub-layer
commit objects are written to the bucket. The database is an index that
`openxet reindex` can rebuild from the bucket. Losing the database must never
lose data.

Durability details *(builder-check calls)*:
- Commit objects and storage-only pins are written to the bucket
  **synchronously**, before they are acknowledged.
- Branch and tag pointers live in the database, which is authoritative while
  it exists. They are also appended to a ref log in the bucket within seconds
  (documented RPO ≤ 60 s). So after total database loss, `reindex` may roll
  a branch back to an earlier commit, but no data or commit object is lost.
  Newer commits it cannot place on a branch are listed as recoverable.

### 5.4 Identity, authorization and tenancy

openxet never stores users. A request carries a credential; an
**authenticator** turns it into a principal (subject plus claims); an
**authorizer** decides read or write on a repo or scope.

Authenticators (v1):
- **OIDC/JWT**: validate tokens from any OIDC issuer via JWKS (Keycloak,
  Auth0, Okta, Entra, Google, GitHub Actions OIDC and others).
- **Static tokens**: from config, for dev, CI and small setups.
- **openxet access tokens** *(decided by the user)*: long-lived, revocable
  tokens for tools like `hf auth login`, each bound to an external identity.
  - A user proves who they are once through the operator's identity system,
    for example `openxet login` (OIDC device or browser flow), a minimal
    login page, or the platform's backend calling the admin API.
  - openxet then issues a token. It stores only a hash plus the bound subject,
    claims snapshot, scopes, expiry and last use.
  - Authorization is re-evaluated against the authorizer on use, so revoking
    access upstream takes effect even if the token itself is still valid.
  - openxet still owns no users: no passwords, profiles or sign-up.
  - `whoami-v2` answers from the bound identity, with names and orgs mapped
    from claims via config or the webhook.
- **Who may create repos:** decided by the authorizer, as a `create` action on
  the namespace.

Authorizers (v1):
- **Config rules** on claims and repo patterns (e.g. group `ml-team` may
  write `org/*`).
- **Webhook**: openxet asks the operator's backend "may subject X read/write
  repo Y?", with a short-TTL cache. This is how platforms like aqora plug in.

CAS tokens are short-lived tokens openxet signs itself, scoped to
(scope, revision, read|write, exp). Every node verifies them statelessly.
Keys rotate through a key set.

**Storage-only mode:** a platform with its own naming layer gets CAS tokens
for its users either through an admin endpoint or by signing them itself
with a configured key. It then stores Xet file hashes in its own database.

**Scopes and tenants (builder-check call):**
- A **scope** is the unit of access. In the Hub layer a scope is a repo
  (`{type}/{namespace}/{name}`), matching HF's repo-scoped Xet tokens. In
  storage-only mode it is an opaque string the platform chooses.
- A **tenant** is the unit of isolation for `per-scope` dedup and metering.
  In the Hub layer it is the namespace, the first path segment; in
  storage-only mode it is configurable.
- A commit in repo B may only reference files registered to repo B. A file
  already in repo A is registered to B by uploading it to B; dedup makes
  that nearly free, because only a shard is sent.

**File access scoping:** files registered by a shard upload are associated
with the token's scope. Reconstruction and head-file only succeed for files
reachable from the caller's scope. Xorbs remain globally content-addressed
and shared.

**Global-dedup visibility** is configurable:
- `global`, the default, follows HF's model with HMAC-protected responses.
- `per-scope` is for multi-tenant hosting, where revealing whether a chunk
  already exists across tenants is unacceptable.

### 5.5 Data paths

- **Upload:** client → CAS (`POST xorbs` streamed and verified into the object
  store; `POST shards` verified and registered). This scales with node count.
- **Download:** client → CAS reconstruction (small JSON). Then either:
  - **(a)** direct from the object store via native presigned URLs, one range
    per fetch entry (S1); or
  - **(b)** via the transfer role, with signed range URLs and
    `multipart/byteranges`, for local disk and caching setups.

  Mode (a) is the default when the backend supports it. v1 reconstruction is
  served too, for older clients.
- **Plain HTTP read gateway:** `GET /{repo}/resolve/{rev}/{path}`, when the
  redirect is followed, streams the reconstructed file with HTTP `Range`
  support. This makes openxet readable by anything that speaks HTTP or `hf://`
  (curl, browsers, fsspec, pandas, polars, and DuckDB if its `hf://` endpoint
  can be pointed at openxet, spike S4). It is served by the transfer role so
  it scales and caches independently.

### 5.6 Garbage collection and retention

- **References:** Hub-layer commits reference files; in storage-only mode the
  platform pins files through an admin API, optionally with a TTL. Files
  reference xorbs.
- **Mark and sweep with a grace period:** default 7 days, configurable. This
  covers in-flight uploads, because xorbs land before the shard that
  references them.
- **Storage-only default is safe:** files are never collected unless the
  operator enables pin-based retention for that tenant. An integrator who
  never pins loses nothing. *(builder-check call)*
- `openxet gc --dry-run` reports what would be deleted. Every deletion is
  logged. Branch and commit deletion in the Hub layer only drops references;
  data goes only via GC.

### 5.7 Metering

Logical bytes (what users uploaded) and physical bytes (what dedup actually
stored) are tracked per scope and per tenant. They are exposed via the admin
API and metrics, because platforms need them for quotas and billing.
*(builder-check call)*

### 5.8 Scale targets (initial; validated by benchmarks, revised with data)

- **Database topology for v1** *(builder-check call)*: openxet nodes scale
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
- **Ingest:** a single node saturates 10 GbE on xorb upload; throughput
  scales roughly linearly with nodes.
- **Time to first upload:** under 2 minutes from a clean machine, measured on
  every release.

## 6. Hub-compatibility layer (v1 surface)

Only what `huggingface_hub` / `hf` need for the workflows below. Exact
request and response shapes come from `huggingface_hub` source and recorded
fixtures, pinned by the compatibility suite.

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
- `POST /api/repos/create`, `POST /api/repos/delete`
- `GET /api/{type}s/{id}[/revision/{rev}]`
- `GET /api/{type}s/{id}/refs`, `GET /api/{type}s/{id}/commits/{rev}`
- `GET /api/{type}s/{id}/tree/{rev}/{path}`, `POST /api/{type}s/{id}/paths-info/{rev}`
- `POST /api/{type}s/{id}/preupload/{rev}`
- `GET /api/{type}s/{id}/xet-{read|write}-token/{rev}`
- `POST /api/{type}s/{id}/commit/{rev}` (NDJSON)
- branch and tag create/delete
- `HEAD|GET /{[datasets/]}{id}/resolve/{rev}/{path}`

**Model:**
- Repo types `model` and `dataset` in v1.
- Commits are immutable objects with 40-hex ids, a tree of
  path → (Xet file hash | small inline blob), and parent links.
- Refs are branches and tags with compare-and-swap updates, so concurrent
  commits are safe.
- No git wire protocol.

**Small files:** `preupload` decides whether small "regular" files go inline
in the commit (stored as content-addressed blobs) or through Xet. The
threshold is the builder's call, informed by benchmarks.

**Explicitly not implemented:** model cards, search, likes, discussions,
Spaces, inference, collections, org settings. Unimplemented endpoints
return a clear `501` that names the endpoint, never a confusing `404`.

## 7. Operability and user experience

- **Zero-config dev mode:** `openxet serve` runs with SQLite and a local
  directory, prints a static token and the exact `export HF_ENDPOINT=… HF_TOKEN=…`
  lines, and works immediately.
- **Config:** one TOML file, with every key overridable as `OPENXET_*`.
  `openxet config check` validates and explains.
- **Errors are actionable:** they say what happened and what to do next.
- **Observability:**
  - Prometheus `/metrics` and OpenTelemetry traces.
  - Structured logs. Tokens are never logged.
  - `/healthz`, `/readyz`.
- **Admin CLI:** `reindex`, `gc [--dry-run]`, `verify` (integrity scan),
  `token mint`, `migrate`.
- **Feedback loop:**
  - GitHub Discussions, issue templates and a public roadmap.
  - A docs feedback link.
  - **No telemetry**, by default or ever without explicit opt-in.

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
  (xorb, shard, NDJSON commit).
- **Compatibility suite**, the most important test. CI runs real `hf_xet`,
  `huggingface_hub`/`hf` CLI and `huggingface.js` (pinned and latest) against
  openxet, across a matrix:
  - SQLite with local disk
  - Postgres with MinIO

  It covers upload, re-upload dedup, range download, branch workflows and
  token refresh. A nightly job against latest upstream catches protocol drift
  early.
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
  - `docker compose` quickstart with MinIO
  - Terraform examples for AWS, GCP and Azure
  - library crates on crates.io for embedding the storage core
- **After v1:** one-click templates (Railway, Fly, Render), cloud marketplace
  listings, a Nix package, an operator for Kubernetes if users ask.
- **Discoverability:**
  - listing in awesome-selfhosted
  - a "self-host Xet" guide that ranks for the obvious queries
  - published benchmarks
  - a clear "works with the official HF tools" message

## 10. After v1 (designed for, not built)

- **Git/LFS integration:** an LFS endpoint so Git repos on GitHub, GitLab or
  Gitea store large files in openxet via `git-xet`.
- **`bucket` repo type** (mutable, non-versioned), mirroring HF Storage Buckets.
- **Our own client/CLI:** sync, mount (FUSE), works without the Hub layer,
  speaks storage-only mode natively. The CAS API and protocol crates are the
  contract that makes this possible.
- **Cache tier:** a regional read-through cache node.
- **Pull-through mirror of huggingface.co:** cache public HF repos inside the
  operator's network.

## 11. Spikes to run first (verify, don't assume)

- **S1:** `hf_xet` downloads correctly when v2 reconstruction returns native S3
  presigned URLs with one range per fetch entry.
- **S2:** `hf_xet` and `huggingface_hub` accept `http://` CAS and Hub endpoints
  for local dev, or what TLS setup the quickstart needs.
- **S3:** the exact `whoami-v2`, `preupload`, `commit` and `tree` response
  shapes `huggingface_hub` requires, captured as fixtures.
- **S4:** whether DuckDB `hf://` (and polars or fsspec) can target a custom
  endpoint; if not, document the plain-HTTPS URL route.
- **S5:** whether `xet-core-structures` is usable server-side, or which pieces
  to vendor or reimplement, and its versioning on crates.io.
- **S6:** the shard upload verification rules (spec "shard" page) implemented
  against the reference files.

**Client version floor** *(builder-check call)*: the oldest `huggingface_hub`,
`hf_xet` and `huggingface.js` versions that use the v2 reconstruction and
current token flow. These are pinned in M0 from spike results, stated in the
compatibility matrix, and tested as the "oldest" leg of the suite.

## 12. Milestones (v1 = all of them)

Release cut *(builder-check call)*: M0–M3 ship as a public alpha (`0.x`) to
start the feedback loop early; M4–M5 complete `1.0`. The work is built by
the user with coding agents, so the plan sequences milestones but sets no
calendar dates.


- **M0:**
  - repo, `AGENTS.md`, CI skeleton, ADR template
  - spikes S1–S6
- **M1:** CAS core on SQLite with local disk and static tokens.
  - Storage-only mode passes `hf_xet` upload and download, plus the golden
    vectors.
- **M2:** Hub-compat layer.
  - `hf auth login`, `hf upload`, `hf download` and listing work unmodified.
- **M3:** production backends.
  - Postgres; S3, GCS and Azure; presigned downloads.
  - OIDC and webhook auth.
  - A multi-node scale test.
- **M4:** operational completeness.
  - GC, `reindex` from bucket, `verify`.
  - Metrics, tracing and HTTP read gateway.
- **M5:** distribution and docs.
  - Release pipeline, images, Helm, compose, docs site, benchmarks.
  - Time-to-first-upload check.

## 13. Left to the builder

- Exact crate layout and names, HTTP framework, DB access library, token
  format (JWT/PASETO), signing algorithm.
- Small-file threshold, cache design, partitioning scheme details.
- How global-dedup response shards are assembled (reuse uploaded shards or
  synthesize), within the spec.
- Docs site tooling.

## 14. Name

The project starts as **openxet** *(decided by the user)*. Because it uses the
"Xet" name, there is some trademark risk. Keep brand-specific strings in a few
places (binary name, image name, docs domain) so a rename stays cheap if it
is ever needed.
