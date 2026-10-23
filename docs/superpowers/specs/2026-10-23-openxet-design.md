# openxet — Design

Date: 2026-10-23 · Status: approved; reconciled with the M0 spike results

## 1. Intent

Hugging Face's Xet storage gives chunk-level deduplicated storage for large,
versioned files. Models, datasets and checkpoints are the common examples, not
the definition. The client side is open source
(`huggingface/xet-core`, Apache-2.0) and the protocol is published
(Xet Protocol Specification v1.1.0, https://huggingface.co/docs/xet). The server
is not. Nobody can run Xet on their own infrastructure.

**openxet is the open-source, self-hostable server for the Xet protocol.** The
official, unmodified Hugging Face clients (`hf_xet`, `huggingface_hub` /
`hf` CLI, `git-xet`, `@huggingface/hub`) work against it. Git repos can live
on any Git host (GitHub, GitLab, Gitea and others) and keep their large files
in openxet (§6.1). It runs on storage
and identity systems people already own.

It is not a Hugging Face copy. It is plug-and-play infrastructure: small surface,
clear seams, no opinions forced on the operator.

### Who it is for

- Organizations that cannot or will not put data on huggingface.co (regulated,
  sovereign, air-gapped, cost-sensitive) but want HF-style workflows.
- Teams who today keep large files in MinIO + Git LFS / DVC and get no
  cross-version deduplication.
- Platforms that host other people's large files, such as models and
  datasets (aqora is a likely first user), and want Xet storage under their
  own product, with their own naming, permissions and UI.

### What success looks like

- From zero to a working `hf upload` against openxet in under two minutes:
  one command, one environment variable.
- Uploading a slightly changed 10 GB checkpoint transfers megabytes, not
  gigabytes.
- It scales by adding nodes, with no redesign, from a laptop to petabytes.
- Operators recommend it because it is boring to run.
- People who don't want to run anything get the same thing, done perfectly, on
  **openxet.com** (§15).

## 1a. User value model *(decided by the user)*

What users get from openxet, and how each value is measured. The targets
are initial, like §5.8's, and revised with data. Each is checked by the
benchmark suite or the compatibility suite (§8), and the measured numbers
are published with every release. "Checked in" says where: targets that
fit a CI runner gate every PR, and those that need hosted infrastructure,
10 GbE hardware or an external score (OpenSSF Scorecard) run on the
release benchmark rig or as a release check.

| Dimension | What it means to the user | How openxet delivers it | Target | Checked in |
|---|---|---|---|---|
| Speed | Uploads, downloads and listings never wait on openxet | Direct presigned downloads (ADR 0002), stateless nodes, a fast index, the cache tier (§10.5) | At 1,000 requests/s per node, against an index of 10⁷ files and 10⁷ xorbs: reconstruction p99 < 50 ms for files up to 100k terms; token, head and resolve p99 < 10 ms (§5.8). ≥ 9 Gbit/s sustained xorb ingest on one node | Release rig |
| Dedup and efficiency | Only changed bytes move or get stored | Client-side chunking with global dedup; server-side chunking for every other client (§10.3); our client's Git LFS download agent (§6.1) | A 1 MB edit of a 10 GB file transfers ≤ 2 MB up with stock `hf_xet`, also from a second machine with an empty cache; ≤ 2 MB down with our client when the previous revision is in the local Xet chunk cache (§10.8 can seed it from an existing LFS checkout); storage overhead over unique compressed data ≤ 5% | Per-PR CI |
| Compatibility | The tools they already use just work, in both directions with huggingface.co | Byte-exact formats on `xet-core`'s crates (ADR 0001); the Hub and LFS surfaces of §6 | 100% of the compatibility suite passes for the oldest supported, pinned and latest clients; zero divergence on the golden vectors | Per-PR CI; latest clients also nightly |
| Reliability and integrity | Acknowledged data is never lost, corrupted or silently dropped | The bucket is the source of truth (§5.3); every xorb and shard is verified on ingest (§5.9); deletion only through GC (§5.6) | Zero acknowledged writes lost under SIGKILL fault injection; `reindex` from the bucket alone restores every file, with refs at most 60 s stale (RPO); `verify` has zero false positives and exits non-zero on real corruption | Per-PR CI |
| Time to value | Working in minutes, with nothing to learn first | One binary, zero-config dev mode (§7), copy-ready commands | First `hf upload` of a ≥ 1 GiB file < 2 min from a clean machine self-hosted, < 60 s from the landing page on openxet.com (§15) | Release check (rig for self-hosted, the hosted service for openxet.com) |
| Control and sovereignty | Their bucket, region and identity system; no lock-in | Storage and auth adapters (§5.3, §5.4); pools and residency (§10.7); bring-your-own bucket on openxet.com | No byte crosses a pool boundary (residency test); data written by openxet reads with stock clients and huggingface.co tooling, and the reverse | Per-PR CI |
| Security and privacy | Only the right people read or write, and nothing leaks | The requirements of §5.9, each with its test | Every §5.9 test passes; OpenSSF Scorecard ≥ 9 (§16) | Per-PR CI; Scorecard as a release check |
| Reproducibility | A revision always returns the same bytes | Content-addressed files and Git commit OIDs; data keyed by commit is immutable | A download by commit OID is byte-identical across nodes, cache states and consecutive releases (sha256-checked in the upgrade tests) | Release check (upgrade tests) |
| Cost transparency | Users see what they store, what dedup saved and what it costs | Logical and physical metering per scope and tenant (§5.7), shown in the web UI and `--json` | The sum of metered physical bytes across tenants is within 1% of bucket bytes, excluding objects pending GC; per-tenant physical bytes are exact for dedicated-bucket and `per-scope` tenants; savings shown on every repo page | Per-PR CI |
| Agent UX | Agents use every surface without a human in the loop | §7.1: `--json` with versioned schemas, stable exit codes, structured errors with a hint, MCP | The agent leg passes with zero prompts, zero unparseable outputs and zero schema breaks | Per-PR CI |

**Where openxet beats huggingface.co:**
- Dedup for every client. Our client fetches only missing chunks on
  `git clone` / `git pull` (§6.1), and server-side chunking dedups uploads
  from clients that don't speak Xet (§10.3).
- Your own bucket and region, plus caches near your GPUs (§10.5, §10.7).
- Mount and stream any revision (§10.8).
- Savings shown to the user, in bytes and money (§10.10, §15).

**Explicit trade-offs:**
- **Dedup vs read fragmentation.** The more a file dedups against others,
  the more xorbs a read touches. Stock clients already skip dedup of very
  short ranges (`xet-data` 1.7.0, `deduplication/defrag_prevention.rs`),
  the cache tier offsets the rest, and the benchmark suite tracks ranges
  per GB read.
- **Global dedup vs privacy.** Global dedup reveals whether a chunk
  already exists, even through HMAC-keyed answers. `per-scope` dedup
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
   and recovery from the bucket alone.

### Non-goals (v1)

- No HF-style product features: model cards, discussions, likes, search,
  running Spaces or inference. The only web UI is minimal self-service
  (§10.10).
- No user accounts, passwords or user database. openxet never owns users.
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
- Silent incompatibility: anything that makes data written by openxet unreadable
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
  back to v1 on 404), `HEAD /v1/files/{file_id}`, `GET
  /v2/file-chunk-hashes/{file_id}` (delta uploads). Hash-to-hex conversion
  reverses bytes within each 8-byte group, which is a classic source of
  bugs.
