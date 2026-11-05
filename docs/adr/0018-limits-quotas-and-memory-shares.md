# 0018: Rate-limit per node, enforce quotas globally, and give each credential and tenant a share of the memory budget

<!-- description: Request rate and in-flight limits are per-node GCRA limiters keyed by tenant and credential, with no shared state; stored-byte quotas are global, counted from owned xorbs in the index; one credential or tenant may hold only a share of a node's memory budget. -->

- Status: accepted
- Date: 2026-11-05
- Scope: `http`, `cas`, `hub`, `index`, `storage`, `server`, `docs`
- Rulings: M3-R11 to M3-R13 (design §17); issue [#57](https://github.com/stubbi/openxet/issues/57)

## Context

Design §12 M3 asks for per-tenant rate limits and quotas, and
per-credential shares of the memory budget, verified under abuse load.
Design §15 lists noisy-neighbour limits and enforced quotas among what
openxet.com needs from the core, built into the open-source build behind
the same seams self-hosters use.

M1 bounds each node's memory with one process-wide budget (ruling
M1-R40), and its CPU work with permits, but any one client may take all
of either: one credential reading large answers can make every other
caller wait for `503 memory_busy`. Issue #57 asks for a fairness rule.

Two kinds of limit pull in different directions:
- **Request rates protect a node.** They have to be checked on every
  request, in microseconds, and what they protect (CPU, connections,
  memory) is per node.
- **Stored bytes are a deployment-wide fact.** A tenant's usage is the
  same whichever node it asks, and exceeding a quota on one node must
  not be possible by asking another.

Stateless nodes (non-negotiable 5) allow node-local state only as caches
that fail closed. "Nothing to babysit" (who we build for) argues against
a new service to run.

## Decision

1. **Rate limits are per node.** GCRA limiters (`governor`), keyed per
   tenant and per credential, under `[limits]`:
   - `tenant_requests_per_second`: default 2000, burst 4000;
   - `credential_requests_per_second`: default 1000, burst 2000;
   - `max_in_flight_per_credential`: default 512;
   - `auth_failures_per_minute` per client IP: default 60.

   A credential is keyed by the SHA-256 of its id, never by the secret.
   Over a limit, the answer is `429 rate_limited` with `Retry-After` in
   whole seconds and a hint. The probes are exempt. The deployment-wide
   figure is the per-node one times the number of nodes, and the docs
   say so.
2. **Quotas are global and byte-denominated.** A tenant's stored bytes
   are the sizes of the xorbs it owns (ruling M1-R39), counted as if it
   were alone, as stand-alone billing will be (design §15). The counter
   is kept in the transaction that records an owner and rebuilt from the
   `xorb-owners/` records by `reindex`.
   - A xorb upload that would take its tenant past its quota answers
     `403 quota_exceeded` with the quota, the usage and a hint, before
     the body is read. A dedup hit costs no quota, and reads never stop.
   - `[limits] default_tenant_quota` is unlimited by default.
   - Per-tenant overrides of the quota and the rate limits are set with
     `PUT /api/openxet/v1/admin/tenants/{tenant}/limits`, written to
     `tenant-limits/{sha256(tenant)}` in the bucket, then the index, and
     read by nodes through a 10 s cache.
3. **Memory shares.** One credential may hold at most
   `server.credential_memory_share` of the memory budget (default
   `0.25`, bounds `0.01` to `1`), and one tenant at most
   `server.tenant_memory_share` (default `0.5`). A reservation over its
   share waits by the rules of one over the budget (M1-R40), then gets
   `503 memory_busy`.
   - A reconstruction reserves once, from the upper bound its stored term
     count gives; a ranged request reserves only its range's terms. So
     `413 answer_too_large` is given only to an answer too large for any
     range.
4. **Decided defaults the owner can revise.** The defaults above are
   generous for self-hosters. openxet.com's free tier sets lower
   per-tenant values through the admin API, not through new defaults. M3
   checks with the stock client that the defaults never throttle a
   default `hf_xet` transfer, and changes them, with the measured rate,
   if they do.

## Consequences

- A limit check costs no network round trip, and a node needs nothing
  but itself to enforce one.
- A client spreading requests over N nodes gets up to N times the
  per-node rate. That is the documented behaviour: rate limits protect
  nodes; what must hold deployment-wide is stored bytes, and quotas hold
  it.
- A quota is exact at the index's transaction. A tenant uploading
  through several nodes at once may overshoot by the xorbs already in
  flight when its counter crossed the quota, at most one xorb (64 MiB)
  per concurrent upload.
- Per-tenant overrides take up to 10 s to reach every node.
- One credential can no longer starve a node's memory; one tenant with
  many credentials is held by the tenant share.
- `429` and `403 quota_exceeded` are new answers stock clients meet.
  M3's compatibility tests show `hf_xet` retrying a `429` until the
  transfer completes, and the quota error's message and hint reaching
  the user (`test_rate_limited_client_recovers`,
  `test_quota_error_renders_in_hf_cli`).
- Tests: `over_rate_answers_429_with_retry_after`,
  `tenants_do_not_share_buckets`, `in_flight_limit_releases_on_drop`,
  `credential_share_caps_one_reader`,
  `tenant_share_caps_many_credentials`,
  `quota_refuses_xorb_before_body_read`, `admin_limits_require_admin`,
  `test_default_limits_never_throttle_stock_client`, and the
  `many_tenants` and `abuse` load scenarios.

## Alternatives considered

- **Shared counters in Postgres per request.** Exact deployment-wide
  rates, at the price of a database round trip on every request, which
  would put the index on the critical path of every chunk query and
  transfer. Rejected.
- **A Redis tier for counters.** Exact and fast, but new state to run,
  back up and secure, against "nothing to babysit", and one more thing
  whose outage must fail closed. Rejected.
- **Quotas per node.** A tenant could store N times its quota by
  spreading uploads. Rejected; stored bytes are global.
- **No shares, only rate limits.** A credential under its request rate
  can still hold the whole memory budget with a few large answers.
  Rejected; issue #57's fairness needs the share.
