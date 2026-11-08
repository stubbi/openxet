# 0015: Run on Postgres and object-store buckets, and sign range-bound presigned URLs ourselves

<!-- description: Postgres is a second implementation of the index traits with hash partitions, an optional read replica that never answers "absent" alone, object-store buckets on S3, GCS and Azure behind a conditional-create probe, and OpenXet's own SigV4 and GCS V4 signers that bind the Range header, gated per pool by ADR 0002's probe. -->

- Status: accepted
- Date: 2026-11-05
- Scope: `index`, `storage`, `cas`, `server`, `docs`
- Rulings: M3-R2 to M3-R5 (design §17)

## Context

M1 and M2 run on SQLite and a local-disk bucket. Design §12 M3 asks for
Postgres, S3, GCS and Azure, and presigned downloads; design §5.8 sets
the topology: OpenXet nodes scale out, Postgres scales up, with one
primary, read replicas for reconstruction and listing reads, and
partitioning by hash prefix. The index trait must not assume a single
instance, so a sharded backend can come later without touching callers.

ADR 0002 makes every download URL bind one xorb, one byte range and one
expiry, and gates direct downloads per pool by a range-binding probe.
Its "Open, owned by M3" list asks for the probe against real AWS S3,
R2, Garage, GCS and Azure, and for a signing benchmark.

What we checked, and how:
- **`object_store`'s signer can't bind a range.** In `object_store`
  0.14.0 (read from the crate source), `Signer::signed_url` takes a
  method, a path and an expiry and nothing else (`src/signer.rs`), and
  the S3 presigner sets `X-Amz-SignedHeaders=host` (`src/aws/credential.rs`).
  A URL it signs reads the whole object, which ADR 0002 rule 2 forbids.
- **SigV4 and GCS V4 can bind a range.** Both query-signing schemes take a
  list of signed headers (`X-Amz-SignedHeaders`, `X-Goog-SignedHeaders`).
  Spike S1/S2 observed MinIO enforcing `host;range` and stock `hf_xet`
  downloading through such URLs (ADR 0002, Context). Whether AWS S3, R2,
  Garage and GCS enforce it is not yet observed; ADR 0002's probe decides
  per pool, never a list.
- **Azure SAS has no field that binds a byte range.** A SAS token signs
  permissions, a time window, a resource and optional IP and protocol
  limits, and nothing about the request's `Range`.
- **Registration records rely on create-if-absent** (ruling M1-R37). On
  an object store that is `PutMode::Create`. An S3-compatible store that
  accepts `If-None-Match: *` and overwrites anyway would let two writers'
  records diverge, so the server must not run on one.
- **SQLite's single writer hid every write race.** Two nodes registering
  one shard at once, or committing to one repository, must both succeed
  or one win cleanly on Postgres, never answer `500` from a unique
  violation or a serialization failure.

## Decision

1. **Postgres is a second implementation of the existing traits.**
   `Index`, `Repos` and `GitStore` get a Postgres implementation in
   `openxet-index`, with its own migrations, and both backends pass one
   backend-agnostic conformance suite. The schema mirrors SQLite's
   (hashes as `BYTEA`, times as `BIGINT` Unix seconds). `xorbs`,
   `xorb_layouts`, `xorb_owners`, `file_scopes` and `dedup` are
   `PARTITION BY HASH` on their hash column with 16 partitions; changing
   the count is a migration. Chunk tables and layouts stay per-xorb
   blobs, never per-chunk rows (design §5.8). The registration sequence
   is a Postgres `SEQUENCE`.
2. **Races are success, or one clean winner.** Inserts of
   content-addressed rows use `ON CONFLICT DO NOTHING` and read back, so
   a lost race is success. The row of the lowest `(seq, shard, tenant)`
   (ruling M1-R37) is kept by a conditional upsert. A ref commit locks
   the repository row (`SELECT ... FOR UPDATE`) in the transaction that
   checks the fence (ADR 0004 item 3). Write transactions run at
   `READ COMMITTED`, set per transaction, so Postgres aborts one for a
   conflict only on a deadlock (`40P01`); that is retried up to 3 times,
   then answered `503 index_busy` with `Retry-After: 1`.