- **What stock `hf_xet` 1.7.0 actually sends** (S6, source and observed):
  - Global dedup queries go to `GET /v1/chunks/default/{hash}`, not the
    spec's `default-merkledb`. openxet serves both.
  - It sends `POST /v1/telemetry`. openxet accepts it and drops the body:
    nothing is stored or forwarded (no telemetry, §7).
  - It tries `POST /v2/shards` first and falls back to `/v1/shards` on 404.
  - An empty file is a file entry with 0 segments, an all-zero file hash
    and an all-zero sha256.
  - The wasm build uploads shards **with** lookup tables and a footer,
    although the spec says the footer must be omitted. openxet accepts a
    consistent footer and discards the tail (S6 rule 3).
- **Shard sha256** is stored in the hash layout: the raw bytes are the
  digest with each 8-byte group reversed. It is client-asserted; the
  server can't check it without the file's bytes, so the sha256 → Xet
  hash mapping is scoped to the repo whose writer uploaded it (§5.4).
- **Shard verification** (S6): eleven rules, all checkable from the chunk
  hashes and lengths recorded at xorb ingest, never by reading a xorb
  back. `verify_shard` caps the referenced chunks per shard
  (`MAX_REFERENCED_CHUNKS`, default 2^24, about 1 TiB of logical data) and
  rejects shards that reference missing xorbs. Reference and real
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

  openxet answers `/v1/xorb-grants/*` with `404` until it supports grants,
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
  on those three is unverified. openxet therefore emits **one range per
  fetch entry, each with its own range-bound URL**. Stock `hf_xet`
  downloads correctly from such SigV4 presigned URLs on MinIO.
