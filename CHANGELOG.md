# Changelog

What changes for users and operators, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

The storage-only Xet CAS (milestone M1). Stock `hf_xet` 1.7.0 and newer
upload to and download from it directly; the
[quickstart](docs/quickstart-storage-only.md) runs it in under a second
once built.

### Added

- `openxet serve`: the Xet CAS on local disk with a SQLite index. Without
  a configuration file it runs in dev mode and prints one write token;
  with one, it takes static tokens and Ed25519-signed CAS tokens
  ([config](docs/reference/config.md)).
- `openxet config check`, and `openxet token keygen`, `mint` and
  `generate` for credentials.
- `openxet upload` and `openxet download`, on the same client core as
  `hf_xet`, with `--json` output and stable exit codes
  ([command line](docs/reference/cli.md)).
- Every TLS connection the `openxet` binary makes (the client core's and
  `openxet label`'s) offers the hybrid post-quantum key exchange
  `X25519MLKEM768` first, with X25519 kept for servers without it
  ([ADR 0011](docs/adr/0011-post-quantum-readiness.md)).
- The CAS routes ([routes](docs/reference/routes.md)):
  - xorb upload with validation of every chunk
  - shard upload, v1 and the v2 progress stream, verified against the
    stored chunk hashes before any file is registered
  - global dedup, answering with HMAC-keyed shards that offer every xorb
    of the newest upload holding the matched chunk
  - reconstruction v1, v2 and batch, with transfer URLs signed for one
    byte range each
  - telemetry accepted and dropped
  - `GET /healthz` and `GET /readyz`; readiness probes the index with a
    read and the bucket with a small write, and answers
    `503 unavailable` naming the failing one
- The OpenAPI 3.1 document of every route the server serves, generated
  from the routes themselves: `GET /openapi.json` and `openxet openapi`
  ([OpenAPI document](docs/reference/openapi.md)).
- Every security requirement of design §5.9 that applies to the CAS, each
  with its test ([security tests](docs/reference/security-tests.md)).
- Reconstruction answers a 100k-term file within 50 ms at p99 on an idle
  node, gated in CI ([benchmarks](docs/reference/benchmarks.md)).
- Memory and connection bounds that hold whatever clients do: upload
  bodies and reconstruction answers reserve from one budget,
  `server.memory_budget` (by default a quarter of the host's memory).
  Uploads past it queue for up to 120 s, then get `503 memory_busy`,
  which clients retry; reconstructions that fit pass the queue, and
  others wait 5 s. One answer may hold at most an eighth of the budget,
  else a final `413 answer_too_large` (ask by range); a batch holds at
  most 100,000 terms. An answer not sent within 30 s plus 1 s per MiB is
  cut off. `server.max_connections` (4096 by default) caps open
  connections ([config](docs/reference/config.md#memory)).
- Tenants: every credential names a tenant next to its scope, from a CAS
  token's optional `tenant` claim or a static token's `tenant` field, and
  `scope:` followed by the scope when it names none; explicit tenants may
  not start with `scope:`. `dedup_visibility = "per-tenant"` isolates
  tenants while the scopes of one tenant share dedup
  ([config](docs/reference/config.md#tenants)). The commands
  `openxet token mint` and `token generate` take `--tenant`.
- The bucket records which scope registered which shard, for which tenant,
  and which tenant uploaded which xorb, before the index does, so losing
  the index never loses anyone's access to their files: the index can be
  rebuilt from the bucket alone.

### Upgrading

- `dedup_visibility = "per-scope"` is now `"per-tenant"`; the old value is
  refused at startup with a hint. With no tenant configured, each scope
  is its own tenant, which isolates exactly as `per-scope` did.
- Index migration 0005 gives every existing row the tenant `scope:` and
  its scope, the tenant of a credential that names none,
  and makes each scope an owner of the xorbs its registered shards
  reference where the index lists them (shards registered after migration
  0002), so switching `global` to `per-tenant` keeps their files readable
  and dedupable. A shard registered before 0002 gains that ownership when
  it is registered again.
- Registrations and xorb uploads made before this version have no record
  in the bucket. The index still holds them, but a rebuild from the bucket
  (`reindex`, [#26](https://github.com/stubbi/openxet/issues/26)) can't
  restore them until each shard and xorb is uploaded again, which writes
  its record. `reindex` will backfill the records from a live index.

- Shards registered before index migration 0002 have no list of their
  xorbs, so a dedup match in one offers only the matched xorb, not the
  rest of that upload. A xorb gets the full answer again once a shard
  that references it is registered after the migration: the same shard
  uploaded again, or a new upload of any file that uses the xorb. A
  `reindex` command that rebuilds the index from the bucket
  ([#26](https://github.com/stubbi/openxet/issues/26)) will fill in every
  shard.
- Index migrations 0003 and 0004 record each xorb's chunk layout and each
  file's per-term byte ranges, which make reconstruction fast. Rows from
  before them are still answered correctly, only more slowly:
  - A xorb stored before 0003 has no layout, so planning reads its whole
    chunk table, until the xorb is uploaded again, which records it.
  - A file registered before 0004 has no term byte ranges, so every
    reconstruction of it reads the layouts of all its xorbs. Registering
    the file again keeps the existing row, so this lasts until `reindex`
    ([#26](https://github.com/stubbi/openxet/issues/26)) rebuilds the
    index.