3. **Configuration.** `[index] url` takes a `sqlite:` path or a
   `postgres://` URL (default: the data directory's `index.sqlite`),
   `max_connections` (default 4 × CPUs, bounds 2 to 1024) and an optional
   `read_url` for a replica.
4. **A replica never answers "absent" alone.** With `index.read_url`,
   reconstructions, file heads, listings and dedup lookups read from the
   replica. A replica answer of "absent" is read again from the primary
   before anyone is told "absent", so a replica never hides an
   acknowledged write (§5.9 requirement 7). Writes, Git ref snapshots and
   every authorization-bearing read (tokens, revocations, limits) use the
   primary.
5. **Object-store buckets.** `[storage] url` selects the backend:
   `file:///path` (M1's local bucket, default `{data_dir}/bucket`),
   `s3://bucket/prefix`, `gs://bucket/prefix` or `az://container/prefix`,
   with `object_store`'s builder options under `[storage.options]`. The
   key layout is M1's, under the prefix. Credentials come from the
   environment, instance identity or a `*_file` option, never from the
   config file in plain text. A remote store requires an explicit
   `cas.transfer_key_file` and an explicit CAS-token key (ruling M1-R29),
   since every node behind one endpoint needs the same keys.
6. **A conditional-create probe at start-up.** Registration records are
   written with `PutMode::Create`. At start-up each node creates
   `probe/{node uuid}/create` twice and requires the second call to
   report that the object exists. A store that fails is refused with
   `storage_unsafe`, naming the check and the setting; the server never
   runs on it. Probe objects live under a reserved prefix that `list`
   never returns, are reused rather than deleted, and are swept by M4's
   GC.
7. **OpenXet signs presigned GETs itself, with `Range` signed.** One
   small signer per store family: SigV4 query signing with
   `X-Amz-SignedHeaders=host;range` for `s3://` (AWS S3, R2, MinIO,
   Garage), and GCS V4 signing with `X-Goog-SignedHeaders=host;range` for
   `gs://` (service-account key or HMAC key). `az://` pools have no
   presigner and serve through the transfer role. Each signer is tested
   against its provider's published signing examples.
8. **Direct URLs only after the probe passes, in this process.** Each
   pool runs ADR 0002's range-binding probe at start-up and in `openxet
   config check --probe`, against a fixed 64 KiB object at
   `probe/range-binding`. Only a pool whose three checks pass serves
   direct URLs. The outcome is logged, exported as the
   `openxet_storage_direct_downloads` gauge and shown by `config check`.
   A pool that fails is reported as serving through the transfer role,
   with the check that failed, and nothing more.
9. **Presigned lifetime.** A presigned URL lives
   `min(cas.url_lifetime, credential expiry - 60 s)`. When that falls
   under ADR 0002's 30 min floor, the presigner refreshes its
   credentials first; if it still can't reach the floor, that answer's
   URLs are transfer URLs.

## Consequences

- Nothing changes on the wire for stock clients except the URLs inside
  reconstruction answers, which become native presigned URLs on a pool
  that passed the probe. Every fetch entry still has exactly one range
  and one URL (ADR 0002 rule 1).
- Azure pools serve downloads through the transfer role until a SAS can
  bind a range. Their bytes pass through OpenXet nodes, which then scale
  with download traffic; the docs say so where Azure is configured.
- A replica adds one primary read for every "absent" answer. Absent
  answers are rare on the hot path (a client asks for files it was told
  exist), so the cost lands on misses, which is where a lagging replica
  would otherwise be wrong.
- Every write path must now be correct under concurrent writers on
  separate connections. The conformance suite's two-handle cases
  (`concurrent_register_same_shard_two_pools`,
  `concurrent_put_xorb_two_pools`,
  `concurrent_hub_commits_two_pools_one_winner`) and
  `lease_epoch_fences_a_stale_holder_across_pools` hold both backends to
  it.
- We own two signers. They are small, deterministic and checked against
  published vectors (`sigv4_matches_aws_test_vectors`,
  `gcs_v4_matches_reference`); a provider change that breaks one makes
  its probe fail, and the pool falls back to the transfer role rather
  than serving a URL the store won't honour.
- Tests that enforce the security side: `every_direct_url_binds_its_range`,
  `direct_urls_only_after_probe_passed`,
  `probe_falls_back_on_unbound_signer`,
  `presigned_lifetime_capped_by_credential_expiry`,
  `start_refuses_store_without_conditional_create`,
  `replica_miss_reads_primary` and
  `replica_never_used_for_tokens_or_refs` (security test map, §5.9
  requirements 2, 3 and 7).
- ADR 0002's open list is carried out by M3 Task 7: each store's probe
  outcome, with the date and region, goes into
  `docs/reference/object-stores.md`, which says "not yet probed" for a
  store until it is recorded.

## Alternatives considered

- **`aws-sigv4` from the AWS SDK.** It can sign arbitrary headers, but it
  brings the SDK's dependency tree for one function. One signer per
  family is about 200 lines with published test vectors, and GCS needs
  its own anyway. Rejected.
- **`object_store`'s presigner, with unbound URLs.** A URL that reads the
  whole xorb crosses scopes, since xorbs are shared (ADR 0002,
  alternatives). Rejected by ADR 0002 rule 2.
- **A static list of stores that support range binding.** It would trust
  documentation over behaviour and miss proxies, CDNs and provider
  changes. Rejected by ADR 0002 rule 3; the probe decides.
- **A distributed key-value store for the index now.** Design §5.8 keeps
  it for later: partitioned Postgres holds the 1 PB sizing, and the
  traits don't assume one instance, so it can be added without touching
  callers.
- **Read everything from the replica.** It would let a lagging replica
  answer "absent" for an acknowledged write, and serve stale
  authorization data such as a revoked token. Rejected.