- **Global dedup:** only "eligible" chunks are queryable: the first chunk of
  each file, plus chunks whose hash's last 8 bytes (LE u64) % 1024 == 0.
  Responses are shards whose chunk hashes are HMAC-keyed. This keeps the
  global dedup index about 1/1000th of the chunk count, which is what makes
  scale tractable.
- `huggingface_hub` now knows a `bucket` repo type (HF Storage Buckets:
  mutable, no git revisions). openxet serves it in v1 (§10.4).
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
- Our own client is in v1, and must stay 100% compatible with the official
  tools and huggingface.co (§10.8).
- `git-xet` and Git over SSH are core, v1 (§6.1, §10.1).
- Build straight to 1.0 with no alpha pause (§12).
- Be ambitious: nothing is deferred for convenience. That brings in
  server-side chunking, all repo types including `bucket`, the cache tier, the
  HF pull-through mirror, multiple storage pools and residency, and the full
  distribution list (§9, §10).
- openxet issues revocable access tokens (and SSH keys) bound to external
  identities (§5.4).
- The name is openxet.

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

**The bucket is the source of truth.** Xorbs, uploaded shards, and Hub-layer
commit objects are written to the bucket. The database is an index that
`openxet reindex` can rebuild from the bucket. Losing the database must never
lose data.

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
  ADR 0002). Purge also evicts the file's ranges from the cache tier. The
  bytes then leave storage through GC (§5.6), once no other file
  references their xorbs. Removing a reader from the scope revokes their access once their
  current CAS token expires.

**Global-dedup visibility** is configurable:
- `global`, the default, follows HF's model with HMAC-protected responses.
- `per-scope` is for multi-tenant hosting, where revealing whether a chunk
  already exists across tenants is unacceptable.

**Existence checks in `per-scope` mode** answer per scope too, or they
would reveal what dedup answers hide. Every check that says whether
something exists is filtered by the caller's scope:
- `HEAD /v1/xorbs/…`, `HEAD /v1/files/…` and dedup lookups
  (`GET /v1/chunks/…`)
- the xorb lookups of shard verification (`verify_shard`'s
  `XorbChunkLookup`)

A xorb counts as registered to a scope once it was uploaded under that
scope or a file registered to the scope references it. A xorb or file not
registered to the caller's scope answers exactly as if it were absent
(`404`, no dedup match, `UnknownXorb`), so the client uploads it, which
registers it to its scope. Storage stays shared: the upload of a xorb
already stored writes nothing new. Test:
`per_scope_existence_checks_hide_other_tenants`.

### 5.5 Data paths

- **Upload:** client → CAS (`POST xorbs` streamed and verified into the object
  store; `POST shards` verified and registered). This scales with node count.
  Today every xorb byte goes through openxet (§3).
  - **Xorb upload grants** are a tracked future item, not an ADR yet, since
    no stable client uses them (§3). When clients ship them, openxet
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
  - **(b)** via the transfer role, with openxet-signed range URLs (also one
    range per entry, so no `multipart/byteranges`), for local disk, caching
    setups and any pool that fails the range-binding probe.

  Mode (a) is gated per deployment and pool by a **range-binding
  enforcement probe**, not by a backend list (ADR 0002). It runs at
  startup and in `openxet doctor` / `openxet config check`. openxet
  presigns a GET for one range of a probe object and requires all three:
  the signed range returns `206`, a different range returns `403`, and no
  `Range` header returns a `4xx`. Only then does the pool serve direct
  URLs; otherwise it falls back to (b) and logs why. This applies to every
  backend (S3, R2, Garage, MinIO, and GCS or Azure once their signers can
  bind a range). CI runs the probe against MinIO. URL lifetime defaults to
  60 min with a 30 min minimum, as derived in ADR 0002. v1 reconstruction
  is served too, for older clients.
