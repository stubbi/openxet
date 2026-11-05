# M3: Production backends, identity, delegated tokens, limits and load testing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

<!-- description: The M3 plan: Postgres and object-store backends with range-bound presigned downloads, OIDC and the webhook authorizer, issued and delegated agent tokens, per-tenant limits and quotas, a migration policy for rolling upgrades, and the load-test harness with its multi-node, soak and key-rotation drills. -->

**Goal:** `openxet serve` runs as N stateless nodes on Postgres and an S3,
GCS or Azure bucket, serves downloads from range-bound presigned URLs
wherever the store proves it binds a range, accepts OIDC identities and an
operator's webhook authorizer, issues revocable access tokens and
narrower delegated tokens for agents, enforces per-tenant rate limits,
quotas and memory shares, survives a rolling upgrade and a key rotation
under load, and proves all of it with a reproducible load harness whose
results are published.

**Architecture:** No new server crate for storage or the index: Postgres
is a second implementation of the existing `Index`, `Repos` and
`GitStore` traits in `openxet-index`, and an `object_store`-backed
`ObjectBucket` is a second `Bucket` in `openxet-storage`, both proven by
backend-agnostic conformance suites that SQLite and `LocalBucket` already
pass. Range-bound presigning is OpenXet's own small signer per store
family (`object_store`'s signer can't bind a `Range` header), gated per
pool by ADR 0002's probe. Identity grows in `openxet-auth` (OIDC, the
webhook authorizer, access and delegated tokens) behind the M2 seams;
limits live in `openxet-http` so the CAS and the Hub share them. A
native load generator (`crates/loadgen`) and an orchestrator with
scenarios, thresholds and a multi-node compose stack (`loadtest/`) are
the M3 test rig.

