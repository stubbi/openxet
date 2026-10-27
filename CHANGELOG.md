# Changelog

What changes for users and operators, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

The storage-only Xet CAS (milestone M1), and on top of it the Hub API,
Git hosting and Git LFS (milestone M2). Stock `hf` and `huggingface_hub`
2.2.0 and newer upload, download and list repositories; `git clone` and
`git push` work over HTTPS on the same history; and stock `hf_xet` 1.7.0
and newer uploads to and downloads from the CAS directly. The
[hf and git quickstart](docs/quickstart-hub.md) and the
[storage-only quickstart](docs/quickstart-storage-only.md) run as written
in CI.

### Added in M2

- Hub token, resolve and tree answer within 10 ms at p99 on an idle node,
  against 10,000 repositories and a 10,000-file repository; serving a
  clone costs at most twice git's own CPU; both gated in CI
  ([benchmarks](docs/reference/benchmarks.md)). A `tree` page of 1,000
  Xet files went from 53 to 111 ms to 4 to 8 ms at p50 on CI.
- `git.thin_bases.max_objects` defaults to 1,000,000, measured (it was a
  2,000,000 placeholder); `git.thin_bases.max_ms` stays 2,000
  ([config](docs/reference/config.md#gitthin_bases)).
- The Hub API that `hf` and `huggingface_hub` call: `whoami`, repository
  create, delete, info and listing, `preupload`, the Xet token routes,
  commits with regular and Xet files, branches, tags, `refs`, `tree`,
  `paths-info`, `commits`, and `resolve` for downloads
  ([Hub routes](docs/reference/hub-routes.md)). Answers match
  huggingface.co's field for field; Hub endpoints OpenXet doesn't
  implement answer `501` naming themselves.
- Git hosting over smart HTTP: clone, fetch and push with stock `git`,
  protocol v0 and v2, with Hub commits and pushes on one history. The
  first push to a missing repository creates it. Every ref change, pushed
  or committed through the Hub, goes through one fenced transaction; a
  fetch serves only objects reachable from the repository's refs; packs
  live in the bucket, refs in the index, and each node keeps a cache
  ([Git hosting](docs/guides/git-hosting.md),
  [Git routes](docs/reference/git-routes.md)).
- A push that adds a file of 10 MiB or more outside Git LFS names it and
  suggests `git lfs track`; `git.max_plain_blob_bytes` can refuse such
  files, and `git.max_push_bytes` (10 GiB) bounds a push.
- The Git LFS endpoint of every repository: uploads through `git-xet`
  with self-refreshing capability URLs, downloads for plain `git-lfs`
  through the file gateway, and token routes for Xet-native clients.
  Plain `git-lfs` uploads are refused up front with a message naming
  `git-xet` ([Git LFS routes](docs/reference/lfs.md),
  [Git LFS for any Git host](docs/guides/lfs-for-any-repo.md)).
- The file gateway, `/v1/transfer/files/…`, which serves a repository's
  Xet files over plain HTTP with `Range`, `If-Range`, `ETag` and `HEAD`,
  to the repository's readers.
- User credentials and rules: a static token without `scope` acts for its
  `subject` on the Hub, Git and LFS routes, with optional `name`, `email`
  and `claims`, and `[[auth.rules]]` grant `read`, `write` and `create`
  per repository pattern, including to `anonymous`
  ([config](docs/reference/config.md#rules)). A repository the caller may
  not read answers exactly as one that doesn't exist.
- Labels and typed views: every repository carries generic labels;
  `model`, `dataset`, `space` and `kernel` select the Hub's typed routes.
  `openxet label add`, `remove` and `list`, and the labels API under
  `/api/openxet/v1/`, change and list them, each change recorded in an
  audit row ([command line](docs/reference/cli.md#openxet-label)).
- `openxet purge <file-hash>` unregisters a file from every scope, with
  `--dry-run`, `--yes` and `--json`
  ([command line](docs/reference/cli.md#openxet-purge)).
- The `xet://` address specification, with its parser and formatter in
  the `openxet-uri` crate ([xet:// addresses](docs/reference/xet-uri.md)).
- The `[hub]` and `[git]` configuration sections
  ([config](docs/reference/config.md#hub)).
- The compatibility suite's Hub, Git and LFS legs run stock
  `huggingface_hub` 2.2.0, `git`, `git-lfs` 3.7.1 and `git-xet` 0.2.1
  (with Gitea) against `openxet serve` in CI.

### Added in M1

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

- `openxet serve` now needs `git` 2.37 or newer on its `PATH`, and
  refuses to start, naming what to install, without it. Its data
  directory gains `git-cache/`, unless `git.cache_dir` names another
  place.
- Index migrations 0006 and 0007 add the tables for repositories, refs,
  packs, labels, purges and the audit log, and leave the tables M1
  created unchanged.

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