- **Plain HTTP read gateway:** `GET /{repo}/resolve/{rev}/{path}`, when the
  redirect is followed, streams the reconstructed file with HTTP `Range`
  support. This makes openxet readable by anything that speaks HTTP (curl,
  browsers, pandas, polars, DuckDB via the plain resolve URL) or `hf://`
  through fsspec `HfFileSystem`. DuckDB and polars hard-code
  huggingface.co in their own `hf://` handlers (spike S4,
  `docs/research/S4-analytics-clients.md`), so they read openxet through
  `https://{host}/{repo_type}/{repo}/resolve/{rev}/{path}`. The gateway
  must pass the conformance checklist in that note (HEAD, all three
  `Range` forms, `416`, `ETag`/`If-Range`, redirects, auth); private
  repos need a DuckDB `http` secret, and polars needs an fsspec file
  object. Private-CA https works in DuckDB but not in polars on macOS
  (observed; `docs/research/S4-analytics-clients.md`). It is served by the transfer role so
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
- **Ingest:** a single node sustains ≥ 9 Gbit/s of xorb upload; throughput
  scales roughly linearly with nodes.
- **Time to first upload:** under 2 minutes from a clean machine, measured on
  every release.

### 5.9 Security requirements

Each requirement is a release blocker and has a named test. The tests are
planned for the milestone that builds the surface (M1 for CAS, M2 for Hub
and Git, later for caches and the mirror); a surface doesn't ship without
its test.

| # | Requirement | Tests |
|---|---|---|
| 1 | **No unauthenticated reads when auth is on.** Every data URL (reconstruction, xorb ranges, the HTTP gateway, LFS `href`s) needs a credential, or is a signed URL bound to one range and a short lifetime (ADR 0002). | `no_unauthenticated_reads` walks every route and every URL openxet emits without a credential; `signed_url_bound_to_range` |
| 2 | **Credentials are repo-scoped.** CAS tokens, signed URLs and capability `href`s are scoped to one repo; user credentials are authorized per repo on every Hub and CAS route. A foreign repo answers `403`, or `404` where the caller can't read it, never revealing a private repo's existence. Repo creation is authorized per namespace (§5.4). | `credential_scoped_to_repo`, a matrix of every route × read, write and create with another repo's credential; `repo_create_authorized_per_namespace` |
| 3 | **Caches and proxies are keyed by the caller's credential** for anything that depends on authorization. They never share tokens or authorized responses, and they fail closed. The only exception is stale serving of content whose last upstream check was public and ungated (§10.6). | `cache_keyed_by_caller`, `cache_fails_closed` |
| 4 | **Dedup answers reveal no raw chunk hashes.** They are HMAC-keyed shards with a footer, and per-tenant dedup (`per-scope`, §5.4) is available. In `per-scope` mode every existence check (`HEAD` of xorbs and files, dedup lookups, shard xorb lookups) answers as absent for anything not registered to the caller's scope. | `dedup_response_hmac_keyed`, `per_scope_dedup_isolates_tenants`, `per_scope_existence_checks_hide_other_tenants` |
| 5 | **Uploads are verified.** Xorbs are re-hashed on ingest. Shards are checked against the stored xorb chunk hashes by `verify_shard`, with its work cap (S6), and a shard that references a missing xorb is rejected. | the S6 rule tests; `shard_with_missing_xorb_rejected`, `shard_verify_work_capped` |
| 6 | **Credentials are never echoed or logged.** LFS download actions carry no CAS token (ADR 0003), no action `header` or response carries the client's own credential, and fixtures and docs pass the redaction guard. | `lfs_download_action_has_no_token`, `responses_never_echo_credential`, `credentials_never_logged` (scans all logs of a suite run), `test_fixture_redaction` |
| 7 | **No silent no-op success.** Every acknowledged write has its effect, and survives a SIGKILL right after the acknowledgement. | `acknowledged_write_is_visible`, `acked_write_survives_sigkill` |
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
- `HEAD|GET /{[datasets/]}{id}/resolve/{rev}/{path}`

**Model:**
- Repo types `model`, `dataset`, `space`, `kernel` and `bucket` (§10.4).
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

**Small files:** `preupload` decides whether small "regular" files go inline
in the commit (stored as content-addressed blobs) or through Xet. The
threshold is the builder's call, informed by benchmarks. Answering
`"uploadMode": "lfs"` is the server's only lever; `hf_xet` then takes the
Xet path (S3). The compatibility test for this (competitive-matrix lesson
1) uploads a file of at least 1 GiB whose name matches no `.gitattributes`
pattern, so it proves openxet routes by size, not by pattern.

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
files through openxet with chunk-level dedup.

How it works (verified in `xet-core/git_xet` source, 2026-10-23):
- `git-lfs` sends the Git LFS Batch API request with its registered transfer
  agents.
