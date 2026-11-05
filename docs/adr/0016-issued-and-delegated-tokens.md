# 0016: Authenticate with OIDC, authorize through a webhook, and issue access and delegated tokens bound to an identity

<!-- description: OIDC issuers are verified through cached, fail-closed JWKS; an operator's webhook can authorize; OpenXet issues revocable access tokens only against an identity-provider proof, and narrower delegated tokens for agents that can never widen, extend or mint; every token record reaches the object-store bucket before the index. -->

- Status: accepted
- Date: 2026-11-05
- Scope: `auth`, `hub`, `cas`, `index`, `storage`, `server`, `docs`
- Rulings: M3-R7 to M3-R10 (design §17)

## Context

Design §5.4 names the authenticators and authorizers of v1: OIDC, static
tokens and OpenXet access tokens; config rules and a webhook. M2 shipped
static tokens and config rules behind the authenticator and authorizer
seams (ADR 0012). Design §7.1 adds delegated, attenuated tokens, with
which a human hands an agent less than their own access, and ADR 0014
item 3 makes a security property of it: an agent can never widen its own
limits, approve its own gated action or extend its own token.

The constraints these tokens have to meet:
- **OpenXet never owns users** (non-negotiable 4). Every token is bound
  to an external identity, and authorization is re-evaluated on use, so
  revoking access upstream takes effect while the token is still valid
  (design §5.4).
- **Losing the database must never lose data, nor anyone's access to
  it** (design §5.3, non-negotiable 2).
- **Stateless nodes** (non-negotiable 5): any node verifies any token;
  caches are allowed only when they are keyed by the caller and fail
  closed (§5.9 requirement 3).
- **Stock clients don't change.** `hf auth login --token` takes an opaque
  secret, and CAS tokens stay ADR 0007's EdDSA JWTs.
- **Downloads may bypass OpenXet.** On a pool that passed ADR 0002's
  probe, xorb bytes go from the object store to the client, so OpenXet
  sees the ranges it grants, not the bytes a client fetches (ADR 0013's
  caveat).

## Decision

1. **OIDC.** Each `[[auth.oidc]]` entry names an issuer: `name` (the
   subject prefix), `issuer` (an exact match, discovered through
   `{issuer}/.well-known/openid-configuration`), `audiences` (required;
   one must match `aud`) and claim mappings (`subject_claim`, default
   `sub`; `name_claim`, default `preferred_username`, falling back to the
   subject; `email_claim`, default `email`; `claims`, string or
   string-list claims copied into the principal, where `orgs` feeds
   `whoami-v2`).
   - A principal's subject is `{name}:{sub}`, so two issuers never share
     a subject.
   - The accepted `alg` is the one of the JWKS key the `kid` names, from
     RS256, PS256, ES256, ES384 and EdDSA; `none` and HMAC algorithms
     are always refused.
   - `exp` is exact; `nbf` and `iat` allow the configured clock skew.
   - JWKS refreshes every 10 min, and on an unknown `kid` at most once
     per 30 s per issuer. After a failed refresh the last keys serve up
     to `jwks_max_stale` (default `24h`); past it, requests get `503
     authn_unavailable` with a hint. An unverifiable token is never
     accepted.
   - An OIDC token is a user credential, never a CAS credential.
2. **The webhook authorizer.** `[auth.webhook]` takes `url`,
   `secret_file`, `timeout` (default `2s`), `allow_ttl` (default `30s`)
   and `deny_ttl` (default `5s`).
   - OpenXet POSTs `{"schema": "openxet.authz.v1", "subject", "claims",
     "action", "namespace", "name", "delegated_by"}` with
     `X-Openxet-Signature: sha256={hex HMAC-SHA256 of the body}` and
     `X-Openxet-Timestamp`. The answer is `{"allow": bool}` with an
     optional `ttl_seconds`, capped by the configured TTLs.
   - Decisions are cached under the SHA-256 of the whole request body,
     so a decision is never shared between two subjects or two claim
     sets.
   - A timeout, a non-`2xx` answer or an unparseable body is `503
     authz_unavailable`, never an allow.
   - `auth.authorizer` is `rules`, `webhook` or `rules+webhook` (default
     `rules`). With both, a rule grant allows without a call and the
     webhook decides the rest.
