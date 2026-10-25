# 0007: CAS tokens are EdDSA JWTs

<!-- description: CAS tokens are Ed25519-signed JWTs with a fixed set of claims: subject, scope, mode and expiry. -->

- Status: accepted
- Date: 2026-10-23
- Scope: `auth`, `cas`

## Context

Design §5.4 says CAS tokens are short-lived, signed by OpenXet or by a
platform with a configured key, and verified statelessly on every node.
Design §13 left the format and algorithm to the builder.

Stock clients treat the token as opaque and refresh it only by clock,
30 s before `exp`; they never react to a `401` (S10, "Auth"). The server
therefore has to honour `exp` exactly.

## Decision

1. **Format.** A JWT signed with EdDSA (Ed25519).
2. **Claims.** `{sub, scope, mode, exp, iat}`:
   - `sub`: the subject the token acts for.
   - `scope`: the scope string (design §5.4).
   - `mode`: `"read"` or `"write"`; `write` supersedes `read`.
   - `exp`, `iat`: Unix seconds.

   The header carries `kid`, naming the verification key.

   *Amended (M1-R19):* the only other claim accepted is an optional `nbf`,
   and the token is rejected while `now < nbf`. Any other claim, such as
   `aud`, `iss` or `jti`, is rejected rather than ignored, so a restriction
   OpenXet cannot honour never fails open. `sub` and `scope` must be
   non-empty and free of control characters, and `scope` may be at most
   1024 bytes.

   *Amended (M1-R38):* an optional `tenant` claim names the tenant the
   scope belongs to, the unit of isolation for xorbs and dedup under
   `per-tenant` visibility (design §5.4). It follows the rules for
   `scope`: a non-empty string, free of control and format characters,
   at most 1024 bytes; `null` or any other type is rejected. Without it
   the tenant is `scope:` followed by the scope, so tokens minted before
   the claim existed keep working and keep each scope a tenant of its
   own. *(M1-R43)* An explicit `tenant` may not start with `scope:`, so
   a scope whose name equals a tenant's, a project called `acme` next to
   a tenant `acme`, never shares that tenant's xorbs. The claim is added
   now, before platforms integrate, because the strict claim set above
   would make a later addition break every platform's tokens. Static
   tokens take an optional configured `tenant` with the same default.
3. **Key set.** The server accepts several public keys at once and signs
   with one private key. Rotation adds the new public key everywhere,
   switches the signer, then removes the old key after the longest token
   lifetime. A token with an unknown `kid` is rejected.
4. **Expiry.** `exp` is honoured exactly: valid until `exp`, `401` after,
   with no leeway.
5. **Static tokens** (dev, CI, small setups) are not JWTs. They are
   compared by SHA-256 in constant time, so timing does not reveal
   how much of a guess matched.
6. **Platforms** sign tokens themselves with any standard JWT library
   against a public key set OpenXet is configured with. OpenXet never
   needs their private key.

## Consequences

- Any language can mint tokens, so storage-only platforms need nothing
  from us beyond a key set.
- Verification is stateless, so any node serves any request
  (non-negotiable 5).
- Revoking a CAS token before `exp` is not possible. Lifetimes stay short,
  and revocation happens at the access token or authorizer (design §5.4).
- Tokens carry no revision: Xet reads follow scope membership (R25), so a
  revision claim would enforce nothing. The Hub token route still accepts
  a revision in its URL (M2) and ignores it when minting.
- Tests, added with the implementation: expired, wrong-`kid`,
  wrong-algorithm and wrong-mode tokens are rejected; a rotation test
  accepts old and new keys together.

## Alternatives considered

- **PASETO v4.public.** Rejected: platforms need signers in every
  language, and JWT libraries are ubiquitous.
- **HMAC-signed tokens (HS256).** Rejected: verifiers would hold the
  signing secret, so a platform could not mint tokens without sharing it
  with every node.