- openxet answers uploads with transfer `xet`. Each object's `action`
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
- Auth uses Git's normal credential flow: an openxet access token as the
  HTTP password.
- LFS repos are scopes like Hub repos. Creating one is an authorizer
  `create` decision, so the first push can create it.
- **Downloads always answer `basic`.** `git-xet` 0.2.1 implements uploads
  only ("custom transfer for download is not implemented yet"). But
  `git xet install` registers it for both directions, so `git-lfs` offers
  `xet` in download batches. A server that chooses `xet` there breaks
  `git lfs pull` with no fallback (S7, observed). openxet therefore answers
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
     `openxet setup git` registers it under its own transfer name (not
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
    (§10.3), so the data is still deduplicated in storage. The response
    suggests installing `git-xet` for faster uploads.
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
  `openxet config check` validates and explains.
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

openxet is built for people **and** AI agents, and for the two working
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
  openxet, across a matrix:
  - SQLite with local disk
  - Postgres with MinIO

  It covers upload, re-upload dedup, range download, branch workflows and
  token refresh. A "third-party Xet client" leg replays the expectations of
  jj-xet's `tests/openxet.rs` mock against openxet (ADR 0005). A nightly job
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
    - openxet presigns with a store endpoint the client can reach, not the
      compose-internal service name
  - Terraform examples for AWS, GCP and Azure
  - library crates on crates.io for embedding the storage core
  - **The `git` role needs `git` ≥ 2.37 on `PATH`** (ADR 0004).
    - openxet runs git as a separate program. That is aggregation, so
      openxet stays Apache-2.0 (inferred, not legal advice).
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

- openxet runs an SSH endpoint, enabled by the `ssh` role. Identities are SSH
  public keys bound to an external identity, the same way access tokens are
  (§5.4). Keys are registered via `openxet login` or the admin API. No
  passwords and no users are stored.
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
    openxet-hosted repos over SSH, `openxet setup git` sets
    `lfs.<url>.sshtransfer=never` when `git-xet` is installed, which sends
    those users through `git-lfs-authenticate` plus the HTTPS batch, where
    `xet` is offered (ADR 0003).
- Git repos whose *remote* is on another host over SSH (e.g.
  `git@github.com:…`) work as long as `lfs.url` points at openxet and
  `openxet setup git` has written the `access` key (§10.2).

### 10.2 `git-xet` with third-party Git hosts

When `lfs.url` points at openxet and the remote lives elsewhere, openxet's
policy is: **capability routes never authenticate by header, and `openxet
setup git` configures access explicitly**
([ADR 0003](../../adr/0003-git-xet-credential-strategy.md), S7). All three
parts are v1:
- **Server side:**
  - The `href` refresh route openxet returns is a short-lived, signed,
    single-repo capability URL, so the refresh needs no credential.
  - CAS tokens issued through the LFS path last for the transfer session.
  - openxet never authenticates by, logs or echoes an `Authorization`
    header on that route.
- **Client setup:** `openxet setup git` writes the clone's git config:
  - `lfs.url`
  - `lfs.<endpoint derived from the remote>.access=none`, so `git-xet`
    calls the capability `href` without a credential; the `href` alone
    completes the refresh (S7, observed)
  - the openxet token in the credential helper under the openxet host, for
    `git-lfs`'s batch

  The `access` key must be in git config, because `git-xet` doesn't read
  `.lfsconfig`.
- **Upstream:** contribute a lazy credential lookup, an optional
  `expires_in` and download support to `git-xet` in `huggingface/xet-core`,
  coordinated with Hugging Face.

### 10.3 Server-side chunking for non-Xet clients

Plain `git-lfs` (basic transfer, HTTPS or SSH), older `huggingface_hub`
without `hf_xet`, and any LFS client can **upload**. openxet chunks,
deduplicates and writes xorbs and shards on ingest, using the same `xet-core`
chunking, so the hashes are identical to client-side uploads.
- Users get dedup in storage even if their client never heard of Xet. They
  don't get the bandwidth savings; the docs say so.
- Ingest scales with the `cas` role. CPU cost is benchmarked.
- `preupload` and the LFS batch endpoint offer `basic` and `multipart`
  alongside `xet`.

### 10.4 More repo types

`model`, `dataset`, `space` (files only; nothing is run) and `kernel`
behave the same as Hub repos.

**`bucket`** (mutable, non-versioned, mirroring HF Storage Buckets): paths map
to Xet files with last-writer-wins and optional object versioning. Bucket
tokens are not revision-scoped, matching `huggingface_hub`.

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
  sizes. Clients only ever see openxet URLs and tokens.
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
  - Public, ungated repos: the openxet authorizer.
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

`openxet` ships a client: a CLI and a Rust library, with Python bindings.
It is better to use than the stock tools and never incompatible with them.

**The compatibility contract is non-negotiable:**
- Byte-identical chunking, hashes, xorbs and shards. The client builds on
  `xet-core`'s `xet-data` / `xet-core-structures` rather than reimplementing
  them. Any divergence is a release-blocking bug.
- It works against **huggingface.co itself** as well as openxet, via the same
  Hub and CAS APIs. Data uploaded by our client is readable by `hf`,
  `hf_xet`, `git-xet` and `huggingface.js`, and vice versa.
- It reads and writes the standard HF cache layout (`HF_HOME`, `HF_XET_CACHE`)
  and honours `HF_ENDPOINT` / `HF_TOKEN`, so it coexists with `hf` on the
  same machine.
- CI proves it in both directions: our client against openxet and against
  huggingface.co (nightly, using a dedicated test org), plus cross-tool round
  trips (upload with ours, download with `hf`, and the reverse).

**What makes it worth installing** (the extra mile for users):
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
  after switching to openxet fetches only changed chunks (§1a).
- **`login`:** the OIDC device/browser flow that issues openxet access tokens
  and SSH key registration (§5.4, §10.1).
- **`setup git`:** one command configures `lfs.url`, credentials and
  `git-xet` for a repo (§10.2). It also registers `openxet` as the Git LFS
  transfer agent, so `git clone` / `git pull` download only missing chunks
  (§6.1). It works against openxet and huggingface.co alike.
- **Diagnostics:** `doctor` checks connectivity, auth, clock skew and
  versions, then says what to fix.

**Distribution:** the same channels as the server (§9), plus PyPI (Python
bindings), npm via WASM if the bindings allow it, `winget`, and Windows
binaries. The client must run on Windows; the server need not.

### 10.9 Git hosting *(decided by the user: v1)*

`git clone https://openxet.example/org/model` (and `git@openxet.example:org/model`)
works as it does on huggingface.co. This sits alongside §6.1, where the repo
lives elsewhere and openxet is only its LFS store; both modes are supported.

**Behaviour:**
- **Protocols:** Git smart HTTP and SSH, both `upload-pack` and
  `receive-pack`, protocol v2 where clients support it. The LFS endpoint is
  implicit at `{repo}.git/info/lfs`, so `git-xet` and `git-lfs` work with no
  configuration. §10.2 doesn't apply, because the Git remote *is*
  openxet.
- **Large files land in Xet automatically:**
  - Pushes with `git-xet` take the Xet path.
  - Plain `git-lfs` pushes go through server-side chunking (§10.3).
  - Large blobs pushed **without** LFS are kept as ordinary Git objects. They
    don't silently corrupt history. openxet reports them and offers
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
  - openxet answers the protocol v2 capability advertisement and `ls-refs`
    in-process.
  - openxet enforces reachability for every upload-pack want, v0 to v2
    (R22), because git's v2 `upload-pack` serves any object in the odb.
    It interposes on the request stream and checks wants before
    forwarding them to git.
    - Ref tips pass.
    - All other wants share one gix walk per request, within a configured
      commit, time and concurrency budget.
    - Anything not found is refused (fail closed).
  - Objects are never shared across repos.
  - Ref updates go through a hardened `proc-receive` hook back into
    openxet:
    - an inherited socketpair fd (HTTP and SSH) and a single-use token
      bound to repo, principal and request
    - pack paths confined to the request's snapshot
    - policy re-checked in openxet
    - a sanitised git environment
  - openxet then publishes the pushed pack to the bucket as `pending`.
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
    the refusal hints `git push --no-thin`, which sends no thin bases.
  - The v2 interposer allows only `ls-refs` and `fetch`. git 2.37–2.44
    serves `object-info` for any object, so openxet refuses it, along
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
- Browse repos, branches, commits and files; download files; copy clone,
  `hf` and `openxet` commands.
- See storage usage per repo and tenant: logical bytes, stored bytes, and the
  dedup savings ratio.
- Admin view (for operator-designated groups): health, GC status and dry-run
  reports, pools, mirror status.

**Explicitly not included:** model cards, README rendering beyond plain
text, discussions, social features, previews of model or dataset contents.

Easy to restyle and disable; platforms like aqora will typically run their
own UI and leave this role off.

## 11. Spikes (verify, don't assume)

All nine M0 spikes are answered. Each note records versions, method and
what was observed versus inferred.

- **S1** *(answered: yes, on MinIO;
  [S1–S2](../../research/S1-S2-direct-downloads.md), ADR 0002)*: `hf_xet`
  downloads correctly when v2 reconstruction returns native S3 presigned
  URLs with one range per fetch entry.
- **S2** *(answered: `http://` works for Hub, CAS and store; a private CA
  works via `SSL_CERT_FILE` on Linux but not for `hf_xet` on macOS;
  [S1–S2](../../research/S1-S2-direct-downloads.md))*: `hf_xet` and
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
- **S6** *(answered: eleven rules, all checkable from per-xorb chunk hashes
  and lengths stored at xorb ingest, with a work cap per shard; reference
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
  every URL and token re-issued by openxet; HF reconstructions can be
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
  snapshot repos, over openxet-owned database refs and bucket packs, with
  gix in-process off the wire path; gix 0.90 has no upload-pack or
  receive-pack server, no delta compression and no bitmaps, and cost 16x
  git's CPU to serve a 10k-commit clone; database CAS gave exactly one
  winner in 60/60 concurrent-push rounds across two nodes; JGit DFS,
  go-git, libgit2, Gitaly and Mononoke were assessed and rejected;
  [S9](../../research/S9-git-engine.md), ADR 0004)*: Git hosting engine:
  gitoxide server-side capabilities (upload-pack/receive-pack, protocol
  v2) against the `git` binary on a cached bare repo. Measure throughput
  and latency, and check that bucket-backed packs keep nodes stateless.

**Client version floor** *(builder-check call)*: the oldest `huggingface_hub`,
`hf_xet` and `huggingface.js` versions that use the v2 reconstruction and
current token flow. They are stated in the compatibility matrix and tested
as the "oldest" leg of the suite. M0 did not pin them; the M1 plan does.

## 12. Milestones (v1 = all of them)

Release cut *(decided by the user)*: build straight through M0–M7 to `1.0`,
with no pause between milestones to wait for outside feedback. Real-world
feedback takes time to arrive, so waiting on it would only stall the work.
Until `1.0`, the compatibility suite, benchmarks and the measured
time-to-first-upload stand in for users. The repo is public while it is
built, so anyone can try it early, but no milestone waits on that. The work
is built by the user with coding agents, so the plan sequences milestones
but sets no calendar dates.


- **M0:**
  - repo, `AGENTS.md`, CI skeleton, ADR template
  - spikes S1–S9
- **M1:** CAS core on SQLite with local disk and static tokens.
  - Storage-only mode passes `hf_xet` upload and download, plus the golden
    vectors.
  - The stock-client paths of §3: `/v1/chunks/default/{hash}`,
    `/v1/telemetry` dropped, `/v1/xorb-grants/*` answering `404`.
  - Fuzz targets for the xorb and shard parsers.
  - The M1 rows of the security tests (§5.9).
- **M2:** Hub-compat layer and Git hosting.
  - `hf auth login`, `hf upload`, `hf download` and listing work unmodified.
  - `git clone` / `git push` over HTTPS against openxet-hosted repos, sharing
    one history with Hub API commits (§10.9).
  - The Git LFS endpoint works: `git push` / `git clone` with `git-xet`
    against a Gitea-hosted repo, and `basic` download for clients without
    `git-xet`.
  - The pack-lifecycle prototype and the ADR 0004 test checklist.
  - `openxet purge <file-hash>` (§5.4).
- **M3:** production backends.
  - Postgres; S3, GCS and Azure; presigned downloads.
  - OIDC and webhook auth.
  - A multi-node scale test.
- **M4:** operational completeness.
  - GC, `reindex` from bucket, `verify`.
  - Metrics, tracing and HTTP read gateway.
  - Server-side chunking for non-Xet clients.
  - Git over SSH (`git-lfs-authenticate`, `git-lfs-transfer`).
  - The `git-xet` setup of §10.2: the server-side capability route, plus
    the upstream credential-sourcing contribution to `xet-core`.
  - Upstream: Xet download support in `git-xet` (§6.1). This is contributed
    alongside the credential-sourcing change and coordinated with Hugging
    Face before any PR is filed.
- **M5:** reach.
  - `bucket`, `space` and `kernel` repo types.
  - Cache tier.
  - HF pull-through mirror.
  - Multiple storage pools, residency and KMS.
- **M6:** our own client (§10.8).
  - `sync`, `mount`, storage-only mode, `login`, `setup git` (including the Git LFS download agent), `doctor`.
  - Python bindings.
  - The minimal self-service web UI (§10.10).
  - The two-way compatibility suite, including nightly runs against
    huggingface.co.

  The client's core (upload/download on `xet-data`) starts as early as M1,
  because it doubles as our own test driver.
- **M7:** distribution and docs.
  - Release pipeline, images, Helm (operator only if needed), compose,
    one-click templates, Nix, Homebrew, winget, PyPI, marketplace listings.
  - Docs site.
  - Published benchmarks.
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

Prior art found on 2026-10-23: another repository already uses the name
(`ggoggam/openxet`, Apache-2.0, 8 stars). The GitHub organization `openxet`
exists (created 2025-08-15, empty) and belongs to someone else, so our GitHub
organization needs a different handle; that is the user's choice. For now the
repository lives at `github.com/stubbi/openxet` *(decided by the user)*. The user
owns the openxet.com domain.

## 15. Hosted openxet.com *(decided by the user: we must have the perfect hosted experience)*

openxet.com is a first-class product, not an afterthought. "Perfect" is
measured by the same anti-goals as the open-source project: fast, effortless,
and listening. Detailed design is its own brainstorming round (pricing, legal,
operations). These requirements are fixed now because they shape the core:

**Experience:**
- From the landing page to a working `hf upload` in under 60 seconds: sign in
  with GitHub, Google or SSO; a token is shown with copy-ready commands; done.
- A free tier with no time limit, and no card needed to start.
- Every page shows what the user gains: stored vs. uploaded bytes and the dedup
  savings, in money where possible.
- Bring your own bucket as a headline option: we operate openxet, and the data
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
- Hard multi-tenant isolation (per-scope dedup visibility, pools per tenant,
  noisy-neighbour limits).
- Enforced quotas and per-tenant rate limits.
- Metering accurate enough to bill.
- Self-service sign-up via the web UI.
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
| Performance | Fastest Xet server on upload, download, dedup ratio and metadata latency | Public benchmark suite run in CI against competitors on identical hardware; regressions fail CI; results published per release |
| Code quality | Small, readable, reviewed code with no slop | Strict clippy and `forbid(unsafe)`; mutation testing (`cargo-mutants`) on core crates; fuzzing continuously (OSS-Fuzz once eligible); lines of code tracked as a cost, not a goal |
| Architecture | Every decision recorded and reversible | ADRs for every significant choice; crate boundaries enforced (no cyclic or upward deps, checked in CI); bucket-as-truth and stateless invariants have dedicated tests |
| Security | Secure by default, with a verifiable supply chain | The §5.9 requirements, each with its test; `SECURITY.md` with a response SLA; OpenSSF Scorecard ≥ 9; signed releases with SLSA provenance; reproducible builds; `cargo-deny` + `cargo-vet`; a third-party audit before 1.0 |
| Documentation | Every user finds the answer in under a minute | Diátaxis structure (tutorials, how-tos, reference, explanation); every example runs in CI; reference generated from code (CLI, config, OpenAPI); `llms.txt`; a docs feedback link on every page |
| Governance | Trustworthy beyond one person | `GOVERNANCE.md` (roles, decision process, path to maintainer); every change merged via PR with at least one independent review (human, or an agent reviewer plus human sign-off), enforced by branch protection; an RFC process for big changes; DCO; code of conduct; a public roadmap |
| Community | Contributors feel welcome and succeed | Labelled good-first issues; contributor guide tested by newcomers; first response to issues and PRs within two business days; credit in every release's notes |
| Release engineering | Boring, predictable releases | SemVer; changelog generated from Conventional Commits; release train cadence; upgrade tests between consecutive versions; a stated support window |
| UX (humans and agents) | Best on both (§7.1) | Time-to-first-upload measured per release; the agent leg of the suite; accessibility (WCAG 2.2 AA) checked for the web UI |
| Operations / hosted | openxet.com is the reference deployment (§15) | Public status page and SLO; chaos and failover drills in CI; disaster-recovery restore tested on a schedule |

Bus factor is a real risk while one person leads the project. Governance work
starts at M0, not after 1.0.