3. **OpenXet access tokens.** A secret is `oxt_` and 43 base64url
   characters (ruling M1-R28). Its record holds the SHA-256 of the
   secret, never the secret; a UUIDv7 id; the bound subject, a snapshot
   of its claims and the issuer name; `created_at`, `expires_at`,
   `revoked_at`; and `last_used_at`, in the index only, written at most
   once a minute.
   - Issued by `POST /api/openxet/v1/tokens` with an OIDC credential, or
     by `POST /api/openxet/v1/admin/tokens` by a subject in
     `auth.admins`, for any subject and claims (a platform's backend).
     An access, delegated or static token can't issue one; it gets `403
     needs_identity_provider` with a hint.
   - Lifetime: default 90 days, at most `auth.access_token_max_ttl`
     (default `365d`, bounds `1d` to `3650d`).
   - Every use asks the authorizer again with the bound subject and
     claims.
   - Lookup: static tokens, then a per-node cache of found tokens held
     for 5 s, then the primary index. Unknown secrets are never cached.
4. **Delegated tokens.** Minted by `POST
   /api/openxet/v1/tokens/delegate` with an OIDC or access credential,
   never with a delegated one.
   - The body names `repos` (up to 100 `{namespace}/{name}`, no globs),
     `mode` (`read` or `write`), `ttl` (default `8h`, at most
     `auth.delegated_max_ttl`, default `24h`, bounds `1h` to `720h`),
     optional `max_bytes` and `max_ops`, and a `name` for the audit log.
   - At mint, the parent must be allowed `mode` on every listed
     repository now.
   - On use, a request is allowed only if its target is listed, its
     action is within `mode` (`create` never is), the parent subject is
     still allowed by the authorizer, and the token is not expired,
     revoked or exhausted.
   - Revoking the access token a delegated token was minted with
     revokes the delegated token too.
   - A CAS token minted through a delegated token has `sub =
     "delegated:{id}"` (a reserved prefix, ADR 0007 as amended) and
     lasts at most what the delegated token has left.
   - Ops are authenticated requests made with the token or its CAS
     tokens. Bytes are upload bodies accepted plus the bytes of the
     ranges a reconstruction answer grants.
   - Usage is batched per node and flushed every second; exhaustion and
     revocation are read from a per-node cache at most 2 s old, so a cap
     overshoots by at most two seconds of the token's own traffic.
5. **Token records reach the bucket first.** Issuing writes
   `tokens/{id}` to the object-store bucket, then the index; revoking
   writes `token-revocations/{id}` first. Neither holds a secret. Usage
   counters are index-only, so after `reindex` a capped delegated token
   comes back exhausted.
6. **Decided defaults the owner can revise.** The lifetimes above (90
   days and 365 days for access tokens, 8 h and 24 h for delegated
   tokens) are the defaults M3 ships. openxet.com may set shorter maxima
   through configuration; a change of default is a change to this ADR
   and ruling M3-R9 or M3-R10.

## Consequences

- **Every token traces to an identity-provider proof.** An access token
  can't mint another access token, so a leaked one can't be used to make
  a replacement that survives its revocation, and every token in the
  index names the identity proof it came from.
- **Delegation is one level deep.** A delegated token can't mint a token,
  so the tree under any identity is at most two levels: access tokens,
  and the delegated tokens minted with them or directly with OIDC.
  Revoking one access token stops everything below it, and the audit log
  attributes each action as `delegated:{id}` on behalf of one subject.
- **Caps count what OpenXet grants, not egress.** Bytes served directly
  by the object store never pass a node, so a cap counts the ranges a
  reconstruction grants. A replayed URL isn't counted again. The agent
  tokens guide says so, with the two-second overshoot, in plain words.
- **The authorizer is on the request path for issued tokens.** Every use
  asks it again, so the webhook's latency, behind its cache, is part of
  those requests. The token, head and resolve p99 < 10 ms target of
  §5.8 excludes the webhook; M3 measures a cached decision and records
  it.
- **Losing the database keeps everyone's access.** `reindex` (M4)
  restores tokens and revocations from the bucket. The worst a lost
  database does to a delegated token is end it early, never widen it.
- **New failure modes are closed ones.** An identity provider that is
  down past `jwks_max_stale`, a webhook that times out, or an index that
  can't be read answers `503` with a hint; none of them allows.
- `Authenticator::authenticate` becomes `async`, since OIDC may await a
  JWKS refresh and access tokens an index read.
- Tests that enforce it are in the security test map under §5.9
  requirements 1, 2, 3, 6 and 7, among them
  `alg_none_and_hs256_refused`,
  `jwks_outage_serves_cached_then_fails_closed`,
  `cache_keyed_by_subject_and_claims`, `timeout_fails_closed`,
  `access_token_issued_from_oidc_only`, `unknown_secret_not_cached`,
  `revoked_token_refused_on_every_node`,
  `delegated_token_cannot_widen_itself`,
  `delegated_token_cannot_mint_tokens`,
  `revoking_parent_revokes_delegated` and
  `token_record_reaches_bucket_before_index`.
- Out of M3 by design: `openxet login` (the OIDC device and browser
  flow), the delegated-token web UI, MCP tools for minting, and approval
  gates come in M6; buckets as delegated-token targets in M5; the audit
  API over the rows these routes write in M4.

## Alternatives considered

- **Access tokens that can mint access tokens.** Convenient for scripts,
  but a token could then outlive the identity proof it came from, and
  revoking a leaked token would not revoke what it minted. Rejected.
- **Chained delegation (a delegated token minting a narrower one).** Each
  link would need its own attenuation proof and revocation lookup, and
  ADR 0014 item 3 forbids an agent minting tokens. An agent that needs a
  narrower token asks the human, or a credential the human holds.
  Rejected.
- **Self-contained delegated tokens (macaroons or signed JWTs with
  caveats).** They verify without a lookup, but they can't be revoked
  before `exp` or carry usage caps without shared state, which is the
  point of a delegated token. Rejected; the CAS tokens they mint stay
  short-lived JWTs.
- **Caps on egress bytes.** OpenXet would have to proxy every download,
  giving up direct downloads (design §5.5). Rejected; caps count granted
  bytes, and egress budgets are metering's (M4, M6).
- **Token records in the index only.** Losing the database would end
  every issued token at once, and with it every agent's and platform's
  access. Rejected by design §5.3.