**Tech Stack:** Rust 1.97 (edition 2024); axum 0.8, tokio; sqlx 0.9 with
the `postgres` feature (Postgres 17 in CI); `object_store` 0.14 (S3, GCS,
Azure); `hmac`/`sha2` (already in `openxet-auth`) for SigV4 and GCS V4
presigning; `jsonwebtoken` 11 on `aws-lc-rs` for OIDC; `governor` 0.10
for rate limits; `arc-swap` 1.9 for hot key sets; `reqwest` 0.13 (rustls,
post-quantum key exchange preferred since #89); proptest, criterion;
pytest + uv with `huggingface_hub` 2.2.0 and `hf_xet` 1.7.0; Docker
Compose with MinIO, Garage, fake-gcs-server, Azurite, Postgres 17 and
HAProxy 3 for CI.

**Spec:** `docs/superpowers/specs/2026-10-23-openxet-design.md` (§5.2,
§5.3, §5.4, §5.5, §5.8, §5.9, §7.1, §12 M3, §15 "What this adds to the
core", §16 Performance and Operations, §17 rulings M1-R1, M1-R8, M1-R24,
M1-R25, M1-R29, M1-R30, M1-R32, M1-R37, M1-R39, M1-R40, M1-R42, M1-R43,
M2-R3), with ADR 0002 (range-bound URLs and the probe; its "Open, owned
by M3" list), ADR 0007 (CAS tokens and key sets), ADR 0011 (per-key `alg`
binding, the rotation drill), ADR 0012 (user credentials, the authorizer
seam), ADR 0013 (where M3 sits) and ADR 0014 (agents can never widen
their own limits). Issues: #43, #47, #48, #50, #54, #55, #57, #65.

## Where M3 sits

- **Order (ADR 0013):** agent workspaces steps 1 and 2 are merged. M3
  comes before workspace steps 4 to 6, whose hosted and multi-tenant
  promises need M3's backends, OIDC and issued tokens; step 6 builds on
  Tasks 13 and 14 here (delegated tokens) and on M4's audit log. Step 3
  (the mount) needs nothing from M3 and may run alongside it.
- **Already shipped:** hybrid post-quantum TLS key exchange in every
  rustls client (#66, commit 5d7fe5e, `crates/openxet/tests/post_quantum_tls.rs`).
  The design's §12 M3 bullet for it is done; Task 23 marks it so.
- **Issue placement:**
  - #43, #47, #48, #55, #57 and #65 are M3 deliverables (Tasks 9, 5, 9,
    19 to 22, 15, 10 and 22).
  - **#54 fits M3** and gets Task 18: memory per request is what limits
    concurrent clients per node, which the multi-node and soak scenarios
    measure, and its owner comment already ties it to #43 and #55. Its
    streaming-ingest half is gated on an upstream check (Task 18 step 1),
    so it cannot block the milestone.
  - **#50** is labelled M8 on GitHub, but design §15 lists it as a core
    addition and its status comment ties it to "the hosted rate limits",
    which are M3's. Task 17 builds the mechanism; the policy is an owner
    decision (see the end of this plan).

## Global Constraints

**Protocol and compatibility**
- Nothing on the wire changes for stock clients except the URLs inside
  reconstruction answers, which become native presigned URLs on a pool
  that passed the probe. Every fetch entry still has exactly one range
  and one URL (ADR 0002 rule 1).
- URL lifetime stays 30 min to 60 min, default 60 min (ADR 0002 rule 5,
  M1-R42), on presigned URLs too.
- `xet-core` crates stay at `=1.7.0`; `xet_core_structures` only in
  `crates/protocol`, `xet_data` and friends only in `crates/client`
  (`scripts/check-client-core-ban.sh`). `crates/loadgen` is not a server
  crate but takes only `openxet-protocol`, never `xet-data`, so it can
  drive request rates the client core would shape.
- New dependencies take their latest stable release (M1-R15). At the time
  of writing: `object_store` 0.14.2, `jsonwebtoken` 11.1.0, `governor`
  0.10.4, `arc-swap` 1.9.3, `testcontainers` 0.28.0 (dev only); check
  `cargo search` when adding, and `cargo deny check` must pass.

**Security (design §5.9, every row a release blocker)**
- Every new credential kind (OIDC, access, delegated) and every new route
  lands in the existing matrices: `no_unauthenticated_reads`,
  `credential_scoped_to_repo`, `responses_never_echo_credential` and
  `credentials_never_logged` gain rows, never exceptions.
- Requirement 3 covers the new caches: the JWKS cache, the webhook
  decision cache, the access-token cache and the delegated-usage cache
  are keyed by the caller's credential or subject and fail closed: a
  cache that can't be refreshed denies, it never allows.
- Requirement 2 for presigned URLs: one xorb, one range, one expiry, and
  only after the caller's scope check (ADR 0002 rule 2). No presigned
  URL ever comes from a pool whose probe hasn't passed in this process.
- ADR 0014 item 3 is a security property: a delegated token can never
  widen its scope, extend its expiry, raise its caps, mint a token or
  approve anything. Its tests go in the security map.
- Secrets (OIDC tokens, access and delegated token secrets, the webhook
  secret, object-store credentials, presigned query strings) are never
  logged or echoed; logs carry route templates only (M1-R20).
- R21/R26: no public text describes or hints at an unfixed third-party
  security weakness. Probe results that show a store failing to bind a
  range are reported as "serves through the transfer role", with the
  check that failed, and nothing more.

**Durability (non-negotiables 2, 3 and 5)**
- The bucket stays the source of truth on every backend: registration
  and ownership records, purge records, and the new token, revocation
  and tenant-limit records reach the bucket before the index and before
  the client is answered.
- Nothing is deleted outside GC. Probe objects and staging objects
  (Task 18) live under reserved prefixes that `list(kind)` never returns
  and that M4's GC sweeps; a probe object is reused, never deleted.
- Any node serves any request. No task may add node-local state that a
  request depends on, except caches that fail closed and the per-node
  rate limiters of M3-R10.

**Process**
- One branch and PR per task (`m3/tN-<slug>`), squash-merged; parallel
  tasks run in their own worktrees, at most the number of concurrent
  cargo builds the machine allows (the owner's parallel-worktree rule).
  A task edits only the files it owns below; shared files (`Cargo.toml`,
  `Cargo.lock`, `docs/SUMMARY.md`, `.github/workflows/ci.yml`,
  `AGENTS.md`, `CHANGELOG.md`, `docs/reference/config.md`,
  `docs/reference/security-tests.md`) are edited by the task that needs
  them and rebased by whoever merges second.
- Commits: `git commit -s`, Conventional subject with a component scope,
  ending with the `Co-Authored-By` trailer of the model that wrote it.
- TDD: RED before GREEN; never weaken a test, the compatibility suite
  least of all. Every new CLI command has `--json` with a versioned
  schema, documented exit codes, `--dry-run` where it changes state, and
  an entry in the agent leg's `JSON_COMMANDS`. Every new route is built
  through the route builder, has `utoipa` annotations (M2-R15) and a
  structured error with a `hint`.
- Migrations are numbered at merge time: a task that adds one takes the
  next free number on `main` when it rebases, in both
  `crates/index/migrations/` and `crates/index/migrations-postgres/`,
  and every one is an expand migration (M3-R6) unless this plan says
  otherwise. None in M3 is a contract migration.
- Tests that need a service (Postgres, an emulator) read its URL from an
  `OPENXET_TEST_*` variable and skip with a printed reason when it is
  unset; CI sets `OPENXET_REQUIRE_SERVICES=1`, under which a missing
  variable is a failure, so no CI job passes by skipping.
- Review gates: a fresh reviewer per task against this plan and the
  spec; every security test named here is in
  `docs/reference/security-tests.md` and
  `scripts/check-security-tests-map.py` passes.

## Review Focus

These are the conditions a user is most likely to hit that the spec
implies but no feature test exercises naturally. Each is pinned by a test
in the task that owns it.

1. **Two nodes registering the same xorb or shard at once on Postgres.**
   SQLite's single writer hid every write race M1 and M2 have. Two
   uploads of one file through two nodes must both succeed (one stores,
   one finds it stored), never a `500` from a unique violation or a
   serialization failure, and the registration sequence keeps M1-R37's
   lowest-`(seq, shard, tenant)` row. Pinned by
   `concurrent_register_same_shard_two_pools` and
   `concurrent_put_xorb_two_pools` (Task 3) and
   `concurrent_hub_commits_two_pools_one_winner` (Task 4).
2. **An S3-compatible store that accepts `If-None-Match: *` but ignores
   it.** Registration records rely on create-if-absent; a store that
   silently overwrites would let two writers' records diverge. The
   server must refuse to start on such a pool, naming the check and the
   setting, never run on it. Pinned by
   `start_refuses_store_without_conditional_create` (Task 6).
3. **Downloading right after uploading, with a read replica.** `hf
   upload` then `hf download` lands on a replica a few hundred
   milliseconds behind. The read must not `404`: a replica miss is read
   again from the primary. Pinned by `replica_miss_reads_primary`
   (Task 8).
4. **The identity provider rotates its signing key, or its JWKS endpoint
   is down.** A token signed with a new `kid` is accepted after one
   refresh; an outage keeps serving with the last keys up to
   `jwks_max_stale`, then answers `503 authn_unavailable` with a hint,
   never accepting an unverifiable token. Pinned by
   `jwks_unknown_kid_refreshes_once` and
   `jwks_outage_serves_cached_then_fails_closed` (Task 11).
5. **A stock client under default limits, and one over them.** A default
   `hf upload` and `hf download` of a 4 GiB file never sees a `429`, a
   quota error or a `503` from limits; a client pushed over its rate gets
   `429` with `Retry-After`, `hf_xet` retries, and the transfer
   completes. Pinned by `test_default_limits_never_throttle_stock_client`
   and `test_rate_limited_client_recovers` (Task 15).

## Decisions this plan makes (rulings, recorded in design §17 by Task 1)

- **M3-R1. Where the code goes.** Postgres implements `Index`, `Repos`
  and `GitStore` in `crates/index/src/postgres.rs` and
  `crates/index/src/postgres/{git,repos,tokens,limits}.rs`, with its
  migrations in `crates/index/migrations-postgres/` (SQLite's stay in
  `crates/index/migrations/`). `ObjectBucket` lives in
  `crates/storage/src/object.rs`, the presigners and the probe in
  `crates/storage/src/presign/`. OIDC, the webhook authorizer, access
  and delegated tokens live in `crates/auth/src/{oidc,webhook,access,delegation}.rs`;
  limits in `crates/http/src/limits.rs`. The native load generator is a
  workspace crate `crates/loadgen` (`publish = false`, binary
  `openxet-loadgen`); scenarios, compose files, thresholds and the
  orchestrator live in `loadtest/`.
- **M3-R2. The Postgres index.** One schema mirroring SQLite's tables
  (hashes as `BYTEA`, times as `BIGINT` Unix seconds). `xorbs`,
  `xorb_layouts`, `xorb_owners`, `file_scopes` and `dedup` are
  `PARTITION BY HASH` on their hash column with 16 partitions (design
  §5.8's partitioning by hash prefix; changing the count is a
  migration). The registration sequence is a Postgres `SEQUENCE`.
  Concurrency: inserts of content-addressed rows use `ON CONFLICT DO
  NOTHING` and read back, so a lost race is success; ref commits lock the
  repository row (`SELECT … FOR UPDATE`) inside the transaction that
  checks the fence (ADR 0004 item 3); a serialization failure
  (`40001`/`40P01`) is retried up to 3 times, then answered `503
  index_busy` with `Retry-After: 1`. Configuration: `[index] url`
  (`sqlite:` path or `postgres://`, default the data directory's
  `index.sqlite`), `max_connections` (default 4 × CPUs, bounds 2 to
  1024), `read_url` (optional replica).
- **M3-R3. Replica reads.** With `index.read_url`, reconstruction,
  file heads, listings and dedup lookups read from the replica. A replica
  answer of "absent" is read again from the primary before anyone is
  told "absent", so a replica never hides an acknowledged write
  (requirement 7). Writes, ref snapshots for Git and every
  authorization-bearing read (tokens, revocations, limits) use the
  primary.
- **M3-R4. Object-store buckets.** `[storage] url` selects the backend:
  `file:///path` (the M1 `LocalBucket`, default `{data_dir}/bucket`),
  `s3://bucket/prefix`, `gs://bucket/prefix`, `az://container/prefix`,
  with `object_store`'s own builder options under `[storage.options]`
  and credentials from the environment or instance identity, never from
  the config file in plain text (a `*_file` option reads a secret file,
  as the transfer key does). The key layout is M1's, under the prefix.
  `create` is `PutMode::Create`; at start-up a **conditional-create
  probe** creates `probe/{node uuid}/create` twice and requires the
  second call to report "exists"; a store failing it is refused with
  `storage_unsafe` and a hint. A remote store requires an explicit
  `cas.transfer_key_file` and an explicit CAS-token key (M1-R29).
- **M3-R5. Range-bound presigning.** OpenXet signs presigned GETs
  itself, so `Range` is a signed header: SigV4 query signing with
  `X-Amz-SignedHeaders=host;range` for `s3://` (AWS S3, R2, MinIO,
  Garage), and GCS V4 signing with `X-Goog-SignedHeaders=host;range` for
  `gs://` (service-account key or HMAC key). Azure SAS has no field that
  binds a range, so `az://` pools have no presigner and serve through
  the transfer role. Each pool's **range-binding probe** (ADR 0002 rule
  3) runs at start-up and in `openxet config check --probe`; only a pool
  whose three checks pass serves direct URLs, and the result is logged,
  exported as the `openxet_storage_direct_downloads` gauge and shown by
  `config check`. A presigned URL's lifetime is
  `min(cas.url_lifetime, credential expiry - 60 s)`; when that falls
  under ADR 0002's 30 min floor, the presigner refreshes its credentials
  first, and if it still can't reach the floor that answer's URLs are
  transfer URLs.
- **M3-R6. Schema compatibility for rolling upgrades** (issue #47,
  ADR 0017). Migrations are **expand** (additive: new tables, nullable or
  defaulted columns, new indexes) unless the file's first line is `--
  openxet: contract`. A contract migration only drops or narrows what no
  binary of the previous release reads, and raises
  `schema_compat.min_binary_epoch`. Each binary carries
  `openxet_index::SCHEMA_EPOCH`. A node opens a database that carries
  migrations it doesn't know (sqlx `set_ignore_missing(true)`) as long
  as `min_binary_epoch <= SCHEMA_EPOCH`, and refuses with
  `schema_too_new`, naming the version to upgrade to, otherwise. Nodes of
  two adjacent releases therefore share one database through a rolling
  upgrade. `index.migrate = "auto"` (default) applies pending
  migrations at start-up under Postgres' advisory lock; `"off"` leaves it
  to `openxet migrate [--dry-run] [--json]`.
- **M3-R7. OIDC.** `[[auth.oidc]]` entries: `name` (subject prefix),
  `issuer` (exact match, discovered through
  `{issuer}/.well-known/openid-configuration`), `audiences` (required,
  one must match `aud`), `subject_claim` (default `sub`), `name_claim`
  (default `preferred_username`, falling back to the subject),
  `email_claim` (default `email`), `claims` (names of string or
  string-list claims copied into the principal's claims; `orgs` feeds
  `whoami-v2`). A principal's subject is `{name}:{sub}`, so two issuers
  never share a subject. Accepted `alg`s are those of the JWKS key the
  `kid` names, from RS256, PS256, ES256, ES384 and EdDSA; `none` and HMAC
  are always refused. `exp` is exact, `nbf` and `iat` allow
  `MAX_CLOCK_SKEW`. JWKS refreshes every 10 min, and on an unknown `kid`
  at most once per 30 s per issuer; after a failed refresh the last keys
  serve up to `jwks_max_stale` (default `24h`), then requests get `503
  authn_unavailable`. `Authenticator::authenticate` becomes `async`.
- **M3-R8. The webhook authorizer.** `[auth.webhook]` `url`,
  `secret_file`, `timeout` (default `2s`), `allow_ttl` (default `30s`),
  `deny_ttl` (default `5s`). OpenXet POSTs `{"schema":
  "openxet.authz.v1", "subject", "claims", "action", "namespace",
  "name", "delegated_by"}` with `X-Openxet-Signature: sha256={hex HMAC of
  the body}` and `X-Openxet-Timestamp`; the answer is `{"allow": bool}`
  plus optional `"ttl_seconds"` (capped by the configured TTLs). Cache
  key: the full request body's SHA-256. Timeouts, non-`2xx` answers and
  unparseable bodies are `AuthzError` (`503 authz_unavailable`, never
  allow). `auth.authorizer = "rules" | "webhook" | "rules+webhook"`
  (default `rules`): with both, a rule grant allows without a call, and
  the webhook decides the rest.
- **M3-R9. OpenXet access tokens** (design §5.4, ADR 0016). A secret is
  `oxt_` plus 43 base64url characters (M1-R28); its record holds the
  SHA-256 of the secret, a UUIDv7 id, the bound subject, the claims
  snapshot, the issuer name, `created_at`, `expires_at` (default 90
  days, `auth.access_token_max_ttl` default `365d`, bounds `1d` to
  `3650d`), `revoked_at` and `last_used_at` (index only, updated at most
  once a minute). Issue: `POST /api/openxet/v1/tokens` authenticated by
  an OIDC token (an access token can't mint another access token, so
  every one traces to an identity-provider proof), or `POST
  /api/openxet/v1/admin/tokens` by a subject in `auth.admins` for any
  subject and claims (a platform's backend). The record is written to
  `tokens/{id}` in the bucket, then the index; revocation writes
  `token-revocations/{id}` first. Each use re-asks the authorizer.
  Lookup: static tokens, then a per-node positive cache (5 s), then the
  primary index; unknown secrets are never cached.
- **M3-R10. Delegated tokens** (design §7.1, ADR 0014, ADR 0016). Minted
  by `POST /api/openxet/v1/tokens/delegate` with an OIDC or access
  token, never a delegated one; revoking the access token it was minted with
  revokes it too. Body: `repos` (a list of
  `{namespace}/{name}`, at most 100, no globs), `mode` (`read` or
  `write`), `ttl` (default `8h`, at most `auth.delegated_max_ttl`,
  default `24h`, bounds `1h` to `720h`), `max_bytes` and `max_ops`
  (optional), `name` (for the audit log). At mint, the parent must be
  allowed `mode` on every repository now. On use, a request is allowed
  only if the target is listed, the action is within `mode` (`create`
  never), the parent subject is still allowed by the authorizer, and the
  token is not expired, revoked or exhausted. A CAS token minted through a
  delegated token has `sub = "delegated:{id}"` (a reserved prefix;
  static tokens can't use it) and lasts at most what the delegated
  token has left. Ops are authenticated requests made with the token or
  its CAS tokens; bytes are upload bodies accepted plus the bytes of the
  ranges a reconstruction answer grants. Usage is batched per node and
  flushed every second; exhaustion and revocation are read from a
  per-node cache at most 2 s old, so a cap overshoots by at most two
  seconds of the token's own traffic, which the docs say. This is a cap
  on granted bytes, not on egress: a replayed URL isn't counted (ADR
  0013's caveat). Token records reach the bucket like access tokens;
  after `reindex` a capped token comes back exhausted, since usage
  counters are index-only.
- **M3-R11. Rate limits.** Per-node GCRA limiters (`governor`), keyed per
  tenant and per credential, configured under `[limits]`:
  `tenant_requests_per_second` (default 2000, burst 4000),
  `credential_requests_per_second` (default 1000, burst 2000),
  `max_in_flight_per_credential` (default 512), and
  `auth_failures_per_minute` per client IP (default 60). Over a limit:
  `429 rate_limited` with `Retry-After` in whole seconds and a hint. The
  probes are exempt. Limits are per node on purpose: they protect a
  node's resources and need no shared state (non-negotiable 5); the
  deployment-wide figure is the per-node one times the nodes, which the
  docs say. Global, byte-denominated limits are quotas (M3-R12). Task 15
  confirms with the stock client that the defaults never throttle a
  default `hf_xet` transfer, and lowers or raises them with the numbers
  if not.
- **M3-R12. Quotas.** A tenant's stored bytes are the sizes of the xorbs
  it owns (M1-R39), counted alone, as stand-alone billing will be (§15):
  a counter maintained in the transaction that records an owner, and
  rebuilt from `xorb-owners/` records by `reindex`. A xorb upload that
  would take its tenant past `quota_bytes` answers `403 quota_exceeded`
  with the quota, the usage and a hint, before the body is read; reads
  never stop. `[limits] default_tenant_quota` (default unlimited); per
  tenant overrides of the quota and the rate limits through `PUT
  /api/openxet/v1/admin/tenants/{tenant}/limits`, written to
  `tenant-limits/{sha256(tenant)}` in the bucket, then the index, and
  read by nodes through a 10 s cache.
- **M3-R13. Memory shares** (M1-R40, issue #57). One credential may
  hold at most `server.credential_memory_share` of the budget (default
  `0.25`, bounds `0.01` to `1`), one tenant at most
  `server.tenant_memory_share` (default `0.5`). A reservation over its
  share waits by the same rules as one over the budget, then gets `503
  memory_busy`. A reconstruction reserves once, from the upper bound its
  stored term count gives, and a ranged request reserves only its
  range's terms, so the `413 answer_too_large` hint is only ever given
  to an answer that is too large for any range.
- **M3-R14. Index round trips outside CPU permits** (issue #48; amends
  M1-R8 and M1-R25). Shard verification's chunk tables and a
  reconstruction's chunk layouts are loaded before the `validate` permit
  is taken, with their bytes reserved from the memory budget first (from
  the stored xorb count × 320 KiB and the term count), so the memory
  bound moves from "permits × tables" to the budget, and an `index_io`
  semaphore sized to `index.max_connections` bounds the loads. The
  M1 test `chunk_tables_are_loaded_under_a_validation_permit` is replaced
  by `chunk_tables_are_reserved_before_loading` and
  `cpu_permits_idle_during_index_round_trips_is_zero`.
- **M3-R15. Key rotation** (issue #65, ADR 0011). Each verification key
  is bound to one `alg`, taken from its key type (Ed25519: `EdDSA`), and
  a token whose header `alg` differs from its key's is refused. CAS-token
  keys rotate with today's settings: add the new public key to
  `auth.jwt_public_keys` everywhere, switch `cas-token.key` and list the
  old public key, remove it after `auth.max_token_lifetime`. Transfer
  keys gain `cas.transfer_verify_key_files`: keys accepted but never
  used to sign, so the transfer key rotates the same way, in one URL
  lifetime. `openxet config check` lists every key with its `kid` or
  fingerprint, `alg` and age, and warns past `auth.key_max_age` (default
  `180d`).
- **M3-R16. Load testing** (issues #55, #43). Scenarios are TOML files
  in `loadtest/scenarios/`; thresholds in `loadtest/thresholds.toml`;
  results are JSON (`openxet.loadtest.v1`) with the machine, the git
  revision, the config and the raw latency histograms. The **nightly**
  run (CI, one runner, 3 nodes in compose, about 20 min) fails on any
  error outside the documented `503`/`413`/`429`, any acknowledged write
  missing after the node kill, memory over budget, or a p99 more than 2×
  its threshold. The **release** run (the owner's release rig, full
  scenarios, the 24 h soak) fails at the threshold itself.
- **M3-R17. Dedup influence under `global`** (issue #50; amends M1-R30;
  the policy is the owner's call, see the end of this plan). A dedup
  answer takes the newest visible shard registered **by the caller's
  tenant** that references the matched xorb, and only if there is none
  the newest shard of any tenant. Another tenant's registrations can
  then shape only the answers of tenants that never uploaded those
  chunks themselves.

## Owner-run steps and credentials

CI never holds cloud accounts. Everything below has an emulator path in
CI and an owner-run step for the real service; the owner-run results go
into the docs with the date, the region and the version.

| What | CI (every PR or nightly) | Owner-run, needs |
|---|---|---|
| Range-binding probe (ADR 0002 "Open, owned by M3") | MinIO and Garage (S3), fake-gcs-server, Azurite; each emulator's outcome is recorded, never assumed (an emulator that doesn't check signatures exercises the fallback) | AWS S3 (an IAM user or role with `s3:GetObject`/`PutObject` on a test bucket), Cloudflare R2 (an API token), GCS (a service account key or HMAC key), Azure (a storage account and SAS-capable key): `openxet config check --probe --json` per store, Task 7 step 6 |
| Bucket conformance suite | MinIO, Garage, fake-gcs-server, Azurite | the same four accounts, `OPENXET_TEST_STORAGE_URL=… cargo nextest run -p openxet-storage -E 'binary(object_bucket)'` |
| Signing benchmark (ADR 0002 consequences) | criterion locally, recorded with the machine | none (signing is local); the end-to-end direct-download timing against real S3 is part of the release run |
| Near-linear scaling 1 to N nodes | not meaningful on one runner (the nightly checks correctness only) | the release rig: N ≥ 4 node hosts, a Postgres host and an S3 bucket, or one cloud account to rent them |
| 24 h soak, release gate | a 30 min soak in the nightly | the release rig |

## Dependency graph and file ownership

```
wave 1: T1 docs/ADRs · T2 index conformance · T6 object bucket · T10 key binding + transfer keys · T11 OIDC · T12 webhook · T18 answers (#54)
wave 2: T3 Postgres CAS tables (T2) · T7 presign + probe (T6)
wave 3: T4 Postgres repos/git (T3) · T5 migration policy (T3)
wave 4: T8 serve wiring + replica (T4, T5, T6) · T13 access tokens (T4, T11, T12)
wave 5: T9 permits/#48 (T8) · T14 delegated tokens (T13) · T15 rate limits + shares (T13) · T17 dedup influence (T4) · T19 load harness (T8)
wave 6: T16 quotas (T15) · T20 multi-node + #43 (T19, T7)
wave 7: T21 tenant abuse scenarios (T16, T20) · T22 key-rotation drill (T10, T20)
wave 8: T23 gates, results, docs and project state (all)
```

Tasks in one wave are independent of each other and run in parallel;
each starts once the tasks in parentheses are merged.
Wave 5's Tasks 9, 14 and 15 all touch `crates/cas/src/reconstruction.rs`,
in different steps of a request (permit order, usage charging,
reservation); merge them in the order 9, 15, 14 to keep rebases small.

Owned files:

```
docs/adr/0015..0018, design §5.4/§5.5/§5.8/§12/§17   T1
crates/index/src/conformance/, crates/index/tests/sqlite_conformance.rs   T2
crates/index/src/postgres.rs, migrations-postgres/, tests/postgres_conformance.rs   T3
crates/index/src/postgres/{git,repos}.rs   T4
crates/index/src/{schema,lib}.rs (epoch), crates/openxet/src/migrate_cmd.rs   T5
crates/storage/src/{object,conformance}.rs, tests/object_bucket.rs, loadtest/compose.emulators.yaml   T6
crates/storage/src/presign/, crates/cas/src/answer.rs (URL source), crates/cas/benches/presign.rs   T7
crates/openxet/src/{serve,config}.rs ([index], [storage]), crates/index/src/replica.rs   T8
crates/cas/src/{shards,reconstruction,plan}.rs   T9
crates/auth/src/{jwt,signed_url}.rs, crates/openxet/src/config_cmd.rs (keys)   T10
crates/auth/src/oidc.rs, crates/auth/src/lib.rs (async authenticate)   T11
crates/auth/src/webhook.rs, crates/auth/src/rules.rs (composition)   T12
crates/auth/src/access.rs, crates/index/src/tokens.rs + both backends, crates/storage/src/records.rs (token records), crates/hub/src/tokens_api.rs, crates/openxet/src/token_cmd.rs   T13
crates/auth/src/delegation.rs, crates/cas/src/usage.rs   T14
crates/http/src/limits.rs, crates/cas/src/budget.rs   T15
crates/index/src/limits.rs + both backends, crates/hub/src/admin.rs, crates/cas/src/xorbs.rs (quota check)   T16
crates/cas/src/dedup.rs, Index::dedup_shard_xorbs   T17
crates/cas/src/{answer,send_body}.rs (segments), crates/cas/src/xorbs.rs (streaming, if step 1 allows)   T18
crates/loadgen/, loadtest/{scenarios,run.py,thresholds.toml,pyproject.toml}   T19
loadtest/compose.yaml, loadtest/haproxy.cfg, loadtest/drills/kill_node.py   T20
loadtest/scenarios/{many_tenants,abuse}.toml   T21
loadtest/drills/key_rotation.py, docs/guides/key-rotation.md   T22
docs/, AGENTS.md, CHANGELOG.md, .github/workflows/loadtest-nightly.yml   T23 (plus each task's reference pages)
```

---

### Task 1: Record M3's decisions (ADRs 0015 to 0018, design edits, security map rows)

**Files:**
- Create: `docs/adr/0015-production-backends-and-range-bound-presigning.md`, `docs/adr/0016-issued-and-delegated-tokens.md`, `docs/adr/0017-schema-compatibility-for-rolling-upgrades.md`, `docs/adr/0018-limits-quotas-and-memory-shares.md`
- Modify: `docs/superpowers/specs/2026-10-23-openxet-design.md` (§5.4 OIDC, webhook, access and delegated tokens; §5.5 presigners and the conditional-create probe; §5.8 replicas and limits; §7.1 delegated tokens point at ADR 0016; §12 M3 links this plan and marks PQ TLS shipped; §17 rows M3-R1 to M3-R17), `docs/adr/0002-one-range-bound-url-per-fetch-entry.md` (consequences: "Open, owned by M3" points at Task 7), `docs/adr/0007-cas-tokens-are-eddsa-jwts.md` (amended: the reserved `delegated:` subject prefix), `docs/reference/security-tests.md` (M3 rows, `pending (M3, <file>)`), `docs/SUMMARY.md`

**Interfaces:**
- Produces: M3-R1 to M3-R17 as public text; ADRs 0015 to 0018 as their citations.

- [ ] **Step 1: Write the four ADRs** from the template:
  - 0015 from M3-R2 to M3-R5: context (ADR 0002's open list, §5.8's
    topology, `object_store`'s signer can't sign `Range`), decision,
    consequences (Azure serves through the transfer role until a SAS can
    bind a range; replicas never answer "absent" alone), alternatives
    (`aws-sigv4` from the AWS SDK: rejected, one signer per family is
    about 200 lines with published test vectors and avoids the SDK's
    dependency tree; unbound presigned URLs: rejected by ADR 0002; a
    distributed KV now: rejected, §5.8 keeps it for later).
  - 0016 from M3-R7 to M3-R10: why access tokens can't mint access
    tokens, why delegated tokens can't chain, why caps count granted
    bytes, why token records go to the bucket (losing the database must
    not lose anyone's access, design §5.3).
  - 0017 from M3-R6: expand/contract with an epoch; alternatives
    (refuse unknown migrations, the M1 behaviour: blocks rolling
    upgrades; an N-ahead window without epochs: can't express a
    contract).
  - 0018 from M3-R11 to M3-R13: per-node rate limits vs. global quotas
    and why; alternatives (shared counters in Postgres per request:
    a round trip on every request; a Redis tier: new state to run, against
    "nothing to babysit").
- [ ] **Step 2: Edit the design and ADR amendments** as listed above.
  Record M3-R14 and M3-R17 as amendments of M1-R8/M1-R25 and M1-R30 in
  §17, and add to §5.9 the new test names (Task 11, 13, 14 and 15
  security tests) under requirements 2, 3 and 6.
- [ ] **Step 3: Security map.** Add rows for every security test this
  plan names, `pending (M3, <file>)`, so the checker holds each task to
  its file.
- [ ] **Step 4: Verify.** `scripts/build-site.sh`,
  `python3 -I scripts/check-copy.py`, `python3 -I scripts/check-brand.py`
  and the security-map checks pass; `git grep -n -i -E
  "weak|vulnerab|exploit" docs/adr/001[5-8]*` shows only OpenXet's own
  policy.
- [ ] **Step 5: Commit** as `docs(adr): 0015 to 0018 production backends, issued tokens, schema compatibility, limits; design M3 rulings`.

### Task 2: A backend-agnostic index conformance suite

**Files:**
- Create: `crates/index/src/conformance/{mod,cas,repos,git}.rs` (behind the `test-support` feature), `crates/index/tests/sqlite_conformance.rs`
- Modify: `crates/index/Cargo.toml` (feature), `crates/index/tests/{sqlite_index,repos,git_store,concurrency}.rs` (cases that hold for any backend move into the suite; SQLite-only cases, such as the open lock and file modes, stay)

**Interfaces:**
- Produces:

```rust
#[async_trait] pub trait Fixture: Send + Sync {
    type Index: HubIndex;
    async fn fresh(&self) -> Self::Index;          // an empty, migrated index
    async fn second_handle(&self, of: &Self::Index) -> Self::Index;  // another pool on the same database: a second node
}
pub async fn run_all<F: Fixture>(f: &F);           // every case, each on a fresh index, named in the panic message
pub async fn run_cas<F: Fixture>(f: &F);
pub async fn run_repos<F: Fixture>(f: &F);
pub async fn run_git<F: Fixture>(f: &F);
```

- [ ] **Step 1: Move the cases.** Each backend-agnostic test of the four
  files becomes an `async fn case_<name>(f: &F)` in the suite, unchanged
  in its assertions; add the two-handle cases Review Focus 1 names,
  `concurrent_register_same_shard_two_pools`,
  `concurrent_put_xorb_two_pools` and
  `concurrent_hub_commits_two_pools_one_winner`, each driving 16
  concurrent calls split over `fresh` and `second_handle`.
- [ ] **Step 2: Run** `cargo nextest run -p openxet-index`. Expected:
  PASS on SQLite with the same test count plus the three new cases
  (SQLite's second handle is a second `SqliteIndex::open` on one file).
- [ ] **Step 3: Commit** as `test(index): backend-agnostic conformance suite, run on SQLite`.

### Task 3: Postgres index, CAS tables

**Files:**
- Create: `crates/index/src/postgres.rs`, `crates/index/migrations-postgres/0001_init.sql` (the end state of SQLite's 0001 to 0007, partitioned per M3-R2), `crates/index/tests/postgres_conformance.rs`
- Modify: `crates/index/Cargo.toml` (sqlx `postgres` feature), `crates/index/src/lib.rs` (`PostgresIndex`, `IndexError::Busy`), `.github/workflows/ci.yml` (a `postgres:17` service on the `rust` job, `OPENXET_TEST_POSTGRES_URL`)

**Interfaces:**
- Consumes: Task 2's `Fixture` and `run_cas`.
- Produces:

```rust
pub struct PostgresIndex { /* PgPool, optional replica pool from Task 8 */ }
impl PostgresIndex {
    pub async fn connect(url: &str, max_connections: u32, migrate: Migrate) -> Result<Self, IndexError>;
}
pub enum Migrate { Auto, Off }                     // Task 5 fills in the epoch check
impl Index for PostgresIndex { /* every method */ }
// IndexError::Busy after 3 retried serialization failures → CAS answers 503 index_busy, Retry-After: 1
```

- [ ] **Step 1: Write the failing test** `postgres_conformance.rs`: a
  `Fixture` that creates a fresh database per case
  (`CREATE DATABASE openxet_test_{uuid}` on the service) and calls
  `run_cas`; skips with the reason when `OPENXET_TEST_POSTGRES_URL` is
  unset (fails under `OPENXET_REQUIRE_SERVICES=1`). Add
  `serialization_failures_retry_then_busy` (a fault hook makes the first
  three commits fail with `40001`: the fourth attempt's error is
  `IndexError::Busy`).
- [ ] **Step 2: Run** `OPENXET_TEST_POSTGRES_URL=postgres://… cargo
  nextest run -p openxet-index -E 'binary(postgres_conformance)'`.
  Expected: FAIL (no `PostgresIndex`).
- [ ] **Step 3: Implement** the schema and every `Index` method. Chunk
  tables and layouts stay per-xorb `BYTEA` blobs, as in SQLite (§5.8: no
  per-chunk rows). `register_shard` is one transaction; owner, scope and
  dedup rows use `ON CONFLICT DO NOTHING`; the M1-R37 "lowest
  `(seq, shard, tenant)`" row is kept with `ON CONFLICT … DO UPDATE …
  WHERE (excluded.seq, …) < (…)`.
- [ ] **Step 4: Run** the suite and `cargo clippy`. Expected: PASS; the
  CAS crate's tests unchanged.
- [ ] **Step 5: Benchmark** `cargo bench -p openxet-cas --bench
  commit_throughput` with a Postgres variant
  (`OPENXET_BENCH_POSTGRES_URL`), recording shard and xorb registrations
  per second against §5.8's 200 and 2,000 per second in `benchmarks.md`
  with the machine.
- [ ] **Step 6: Commit** as `feat(index): Postgres index for xorbs, files, scopes and dedup`.

### Task 4: Postgres index, repositories, refs and packs

**Files:**
- Create: `crates/index/src/postgres/{repos,git}.rs`
- Modify: `crates/index/src/postgres.rs`, `crates/index/tests/postgres_conformance.rs` (`run_repos`, `run_git`)

**Interfaces:**
- Consumes: Task 3's `PostgresIndex`.
- Produces: `impl Repos for PostgresIndex`, `impl GitStore for PostgresIndex`, so `PostgresIndex: HubIndex`.

- [ ] **Step 1: Write the failing test:** enable `run_repos` and
  `run_git` in `postgres_conformance.rs`, plus
  `lease_epoch_fences_a_stale_holder_across_pools` (two pools; the
  holder whose lease was taken over can't publish a repack).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** `commit` locks the repository row with
  `SELECT … FOR UPDATE`, checks every `RefUpdate.old` and the
  `full_repack_gen` fence, writes refs and the `ref_log` row in the same
  transaction (ADR 0004 item 3). The case-insensitive name key is a
  unique index on `lower(namespace) || '/' || lower(name)`.
- [ ] **Step 4: Run** the suite. Expected: PASS, including
  `concurrent_hub_commits_two_pools_one_winner`.
- [ ] **Step 5: Commit** as `feat(index): Postgres repositories, refs, packs and leases`.

### Task 5: Schema compatibility for rolling upgrades (issue #47)

**Files:**
- Create: `crates/index/src/schema.rs`, `crates/index/migrations/0008_schema_compat.sql`, `crates/index/migrations-postgres/0002_schema_compat.sql`, `crates/index/tests/schema_compat.rs`, `crates/index/tests/fixtures/next_release/` (an expand and a contract migration used only by tests), `crates/openxet/src/migrate_cmd.rs`, `crates/openxet/tests/migrate.rs`
- Modify: `crates/index/src/{lib,sqlite,postgres}.rs` (open through `schema::open`), `crates/openxet/src/{cli,main}.rs`, `compat/tests/test_agent_cli.py` (`JSON_COMMANDS`), `docs/reference/{cli,exit-codes}.md`

**Interfaces:**
- Produces:

```rust
pub const SCHEMA_EPOCH: u32 = 1;
pub enum SchemaCheck { Current, Behind { pending: Vec<i64> }, AheadCompatible { unknown: Vec<i64> }, TooNew { min_binary_epoch: u32 } }
pub async fn check(pool: &AnyPoolRef<'_>) -> Result<SchemaCheck, IndexError>;
// IndexError::SchemaTooNew { min_binary_epoch } → serve exits with code `schema_too_new` and a hint naming the upgrade
```

  - `openxet migrate [--dry-run] [--json]` (`openxet.migrate.v1`:
    `{applied: [..], pending: [..], epoch, min_binary_epoch}`).

- [ ] **Step 1: Write the failing tests** (both backends where noted):
  - `older_binary_opens_database_with_expand_migration` (apply the
    fixture's expand migration with a test migrator, then open with the
    shipped one: `AheadCompatible`, every conformance read works).
  - `older_binary_refuses_after_contract_migration` (the fixture's
    contract migration raises `min_binary_epoch` to 2: `SchemaTooNew`).
  - `migration_files_declare_their_kind` (every shipped migration is
    expand unless marked, and a contract file is rejected unless it
    raises the epoch).
  - `concurrent_migrate_runs_once` (Postgres: two nodes start together
    with `migrate = "auto"`; each migration is applied once).
  - `migrate_dry_run_changes_nothing`, `migrate_json_schema` (CLI).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement** `schema::open` for both backends with
  `Migrator::set_ignore_missing(true)` and the epoch check; `index.migrate`
  is read by Task 8, so this task takes the setting as a parameter.
- [ ] **Step 4: Write the procedure** in `docs/guides/rolling-upgrades.md`
  (new; in `SUMMARY.md`): upgrade nodes one at a time, contract releases
  only after every node runs the previous release, `openxet migrate
  --dry-run` first. Task 20 runs it in the multi-node rig.
- [ ] **Step 5: Run** the tests and the agent leg. Expected: PASS.
- [ ] **Step 6: Commit** as `feat(index): expand/contract schema epochs so two releases share one database (#47)`.

### Task 6: `ObjectBucket` on S3, GCS and Azure, with the conditional-create probe

**Files:**
- Create: `crates/storage/src/object.rs`, `crates/storage/src/conformance.rs` (behind `test-support`), `crates/storage/tests/object_bucket.rs`, `loadtest/compose.emulators.yaml` (MinIO, Garage, fake-gcs-server, Azurite, each pinned by digest)
- Modify: `crates/storage/Cargo.toml` (`object_store` with `aws`, `gcp`, `azure`), `crates/storage/src/{lib,error}.rs`, `crates/storage/tests/local_bucket.rs` (backend-agnostic cases move to the suite), `.github/workflows/ci.yml` (a `storage-emulators` job)

**Interfaces:**
- Produces:

```rust
pub struct ObjectBucket { /* Arc<dyn ObjectStore>, prefix, kind: StoreKind */ }
pub enum StoreKind { S3, Gcs, Azure }
impl ObjectBucket {
    pub async fn open(url: &Url, options: &BTreeMap<String, String>) -> Result<Self, StorageError>;
    pub async fn probe_conditional_create(&self, node: Uuid) -> Result<(), StorageError>;  // StorageError::Unsafe { check, hint }
    pub fn store(&self) -> (StoreKind, Arc<dyn ObjectStore>);   // for Task 7's presigners
}
impl Bucket for ObjectBucket { /* every method */ }
pub async fn conformance::run_all(open: impl Fn() -> BoxFuture<'static, Arc<dyn Bucket>>);
```

- [ ] **Step 1: Write the failing tests:**
  - the conformance suite from `local_bucket.rs`'s backend-agnostic
    cases (put idempotence, size conflict, `create` first-writer-wins,
    `put_replace` only for `RepoMeta`, ranges, streaming blocks of at most
    `STREAM_BLOCK_BYTES`, `list(kind)` skipping foreign names, `delete`),
    run against `LocalBucket` and against each emulator named by
    `OPENXET_TEST_STORAGE_URL` (`s3://…?endpoint=…`, `gs://…`, `az://…`);
  - `start_refuses_store_without_conditional_create` (a wrapper store
    that ignores `PutMode::Create`: `probe_conditional_create` returns
    `Unsafe` naming the check and `storage.options`);
  - `create_race_one_winner_on_object_store` (32 concurrent `create`s of
    one registration key: exactly one `true`, every reader sees its
    bytes);
  - `put_stream_uses_multipart_and_is_atomic` (a stream failing halfway
    leaves no object under the key).
- [ ] **Step 2: Run** with the emulators up (`docker compose -f
  loadtest/compose.emulators.yaml up -d`). Expected: FAIL.
- [ ] **Step 3: Implement.** `put` of a content-addressed key: `head`
  first, then `PutMode::Overwrite` (identical bytes); `create`:
  `PutMode::Create`, mapping `AlreadyExists`/`Precondition` to
  `Ok(false)`; `put_replace`: plain put (atomic on object stores);
  `put_stream`: `put_multipart` with 8 MiB parts; `get_to_file`: a
  ranged stream to a temp file, synced, renamed. Record which emulator
  passes the conditional-create probe and the suite in the task's PR
  description; an emulator that can't is excluded from CI with the
  reason in `ci.yml`, not patched around.
- [ ] **Step 4: Owner-run step** (documented in the PR, not blocking
  merge): the suite against real AWS S3, R2, GCS and Azure with the
  owner's accounts.
- [ ] **Step 5: Commit** as `feat(storage): object-store buckets on S3, GCS and Azure with a conditional-create probe`.

### Task 7: Range-bound presigned downloads, the probe and the signing benchmark (ADR 0002)

**Files:**
- Create: `crates/storage/src/presign/{mod,sigv4,gcs_v4,probe}.rs`, `crates/storage/tests/presign.rs`, `crates/cas/benches/presign.rs`, `docs/reference/object-stores.md`
- Modify: `crates/cas/src/{answer,lib}.rs` (the URL source), `crates/cas/tests/reconstruction.rs`, `crates/openxet/src/config_cmd.rs` (`--probe`), `compat/tests/test_storage_only.py` (direct-download legs), `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Task 6's `ObjectBucket::store`.
- Produces:

```rust
pub trait RangePresigner: Send + Sync + 'static {
    // Synchronous: called inside the answer writer for each range.
    fn presign_range(&self, key: &Key, start: u64, end_inclusive: u64, now: u64, lifetime: Duration) -> Result<Url, StorageError>;
    fn credential_expiry(&self) -> Option<u64>;     // M3-R5's lifetime cap
}
pub fn presigner_for(bucket: &ObjectBucket) -> Option<Arc<dyn RangePresigner>>;   // None for Azure and LocalBucket
pub enum ProbeOutcome { Direct, Transfer { failed: ProbeCheck, status: Option<u16> } }
pub enum ProbeCheck { SignedRange206, OtherRange403, NoRange4xx, Setup }
pub async fn probe_range_binding(bucket: &ObjectBucket, signer: &dyn RangePresigner, http: &reqwest::Client) -> ProbeOutcome;
// openxet_cas: pub enum XorbUrlSource { Transfer(Arc<UrlSigner>), Direct { presigner: Arc<dyn RangePresigner>, fallback: Arc<UrlSigner> } }
```

- [ ] **Step 1: Write the failing tests:**
  - `sigv4_matches_aws_test_vectors` (AWS's published presigned-URL
    example, extended with a `range` signed header and checked against
    MinIO accepting it) and `gcs_v4_matches_reference` (Google's V4
    signing example);
  - `probe_passes_on_minio`, `probe_falls_back_on_unbound_signer` (a
    signer that omits `range` from the signed headers: `Transfer {
    failed: OtherRange403 }`), one fake-store test per `ProbeCheck`;
  - `direct_urls_only_after_probe_passed`,
    `every_direct_url_binds_its_range` (each fetch entry's URL carries
    `range` in its signed headers and one range, ADR 0002's shape test),
    `refreshed_answer_keeps_terms_and_ranges`,
    `presigned_lifetime_capped_by_credential_expiry`;
  - compat: `test_download_direct_from_minio` with
    `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` unset and `true`, and with
    `HF_XET_CLIENT_RECONSTRUCTION_API_VERSION=1`; and
    `test_expired_direct_url_refreshes` (S1 variant d: the first GETs
    see `403`, the client refreshes once and completes).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement** the signers (derive the SigV4 signing key
  once per day and region, then one HMAC per URL), the probe (a fixed
  64 KiB object at `probe/range-binding`, created once and reused), and
  `XorbUrlSource` in `answer.rs` so the writer asks the source per
  range.
- [ ] **Step 4: Benchmark** `cargo bench -p openxet-cas --bench presign`:
  URLs per second per core for SigV4 and GCS V4 against the transfer
  signer, and the 100k-term reconstruction answer's build time in both
  modes; record in `benchmarks.md` with the machine. The latency gate's
  p99 < 50 ms must still hold with direct URLs (run the gate with a
  MinIO-backed pool once and record it).
- [ ] **Step 5: Run** the tests and the storage-only compat leg against
  MinIO. Expected: PASS.
- [ ] **Step 6: Owner-run step:** `openxet config check --probe --json`
  against real AWS S3, R2, Garage (self-hosted), GCS and Azure; record
  each store's outcome, the failed check if any, the date and the region
  in `docs/reference/object-stores.md`, and tick ADR 0002's "Open, owned
  by M3" items with a link. Until a store is recorded, the page says
  "not yet probed" for it.
- [ ] **Step 7: Commit** as `feat(storage): range-bound presigned downloads gated by the ADR 0002 probe`.

### Task 8: Serve on production backends, with replica reads

**Files:**
- Create: `crates/index/src/replica.rs`, `crates/openxet/tests/serve_backends.rs`, `docs/guides/production-backends.md`
- Modify: `crates/openxet/src/{serve,config,config_cmd,purge_cmd,label_cmd}.rs`, `crates/index/src/postgres.rs` (read pool), `docs/reference/config.md` (`[index]`, `[storage]`), `.github/workflows/ci.yml` (`compat-storage-only` and `compat-hub` also run on Postgres + MinIO)

**Interfaces:**
- Consumes: Tasks 3 to 7.
- Produces: `openxet serve` with `[index] url, max_connections, read_url, migrate` and `[storage] url, [storage.options]`; `purge` and the other data-directory commands open the configured backends through one `open_backends(&Config) -> (Arc<dyn HubIndex>, Arc<dyn Bucket>, Option<Arc<dyn RangePresigner>>)`.

- [ ] **Step 1: Write the failing tests:**
  - `serve_on_postgres_and_minio_round_trips` (upload, register, read
    through two `serve` processes sharing the backends);
  - `replica_miss_reads_primary` (a replica fixture that lags by
    holding reads at a snapshot taken before the write: the read still
    finds the file) and `replica_never_used_for_tokens_or_refs`;
  - config: `remote_store_requires_explicit_keys` (M1-R29: a remote
    `storage.url` with a defaulted transfer key or CAS-token key is a
    problem naming both keys), `storage_secrets_only_from_files`
    (`storage.options.secret_access_key` is refused with a hint to use
    `secret_access_key_file` or the environment), `index_url_bounds`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement** the wiring and the replica wrapper
  (`ReplicaIndex<I>` delegating writes and authorization reads to the
  primary, re-reading "absent" from it).
- [ ] **Step 4: Write** `docs/guides/production-backends.md`: Postgres,
  S3, GCS, Azure, MinIO and Garage configs that run in the docs leg
  against the emulators, and what `config check --probe` says.
- [ ] **Step 5: Run** the tests, the storage-only and Hub legs on both
  backends, and the docs leg. Expected: PASS.
- [ ] **Step 6: Commit** as `feat(server): serve on Postgres and object stores, with replica reads`.

### Task 9: Index round trips outside the CPU permits (issue #48)

**Files:**
- Modify: `crates/cas/src/{shards,reconstruction,plan,lib}.rs` (`index_io` semaphore), `crates/cas/tests/{shards,reconstruction}.rs`, `docs/reference/benchmarks.md`

**Interfaces:**
- Consumes: Task 8's Postgres-backed state; `MemoryBudget` (Task 15 adds shares; this task only reserves).
- Produces: `AppState::index_io: Arc<Semaphore>`; the M3-R14 order: reserve, `index_io`, load, then `validate`.

- [ ] **Step 1: Write the failing tests:** `chunk_tables_are_reserved_before_loading`
  (the reservation exists before `chunk_tables` is called; a budget too
  small for the tables gives `503 memory_busy` without loading),
  `cpu_permits_idle_during_index_round_trips_is_zero` (an index fake
  that sleeps 50 ms per load: while it sleeps, every `validate` permit
  is free), and delete `chunk_tables_are_loaded_under_a_validation_permit`
  in the same commit, citing M3-R14.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement** the reordering in shard ingest and
  reconstruction.
- [ ] **Step 4: Measure** shard-ingest throughput and reconstruction p99
  against Postgres before and after (`commit_throughput` and the latency
  gate with `OPENXET_BENCH_POSTGRES_URL`), and record both in
  `benchmarks.md`.
- [ ] **Step 5: Commit** as `perf(cas): load chunk tables and layouts outside the CPU permits (#48)`.

### Task 10: Per-key `alg` binding, transfer verify keys and the key report (issue #65, code half)

**Files:**
- Modify: `crates/auth/src/{jwt,signed_url,capability}.rs`, `crates/auth/tests/auth.rs`, `crates/openxet/src/{config,config_cmd}.rs`, `docs/reference/config.md` (Keys and tokens on disk, CAS tokens, Transfer URLs)

**Interfaces:**
- Produces:

```rust
pub enum KeyAlg { EdDSA }                          // ML-DSA-65 arrives by its own ADR (ADR 0011)
impl PublicKey { pub fn alg(&self) -> KeyAlg }
impl UrlSigner { pub fn with_verify_keys(self, keys: Vec<[u8; 32]>) -> Self }  // accepted, never used to sign
impl CapabilitySigner { pub fn with_verify_keys(self, keys: Vec<[u8; 32]>) -> Self }
```

  - `config check --json` gains `keys: [{kind, id, alg, age_days, warning?}]`.

- [ ] **Step 1: Write the failing tests:** `token_alg_must_match_key_alg`,
  `unknown_kid_refused`, `old_and_new_cas_keys_both_verify`,
  `transfer_url_signed_with_previous_key_verifies`,
  `capability_signed_with_previous_key_verifies`,
  `verify_only_key_never_signs`, `config_check_warns_on_old_key`.
- [ ] **Step 2: Run.** Expected: FAIL (the first three may pass
  already; keep them as the rotation regression tests).
- [ ] **Step 3: Implement** `cas.transfer_verify_key_files` and the key
  report (age from the key file's modification time).
- [ ] **Step 4: Run** the auth, CAS and Hub suites. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(auth): per-key alg binding, transfer verify keys and a key report (#65)`.

### Task 11: OIDC authentication

**Files:**
- Create: `crates/auth/src/oidc.rs`, `crates/auth/tests/oidc.rs`, `crates/auth/tests/support/issuer.rs` (an in-process issuer serving discovery and JWKS, signing RS256, ES256 and EdDSA)
- Modify: `crates/auth/src/{lib,error}.rs` (`authenticate` becomes `async`; `AuthError::Unavailable`), every call site in `crates/cas/src/auth.rs` and `crates/hub/src/authz.rs`, `crates/openxet/src/{config,serve}.rs` (`[[auth.oidc]]`), `crates/cas/tests/` and `crates/hub/tests/` security matrices, `docs/reference/config.md` (OIDC)

**Interfaces:**
- Produces:

```rust
pub struct OidcIssuer { /* name, issuer, audiences, claim mapping, ArcSwap<Jwks> */ }
impl OidcIssuer {
    pub async fn discover(cfg: OidcConfig, http: reqwest::Client) -> Result<Self, AuthError>;
    pub async fn verify(&self, jwt: &str, now: u64) -> Result<UserPrincipal, AuthError>;  // subject "{name}:{sub}"
}
impl Authenticator {
    pub fn with_oidc(self, issuers: Vec<Arc<OidcIssuer>>) -> Self;
    pub async fn authenticate(&self, bearer: &str, now: u64) -> Result<Credential, AuthError>;  // static, CAS JWT, OIDC; access tokens join in Task 13
}
```

- [ ] **Step 1: Write the failing tests:** `oidc_token_becomes_user_principal`
  (subject, name, email and mapped claims), `wrong_issuer_or_audience_refused`,
  `alg_none_and_hs256_refused`, `alg_must_match_jwks_key`,
  `exp_exact_nbf_iat_skew`, `jwks_unknown_kid_refreshes_once`,
  `jwks_refresh_rate_limited`, `jwks_outage_serves_cached_then_fails_closed`,
  `two_issuers_never_share_a_subject`, and in the CAS and Hub matrices
  `oidc_token_scoped_like_static_user_token` (rules decide; an OIDC token
  is never a CAS credential) and `oidc_token_never_logged`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement** with `jsonwebtoken` 11 on `aws-lc-rs`; a
  background task per issuer refreshes JWKS; the unknown-`kid` path
  awaits one refresh under a per-issuer mutex.
- [ ] **Step 4: Compat:** `test_hf_whoami_with_oidc_token` (the hub leg
  starts the issuer from `compat/hub_harness.py`; `hf auth whoami`
  shows the mapped name and orgs).
- [ ] **Step 5: Run** every suite and the hub leg. Expected: PASS.
- [ ] **Step 6: Commit** as `feat(auth): OIDC authentication with cached, fail-closed JWKS`.

### Task 12: The webhook authorizer

**Files:**
- Create: `crates/auth/src/webhook.rs`, `crates/auth/tests/webhook.rs`, `docs/reference/authorizer-webhook.md` (the contract, with a 20-line reference receiver in Python that the docs leg runs)
- Modify: `crates/auth/src/{rules,lib}.rs` (`Either` composition), `crates/openxet/src/{config,serve}.rs` (`[auth.webhook]`, `auth.authorizer`), `docs/reference/config.md`, `docs/SUMMARY.md`

**Interfaces:**
- Produces:

```rust
pub struct WebhookAuthorizer { /* url, secret, timeout, ttls, a bounded LRU of 100k decisions */ }
impl Authorizer for WebhookAuthorizer { .. }
pub struct RulesThenWebhook(pub RulesAuthorizer, pub WebhookAuthorizer);
impl Authorizer for RulesThenWebhook { .. }
```

- [ ] **Step 1: Write the failing tests:** `webhook_allow_and_deny`,
  `request_is_signed_and_timestamped`, `allow_and_deny_cached_per_ttl`,
  `cache_keyed_by_subject_and_claims` (two subjects, one target: two
  calls; requirement 3), `timeout_fails_closed`,
  `non_2xx_or_bad_body_fails_closed`, `rule_grant_skips_webhook`,
  `webhook_secret_never_logged`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement**; the cache is a bounded LRU (no new
  dependency unless `cargo search` finds one already in the lock file).
- [ ] **Step 4: Run.** Expected: PASS; the token, head and resolve p99
  < 10 ms gate excludes the webhook (§5.8), so measure a cached decision
  and record it.
- [ ] **Step 5: Commit** as `feat(auth): the webhook authorizer with caller-keyed, fail-closed caching`.

### Task 13: OpenXet access tokens

**Files:**
- Create: `crates/auth/src/access.rs`, `crates/index/src/tokens.rs` (trait), `crates/index/src/sqlite/tokens.rs`, `crates/index/src/postgres/tokens.rs`, `crates/index/migrations/0009_tokens.sql`, `crates/index/migrations-postgres/0003_tokens.sql`, `crates/hub/src/tokens_api.rs`, `crates/hub/tests/access_tokens.rs`, `crates/openxet/tests/token_api.rs`, `docs/guides/identity.md`
- Modify: `crates/storage/src/records.rs` (`TokenRecord`, `TokenRevocation`, keys `tokens/` and `token-revocations/`), `crates/index/src/conformance/` (token cases), `crates/openxet/src/token_cmd.rs` (`issue`, `list`, `revoke` against a server), `crates/openxet/src/config.rs` (`auth.admins`, `auth.access_token_max_ttl`), `docs/reference/openapi.json`, `compat/tests/test_agent_cli.py`, `compat/tests/test_hub.py`, `docs/SUMMARY.md`

**Interfaces:**
- Consumes: Task 11's async `authenticate`; Task 12's authorizers; Task 4's backends.
- Produces:

```rust
pub struct TokenId(Uuid);                          // UUIDv7, Display = 32 hex
pub enum TokenKind { Access, Delegated(Delegation) }   // Delegation filled in by Task 14
pub struct IssuedToken { pub id: TokenId, pub secret_sha256: [u8; 32], pub subject: String, pub claims: BTreeMap<String, Vec<String>>, pub issuer: String, pub kind: TokenKind, pub created_at: u64, pub expires_at: u64 }
#[async_trait] pub trait Tokens {
    async fn insert_token(&self, t: &IssuedToken) -> Result<(), IndexError>;
    async fn token_by_secret(&self, sha256: &[u8; 32]) -> Result<Option<(IssuedToken, Option<u64> /*revoked_at*/)>, IndexError>;
    async fn tokens_of(&self, subject: &str, cursor: Option<&str>, limit: usize) -> Result<Page<IssuedToken>, IndexError>;
    async fn revoke_token(&self, id: TokenId, at: u64) -> Result<bool, IndexError>;
    async fn touch_token(&self, id: TokenId, at: u64) -> Result<(), IndexError>;   // last_used_at, at most once a minute per node
}
// HubIndex gains Tokens.
```

  - Routes: `POST /api/openxet/v1/tokens` (OIDC credential only) →
    `{schema: "openxet.token.v1", id, token, expires_at}`, the secret
    shown once; `GET /api/openxet/v1/tokens` (own tokens, no secrets);
    `DELETE /api/openxet/v1/tokens/{id}` (own tokens; admins any);
    `POST /api/openxet/v1/admin/tokens` (admins).
  - CLI: `openxet token issue|list|revoke --server URL` with the
    credential from `OPENXET_TOKEN` (never a flag, so it stays out of
    shell history), `--json`, `--dry-run` on `issue` and `revoke`.

- [ ] **Step 1: Write the failing tests:**
  - `access_token_issued_from_oidc_only` (an access or static token
    gets `403 needs_identity_provider` with a hint);
  - `secret_stored_only_as_hash` (index row and bucket record hold no
    secret), `token_record_reaches_bucket_before_index` (a failing index
    insert leaves the record and answers `503`; a retry succeeds);
  - `revoked_token_refused_on_every_node` (two `serve` processes;
    revocation on one, refusal on the other within the 5 s cache),
    `expired_access_token_refused`, `unknown_secret_not_cached`;
  - `authorizer_reasked_on_use` (the webhook flips to deny: the next
    request with the token is refused);
  - `whoami_answers_bound_identity`;
  - security matrices: `access_token_scoped_to_repo`,
    `access_token_never_logged_or_echoed`;
  - compat: `test_hf_login_with_access_token_upload_download` (OIDC
    token from the test issuer → `openxet token issue` → `hf auth login
    --token` → `hf upload` → `hf download`).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement**; every issue and revoke writes an
  `audit_log` row (actor, `token.issue`/`token.revoke`, token id).
- [ ] **Step 4: Write** `docs/guides/identity.md`: OIDC (Keycloak,
  Entra, Google, GitHub Actions examples), the webhook, issuing a token
  and logging in with `hf`, the agent invocation first and the human
  walkthrough beside it (ADR 0014).
- [ ] **Step 5: Run** every suite, the agent and hub legs, and the
  security-map checks. Expected: PASS.
- [ ] **Step 6: Commit** as `feat(auth): OpenXet access tokens bound to an OIDC identity`.

### Task 14: Delegated, attenuated agent tokens

**Files:**
- Create: `crates/auth/src/delegation.rs`, `crates/cas/src/usage.rs`, `crates/hub/tests/delegated_tokens.rs`, `crates/cas/tests/delegated_usage.rs`, `docs/guides/agent-tokens.md`
- Modify: `crates/auth/src/access.rs`, `crates/index/src/{tokens,sqlite/tokens,postgres/tokens}.rs` (usage counters, `add_usage`), migrations (usage columns: expand), `crates/hub/src/{tokens_api,tokens}.rs` (CAS token `sub`), `crates/cas/src/{auth,xorbs,shards,reconstruction}.rs` (charge usage), `crates/openxet/src/token_cmd.rs` (`delegate`), `compat/tests/test_agent_cli.py`, `docs/SUMMARY.md`

**Interfaces:**
- Consumes: Task 13's `Tokens`, `IssuedToken`, routes and CLI.
- Produces:

```rust
pub struct Delegation { pub parent: Option<TokenId> /* None when minted with an OIDC token */, pub on_behalf_of: String, pub repos: Vec<RepoName>, pub mode: Mode, pub max_bytes: Option<u64>, pub max_ops: Option<u64>, pub name: String }
pub struct DelegatedAuthorizer<A: Authorizer>(pub A);   // M3-R10's use-time rule over the parent's authorizer
pub struct UsageBatcher { /* per node, flushes every 1 s */ }
impl UsageBatcher { pub fn charge(&self, token: TokenId, ops: u64, bytes: u64); pub fn exhausted(&self, token: TokenId) -> bool /* cache ≤ 2 s */ }
// #[async_trait] Tokens gains: async fn add_usage(&self, batch: &[(TokenId, u64, u64)]) -> Result<Vec<(TokenId, Usage)>, IndexError>;
pub const DELEGATED_SUBJECT_PREFIX: &str = "delegated:";
```

  - Route `POST /api/openxet/v1/tokens/delegate`; CLI `openxet token
    delegate --repo ns/name … --mode read|write --ttl 8h [--max-bytes
    10GiB] [--max-ops N] --name NAME`.

- [ ] **Step 1: Write the failing tests** (security tests go in the map
  under requirement 2 and ADR 0014 item 3):
  - `delegated_token_limited_to_listed_repos_and_mode`;
  - `delegated_token_cannot_widen_itself` (no route or CLI accepts a
    delegated token for `delegate`, `issue`, label changes outside its
    repos, or `create`);
  - `delegated_token_cannot_mint_tokens`;
  - `revoking_parent_revokes_delegated` (revoking the access token a
    delegated token was minted with refuses the delegated token too);
  - `delegated_mint_requires_parent_access_now` and
    `parent_losing_access_revokes_use`;
  - `delegated_ttl_bounded_and_cas_tokens_end_with_it`;
  - `byte_cap_counts_upload_and_granted_bytes`,
    `op_cap_counts_cas_token_requests`,
    `exhausted_within_two_seconds_on_every_node` (two `serve` processes);
  - `revocation_stops_outstanding_cas_tokens`;
  - `reindexed_capped_token_comes_back_exhausted` (index rebuilt from the
    bucket's token records);
  - `static_token_cannot_use_delegated_subject_prefix` (config problem);
  - compat (agent leg): `test_agent_delegated_upload_download` (a human
    OIDC identity delegates write on one repo; an `hf upload` with the
    delegated token works there and is refused on another repo, with the
    hint).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement**; audit rows attribute actions as
  `delegated:{id}` on behalf of the parent subject (the M4 audit API
  reads them); the mint answer shows the token once with copy-ready
  `hf` and `openxet` commands.
- [ ] **Step 4: Write** `docs/guides/agent-tokens.md` (human first, as a
  control-surface guide per ADR 0014): mint, hand to an agent, watch
  usage with `token list --json`, revoke; the caps' two-second overshoot
  and the granted-bytes caveat in plain words.
- [ ] **Step 5: Run** every suite, the agent leg and the map checks.
  Expected: PASS.
- [ ] **Step 6: Commit** as `feat(auth): delegated, attenuated agent tokens with byte and operation caps`.

### Task 15: Rate limits, in-flight limits and memory shares (issues #57, M1-R40)

**Files:**
- Create: `crates/http/src/limits.rs`, `crates/http/tests/limits.rs`, `crates/cas/tests/memory_shares.rs`, `compat/tests/test_limits.py`
- Modify: `crates/http/src/{lib,middleware}.rs`, `crates/cas/src/{budget,auth,reconstruction}.rs`, `crates/hub/src/authz.rs`, `crates/openxet/src/config.rs` (`[limits]`, `server.credential_memory_share`, `server.tenant_memory_share`), `docs/reference/config.md` (Limits; the I-2 fairness sentence #57 asks for), `docs/reference/exit-codes.md` (429)

**Interfaces:**
- Consumes: the resolved `Credential` from Tasks 11 and 13; the per-tenant overrides Task 16 adds (this task reads them through a trait with a config-only implementation).
- Produces:

```rust
pub struct LimitKey { pub tenant: String, pub credential: [u8; 32] /* SHA-256 of the credential's id */ }
pub trait TenantLimits: Send + Sync { fn limits_for(&self, tenant: &str) -> RateLimits; }
pub struct Limiter { .. }
impl Limiter {
    pub fn check(&self, key: &LimitKey) -> Result<InFlight, Limited>;   // InFlight drops to release; Limited → 429 rate_limited, Retry-After
    pub fn auth_failure(&self, ip: IpAddr) -> Result<(), Limited>;
}
impl MemoryBudget {
    pub async fn reserve_for(&self, who: &LimitKey, bytes: u64, kind: ReserveKind) -> Result<Reservation, CasError>;  // shares per M3-R13
}
```

- [ ] **Step 1: Write the failing tests:**
  - `over_rate_answers_429_with_retry_after`,
    `tenants_do_not_share_buckets`, `in_flight_limit_releases_on_drop`,
    `probes_exempt`, `auth_failures_limited_per_ip`;
  - `credential_share_caps_one_reader` (one credential's answers stop
    at a quarter of the budget while another credential still reserves),
    `tenant_share_caps_many_credentials`;
  - #57: `ranged_reconstruction_reserves_only_its_range`,
    `reconstruction_reserves_once`,
    `answer_too_large_never_for_a_fitting_range`;
  - compat: `test_default_limits_never_throttle_stock_client` (4 GiB up
    and down with defaults, the server log shows no `429`),
    `test_rate_limited_client_recovers` (limits set low: `hf_xet`
    retries and completes).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement**; if the first compat test shows a default
  throttling a stock client, change the default and M3-R11 together,
  with the measured request rate.
- [ ] **Step 4: Run** every suite, `measure-memory-bounds.py` with two
  credentials (record peaks in `benchmarks.md`), and the compat test.
  Expected: PASS.
- [ ] **Step 5: Commit** as `feat(server): per-tenant and per-credential rate limits and memory shares (#57)`.

### Task 16: Per-tenant quotas and the tenant-limits admin API

**Files:**
- Create: `crates/index/src/limits.rs` (trait), `crates/index/src/{sqlite,postgres}/limits.rs`, migrations (`tenant_usage`, `tenant_limits`: expand), `crates/hub/src/admin.rs`, `crates/hub/tests/admin_limits.rs`, `crates/cas/tests/quotas.rs`
- Modify: `crates/storage/src/records.rs` (`TenantLimitsRecord`, `tenant-limits/`), `crates/index/src/{sqlite,postgres}.rs` (stored-bytes counter in the owner insert), `crates/index/src/rebuild.rs` (rebuild counters from owner records), `crates/cas/src/xorbs.rs` (quota check before reading the body), `crates/http/src/limits.rs` (an index-backed `TenantLimits`), `docs/reference/config.md`, `docs/reference/openapi.json`

**Interfaces:**
- Consumes: Task 13's admins, Task 15's `TenantLimits`.
- Produces:

```rust
pub struct TenantUsage { pub stored_bytes: u64 }
pub struct TenantLimitsRow { pub quota_bytes: Option<u64>, pub rates: Option<RateLimits> }
#[async_trait] pub trait Limits {
    async fn tenant_usage(&self, tenant: &str) -> Result<TenantUsage, IndexError>;
    async fn tenant_limits(&self, tenant: &str) -> Result<Option<TenantLimitsRow>, IndexError>;
    async fn set_tenant_limits(&self, tenant: &str, row: &TenantLimitsRow) -> Result<(), IndexError>;
}
```

  - `PUT|GET /api/openxet/v1/admin/tenants/{tenant}/limits`.

- [ ] **Step 1: Write the failing tests:** `quota_refuses_xorb_before_body_read`
  (`403 quota_exceeded` with quota, usage and hint; no bytes read),
  `quota_counts_owned_xorbs_alone` (a xorb owned by two tenants counts
  for each), `dedup_hit_uses_no_quota` (a shard-only upload passes at
  quota), `reads_never_stop_at_quota`, `stored_bytes_rebuilt_from_owner_records`,
  `tenant_limits_record_reaches_bucket_first`,
  `admin_limits_require_admin`, `limits_override_reaches_every_node_within_10s`;
  compat: `test_quota_error_renders_in_hf_cli` (the message and hint
  reach the user; confirm with the stock client that `403` fails fast
  rather than retrying, and record it in M3-R12).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** every suite and the map checks. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(server): per-tenant storage quotas and the tenant-limits admin API`.

### Task 17: Bound one writer's influence on dedup answers (issue #50)

**Files:**
- Modify: `crates/index/src/lib.rs` (`dedup_shard_xorbs` takes the caller's tenant), `crates/index/src/{sqlite,postgres}.rs`, `crates/index/src/conformance/cas.rs`, `crates/cas/src/dedup.rs`, `crates/cas/tests/dedup.rs`

**Interfaces:**
- Produces: `async fn dedup_shard_xorbs(&self, xorb: &MerkleHash, caller_tenant: &str, visibility: Visibility, budget: u64) -> Result<…, IndexError>` with M3-R17's preference.

- [ ] **Step 1: Write the failing tests:** `global_prefers_callers_own_shard`
  (tenant A registers shard S1, tenant B later registers S2 over the
  same xorb: A's answer is built from S1), `global_falls_back_to_newest_any_tenant`,
  `per_tenant_unchanged` (the M1 per-tenant tests still pass unedited).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement** in both backends.
- [ ] **Step 4: Run** the index and CAS suites. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(cas): prefer the caller's own shards in global dedup answers (#50)`.

### Task 18: Zero-copy answer segments and streaming xorb validation (issue #54)

**Files:**
- Modify: `crates/cas/src/{answer,send_body}.rs`, `crates/cas/tests/reconstruction.rs`; if step 1 allows, `crates/cas/src/xorbs.rs`, `crates/protocol/src/xorb.rs`, `crates/storage/src/lib.rs` (a `staging/` key kind never listed or served)

**Interfaces:**
- Produces: answers written as independently allocated segments of at
  most 1 MiB, sent without copying (the #54 comment's proposal); if
  upstream allows, `openxet_protocol::XorbValidator` that takes chunks as
  they arrive and yields the verified chunk table at the end.

- [ ] **Step 1: Check upstream** (`xet-core-structures` =1.7.0): can a
  xorb be validated chunk by chunk with its own types (R29: no format
  logic of our own)? Record the answer, with the file and line, in the
  PR. If not, open an issue upstream (or reuse one), keep streaming
  validation out of this task, and leave #54 open for that half only.
- [ ] **Step 2: Write the failing tests:** `answer_segments_at_most_1mib`,
  `stalled_client_pins_at_most_one_segment_past_deadline` (replaces the
  copy-based test at segment granularity), and, if step 1 allows,
  `streamed_xorb_validation_peak_is_one_chunk`,
  `failed_streamed_xorb_leaves_nothing_readable`.
- [ ] **Step 3: Run.** Expected: FAIL.
- [ ] **Step 4: Implement**; staged bytes are never readable and never
  count for dedup or verification (design §5.5's grant rule).
- [ ] **Step 5: Measure** concurrent clients per GiB before and after
  with `measure-memory-bounds.py`, and the latency gate's p99, in
  `benchmarks.md`.
- [ ] **Step 6: Commit** as `perf(cas): zero-copy answer segments (#54)`, plus `perf(cas): validate xorbs while streaming (#54)` if step 1 allows.

### Task 19: The load harness, generator and single-node scenarios (issue #55)

**Files:**
- Create: `crates/loadgen/{Cargo.toml,src/main.rs,src/scenario.rs,src/ops.rs,src/histogram.rs,src/report.rs}`, `loadtest/{README.md,pyproject.toml,uv.lock,run.py,thresholds.toml}`, `loadtest/scenarios/{steady_mix,spike,thundering_herd,slow_clients,dedup_heavy,dedup_free,soak}.toml`, `loadtest/tests/test_run.py`, `docs/reference/load-testing.md`
- Modify: `Cargo.toml` (member), `deny.toml` if needed, `docs/SUMMARY.md`, `AGENTS.md` (Commands)

**Interfaces:**
- Consumes: Task 8's `serve` on production backends.
- Produces:
  - `openxet-loadgen run --scenario FILE --target URL --token-file F --json` →
    `openxet.loadtest.v1` `{revision, machine, config, scenario, ops: {name: {count, errors: {code: n}, p50, p99, p999, histogram}}, memory: {peak, budget}, throughput}`.
  - `uv run --directory loadtest python run.py SCENARIO [--nodes N] [--backend sqlite|postgres] [--duration D]` starts the stack, runs `openxet-loadgen` and stock `hf_xet` clients as the scenario says, collects server memory from `/metrics` or the process, and checks `thresholds.toml`, exiting non-zero on a breach.
  - Scenario keys: `duration`, `ops` (weights over `reconstruct`,
    `upload_xorb`, `upload_shard`, `dedup_query`, `head`), `rate`
    (requests per second, or a ramp), `clients` (native or `hf_xet`
    processes), `tenants`, `file_mix`, `dedup_ratio`.

- [ ] **Step 1: Write the failing tests:** `loadgen` unit tests
  (`scenario_parses_and_validates`, `histogram_merges_exactly`,
  `generated_xorbs_verify`, `dedup_ratio_is_honoured`) and
  `loadtest/tests/test_run.py::test_thresholds_fail_the_run` (a
  threshold set to 0 ms fails a 10 s run with exit code 1 and names the
  breach).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement**; the steady mix uses §5.8's rates
  (reconstruction 1,000/s, 2,000 xorb and 200 shard registrations/s at
  full scale; the scenario scales them by a `scale` factor so the
  nightly runner can run it); the spike is 10× for 60 s; the thundering
  herd is 500 clients pulling one new file; slow clients read at 64 KiB/s
  and hold idle connections up to `server.max_connections`.
- [ ] **Step 4: Run** each scenario for 60 s on one node locally and
  record the method in `docs/reference/load-testing.md`.
- [ ] **Step 5: Commit** as `feat(loadtest): native generator, scenarios and threshold checks (#55)`.

### Task 20: Multi-node, the node kill, the soak and the reconstruction p99 under load (issues #55, #43)

**Files:**
- Create: `loadtest/compose.yaml` (Postgres 17, MinIO, N `openxet` nodes from one image, HAProxy 3 round robin), `loadtest/haproxy.cfg`, `loadtest/drills/{kill_node,rolling_upgrade}.py`, `loadtest/scenarios/reconstruct_p99.toml`, `.github/workflows/loadtest-nightly.yml`
- Modify: `loadtest/run.py` (`--nodes`), `docs/reference/benchmarks.md`

**Interfaces:**
- Consumes: Task 19's runner, Task 7's direct downloads, Task 5's migration policy.
- Produces: `run.py --nodes N`, the `kill_node` and `rolling_upgrade` drills, and the nightly workflow.

- [ ] **Step 1: Write the failing checks:**
  - `kill_node`: during the steady mix, `SIGKILL` one node; every write
    the generator saw acknowledged is then readable through each
    surviving node, and errors are limited to requests in flight on the
    killed node (requirement 7, non-negotiables 2 and 5);
  - `rolling_upgrade`: nodes on the merge-base build and on the head
    build share one database through a node-by-node upgrade under load,
    with zero errors outside the documented ones (M3-R6);
  - `reconstruct_p99`: 1,000 reconstructions/s against an index of
    10^7 xorbs (seeded by the generator through the CAS, not by SQL)
    with concurrent uploads; p99 < 50 ms (#43).
- [ ] **Step 2: Run** on 3 nodes in compose. Expected: the drills fail
  until the compose stack and drills exist; then record where p99
  stands. If `reconstruct_p99` misses, profile and either speed up the
  single-thread render or state the capacity per node it holds at, in
  `benchmarks.md` and #43, before closing it.
- [ ] **Step 3: Nightly workflow:** builds the image, runs the steady
  mix, spike, herd, slow clients, dedup scenarios, `kill_node`,
  `rolling_upgrade` and a 30 min soak at the nightly scale, uploads the
  JSON results as artifacts, and fails per M3-R16.
- [ ] **Step 4: Owner-run step:** the release rig (N ≥ 4 node hosts):
  scaling 1, 2, 4 and N nodes on the steady mix (near-linear means at
  least 0.8 × N of one node's throughput at N), the 24 h soak (flat RSS,
  no leaked permits or file descriptors), and `reconstruct_p99` at full
  scale; results with machine, method and raw data in `benchmarks.md`.
- [ ] **Step 5: Commit** as `feat(loadtest): multi-node rig, node kill, rolling upgrade, soak and the p99 under load (#55, #43)`.

### Task 21: Many tenants, one noisy, and abuse load

**Files:**
- Create: `loadtest/scenarios/{many_tenants,abuse}.toml`
- Modify: `loadtest/run.py` (per-tenant reporting), `loadtest/thresholds.toml`, `docs/reference/benchmarks.md`

**Interfaces:**
- Consumes: Tasks 15 and 16 (limits, shares, quotas), Task 20's rig.

- [ ] **Step 1: Write the checks:** `many_tenants`: 50 tenants at the
  steady mix and one at 20× its rate limit: the quiet tenants' p99 stays
  within 20 % of the run without the noisy one, the noisy one gets
  `429`s only; `abuse`: one credential opening 4× its in-flight limit,
  slow-reading answers and retrying `503`s instantly: the memory budget
  holds, other credentials keep their share, and quota-exceeding
  uploads are refused before their bodies are read (#57's fairness
  sentence is observed here).
- [ ] **Step 2: Run** on 3 nodes and record the numbers.
- [ ] **Step 3: Add** both to the nightly at its scale.
- [ ] **Step 4: Commit** as `test(loadtest): noisy-tenant and abuse scenarios verify limits, shares and quotas`.

### Task 22: The key-rotation drill under load (issue #65)

**Files:**
- Create: `loadtest/drills/key_rotation.py`, `docs/guides/key-rotation.md`
- Modify: `.github/workflows/loadtest-nightly.yml`, `docs/SUMMARY.md`, `docs/reference/config.md` (links)

**Interfaces:**
- Consumes: Task 10's verify keys and key report; Task 20's rig.

- [ ] **Step 1: Write the drill:** while the steady mix runs on 3 nodes,
  rotate the CAS-token key, then the transfer key, by M3-R15's steps,
  each step a rolling restart. Checks: zero failed requests caused by
  the rotation; tokens and URLs signed with the old key work until their
  `exp`; new ones work once published; an unknown `kid` and a mismatched
  `alg` are refused; the old key is refused once removed; `config check`
  reports the key set at each step.
- [ ] **Step 2: Write** `docs/guides/key-rotation.md` from the drill's
  steps, for operators and for platforms that sign their own tokens; the
  docs leg runs its commands against a single node.
- [ ] **Step 3: Run** the drill locally and in the nightly. Expected:
  PASS.
- [ ] **Step 4: Commit** as `test(loadtest): CAS-token and transfer key rotation drill under load (#65)`.

### Task 23: Release gate, published results, docs and project state

**Files:**
- Modify: `docs/reference/benchmarks.md` (every M3 number with machine, method and raw data links), `docs/reference/security-tests.md` (no M3 row pending), `docs/superpowers/specs/2026-10-23-openxet-design.md` (§12 M3 marked done, PQ TLS noted as shipped in #89), `docs/README.md`, `docs/SUMMARY.md`, `README.md`, `CHANGELOG.md`, `AGENTS.md` (Current state: M3 complete, next per ADR 0013; Commands: Postgres and emulator variables, `loadtest` commands), `website/index.html` only if a capability claim changes (per `design.md`)

- [ ] **Step 1: Release gate:** document the release-rig run as a
  release checklist item in `GOVERNANCE.md`'s release section (or the
  release doc it points at) with the exact command, and the thresholds
  file as the gate.
- [ ] **Step 2: Run** `scripts/build-site.sh`, every compat leg, the
  map checks, `scripts/check-openapi.sh`, `check-copy.py` and
  `check-brand.py`. Expected: PASS.
- [ ] **Step 3: Final review** of the whole M3 branch set by a fresh
  reviewer against design §12 M3, §5.9, ADR 0002's open list and this
  plan's Review Focus.
- [ ] **Step 4: Close or update** #43, #47, #48, #50, #54, #55, #57 and
  #65 with links to the merged work and the published numbers.
- [ ] **Step 5: Commit** as `docs: production backends, identity and agent-token guides, M3 results and state`.

## Out of M3, by design

Assigned to later milestones by the design, not deferred for convenience:
- **M4:** `openxet gc` (it sweeps `probe/` and `staging/` leftovers),
  `openxet reindex` as a command (it restores token, revocation and
  tenant-limit records too), readiness that checks the index and the
  bucket (#49), metrics beyond the gauges named here, metering and its
  admin API (built on Task 16's counters), the audit API over the rows
  Tasks 13 and 14 write.
- **M5:** buckets as delegated-token targets (`buckets` in the delegate
  body is rejected with a hint until then).
- **M6:** `openxet login` (the OIDC device and browser flow), the web UI
  for delegated tokens, MCP tools for token minting, approval gates,
  budgets and alerts.
- **M8:** the stand-alone billing ADR, capacity tests against a
  production-size staging stack with this harness.

## Decisions needing the owner

1. **#50 policy (M3-R17).** Own-tenant preference is planned; the
   alternative is ranking shards by how many tenants reference them.
   Task 17 builds whichever the owner picks; both fit the same index
   call.
2. **Cloud accounts.** Tasks 6, 7 and 20 have owner-run steps that need
   an AWS account, a Cloudflare R2 token, a GCS project, an Azure
   storage account, and a release rig of at least four node hosts plus a
   Postgres host. CI covers everything else with emulators.
3. **Default limits (M3-R11, M3-R13).** The planned defaults are
   generous for self-hosters; openxet.com's free tier will want lower
   per-tenant values, set through the admin API, not new defaults.
4. **Access-token lifetime.** 90 days by default and at most 365 days
   (M3-R9); the owner may prefer shorter for openxet.com.
