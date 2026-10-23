# 0005: Serve Xet-native clients from the LFS-scoped token route

- Status: accepted
- Date: 2026-10-23
- Scope: `hub`, `git`, `auth`, `client`
- Amended by: ADR 0003 (item 10: downloads answer `basic`; capability
  `href` details), from spike S7

## Context

Design §6.1 serves `git-xet` through the Git LFS Batch API. A third-party
Xet-native client, jj-xet (Jujutsu with Xet-backed large files), chunks and
uploads before it knows any LFS oid, so it cannot start from a batch request.
It was implemented against the contract below, which openxet's maintainers
committed to on 2026-10-23. This ADR makes the contract binding, reviewable
and testable.

The token response shape is identical to Hugging Face's Hub token route.
That was checked against the Xet spec's "auth" page, the huggingface.co
token exchanges captured by spike S3 (`compat/fixtures/hub/`), and
`hf_xet`'s `CasJWTInfo` (`xet_client/src/hub_client/types.rs` at xet-core
`v1.7.0`). The batch header names `X-Xet-Cas-Url`, `X-Xet-Access-Token`,
`X-Xet-Token-Expiration` and `X-Xet-Session-Id` were read from
`git_xet/src/constants.rs` at xet-core `v1.7.0`. `X-Xet-Hash` is not in
that file; it is the `resolve` response header, observed in the S1 and S3
captures.

## Decision

1. **Route.** Xet-native clients get CAS credentials from the LFS-scoped
   token route:
   - `GET {lfs-url}/xet-read-token`
   - `GET {lfs-url}/xet-write-token`

   `{lfs-url}` is exactly what `lfs.url` points at, for example
   `https://host/{ns}/{repo}.git/info/lfs`.
2. **Request.**
   - Header `Accept: application/json`.
   - Auth is HTTP Basic: the username is ignored and the password is an
     openxet access token, which is git-credential-helper compatible.
     `Authorization: Bearer <access token>` is also accepted.
   - A batch capability `href` (see 10) needs no auth header.
3. **200 response body:**
   `{"accessToken": "<opaque>", "exp": <unix SECONDS>, "casUrl": "<absolute https URL>"}`.
   The same values are also returned as headers `X-Xet-Cas-Url`,
   `X-Xet-Access-Token` and `X-Xet-Token-Expiration` (seconds).
4. **Refresh.** Repeat the same GET; the response is the same. Clients
   refresh about 30 s before `exp`, as xet-core does.
5. **Errors.** 401 for a bad or missing credential, 403 for no read/write on
   this scope, 404 for an unknown repo. The body is
   `{"schema":"openxet.error.v1","code","message","hint"}`.
6. **Token scope.** CAS tokens are scoped to (scope, read|write), never to
   objects. Clients may chunk and upload before they know oids.
7. **Registration.** A successful shard upload registers each file's xet hash
   to the scope. The sha256 in the shard's file-metadata extension registers
   sha256 → xet-hash for that scope, with no extra call (no LFS verify).
   Files without the sha256 extension register but can't be served to plain
   git-lfs by oid.
8. **Downloads.** A read token allows `/v2/reconstructions/{xet-hash}` (and
   v1) for any file registered to the scope, until GC collects it. That
   includes files only force-pushed-away commits referenced (ruling R25,
   design §5.4). For Xet-native clients (token
   route) there is no per-object handshake. git-lfs/git-xet clients get the
   hash from the batch response (item 10). Spike S7 found that `git-xet`
   0.2.1 does not download at all, and huggingface.co sends no `X-Xet-Hash`
   (ADR 0003).
9. **Pointers.** Pointers are the canonical 3-key git-lfs pointer
   (`version` / `oid` / `size`). openxet tolerates and preserves unknown keys,
   per the LFS spec, but never writes them. The LFS spec requires exactly one
   valid encoding; extra keys change Git blob ids and break mixed-tool repos.
   openxet's own client uses the same format.
10. **Batch details** (`POST {lfs-url}/objects/batch`, transfers
    `["xet","basic"]`; amended by ADR 0003: uploads answer `xet`,
    downloads always answer `basic`).
    - JSON keys in `actions.*.header` use exactly this casing:
      `X-Xet-Hash`, `X-Xet-Cas-Url`, `X-Xet-Access-Token`,
      `X-Xet-Token-Expiration`, `X-Xet-Session-Id`.
    - The `basic` download action's `header` carries only `X-Xet-Hash`,
      which is not a secret. It has no CAS access token, CAS URL or
      expiration. Download agents get those from `GET {lfs-url}/xet-read-token`
      (item 1). This keeps the token out of `GIT_TRACE` output and out of
      reach of an off-host redirect of the download `href`.
    - The upload action's `href` is absolute: a signed capability URL for
      one scope and one operation. A GET on it returns the token-route
      response with no Authorization header, until its own expiry, which is
      at least as long as the CAS token it returns. Authorization headers sent
      to it are ignored and never logged.
    - The download action's `href` is the HTTP read gateway URL for the
      object (absolute), not a token route.
    - No action `header` or response ever carries the client's own credential (§5.9 req 6).
11. **Dev mode.** `openxet serve` with no config runs SQLite plus local disk,
    prints a static access token and the env/URL lines, and supports `--json`
    startup info. Port and flags are not final.

## Consequences

- Chunk-first clients work without knowing oids. Implication: they need no
  per-object round trips.
- Token scope is coarse (scope + mode). Implication: per-object restriction
  is not available, so the authorizer decision happens at token issue.
- Shard upload registers files (item 7). Implication: it changes index
  state, so it must be verified before registering (S6) and idempotent
  on retry.
- Header-key casing and the pointer encoding are compatibility commitments.
  Implication: changing either needs a superseding ADR.
- Tests, built in M1/M2, enforce this:
  - A compatibility-suite "third-party Xet client" leg, whose expectations
    come from jj-xet's `tests/openxet.rs` mock.
  - Exact header-key casing assertions on batch responses.
  - The pointer canonical-form test.
  - Capability `href` refresh without an auth header.

## Alternatives considered

- **Batch endpoint only.** Rejected: it requires oids before upload, which
  doesn't work for chunk-first clients.
- **Hub token route only.** Rejected: it doesn't exist for repos hosted on
  other Git hosts, which design §6.1 and §10.2 target.
