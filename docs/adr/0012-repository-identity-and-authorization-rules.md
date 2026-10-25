# 0012: Give repositories immutable ids, and authorize users with config rules

<!-- description: A repository has an immutable UUIDv7 id that names its CAS scope and bucket keys, its tenant is its namespace, user credentials are separate from CAS credentials, and M2 authorizes them with [[auth.rules]] that only grant. -->

- Status: accepted
- Date: 2026-10-25
- Scope: `hub`, `git`, `auth`, `index`, `storage`, `server`, `docs`

## Context

M1 serves the CAS alone. Its credentials are CAS credentials: a static
token or an EdDSA JWT naming a scope, a mode and optionally a tenant
(ADR 0007, rulings M1-R38 and M1-R43). M2 adds repositories, the Hub API
and Git hosting (design §6, §10.9), and with them two questions the M1
model can't answer.

**What is a repository's scope?** Design §5.4 makes the scope the unit of
access and persists it: its SHA-256 keys every registration record in
the object-store bucket (ruling M1-R37). ADR 0010 sets a repository's
scope to `{namespace}/{name}` and asks the M2 plan for an immutable scope
id, so that a rename never orphans data. If the scope were the name
itself, renaming `foo/bar` to `foo/baz` would leave every registration
record and every bucket key under the old name, and a later repository
named `foo/bar` would inherit them.

**Who may read, write or create a repository?** Design §5.4 names an
authenticator, which turns a credential into a principal (subject plus
claims), and an authorizer, which decides per repository or namespace.
OIDC, the webhook authorizer and openxet-issued access tokens are M3
work (design §12). M2 still needs stock clients to work end to end:
`hf auth login` sends one token, and that token must open every
repository its holder may use, not one scope.

## Decision

**Repository identity (ruling M2-R2).**
1. A repository has an immutable id: a UUIDv7, rendered as 32 lowercase
   hexadecimal characters. It is assigned at creation and never reused.
2. Its CAS scope is `repo:{id}`, and its tenant is its namespace
   (design §5.4). CAS tokens the Hub and LFS token routes mint carry
   that scope and tenant. The scope prefix `repo:` is reserved for
   repositories: a configured static token whose `scope` starts with
   `repo:` is a configuration problem, and `serve` and `config check`
   refuse it. A CAS token a platform signs with such a scope is
   accepted, because the platform holds the signing key and is trusted
   with any scope.
3. Bucket keys for its data carry the id (`repos/{id}/…`). Namespace and
   name live in the repository's metadata record, so a rename rewrites
   one record and orphans nothing.
4. Namespace and name each match `^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$` and
   contain no `..`, and the name doesn't end in `.git`.
   `{namespace}/{name}` is unique case-insensitively and stored as
   written. Names are unique across labels (ADR 0010 item 5).

**User credentials and the authorizer (ruling M2-R3).**
5. A credential is a **CAS credential** (M1: scope, tenant, mode), a
   **user credential** (a subject plus claims, which are string lists
   under free names such as `orgs` and `groups`), or both.
   - A static token without `scope` is a user credential.
   - A static token with `scope` is a CAS credential, as in M1.
   - A static token with a `scope` and user fields (`name`, `email` or
     `claims`) is both; the dev token is one.
   CAS routes take the CAS credential; Hub, Git and LFS routes take the
   user credential and ask the authorizer.
6. The `Authorizer` decides `read`, `write` and `create` for a user
   credential: `read` and `write` per repository, `create` per
   namespace.
7. M2 ships the config-rules authorizer. Each `[[auth.rules]]` entry has:
   - `subjects`, a list of subjects, and/or `claims`, a table of claim
     names to lists of values. A rule matches a principal whose subject
     is listed, or who carries one of the listed values under the named
     claim.
   - `repos`, a list of globs over `{namespace}/{name}`. `*` matches
     within one path segment and never crosses the `/`, except that `*`
     alone matches every repository: `org/*` matches `org/x`, and `o*/x`
     matches `org/x`.
   - `actions`, a subset of `read`, `write` and `create`. `create` on
     `org/*` allows creating repositories in namespace `org`.
8. **Rules only grant.** A request is allowed if any rule allows it, and
   refused if none does. Rule order doesn't matter: there is no
   first-match-wins and no deny rule.
9. The pseudo subject `anonymous` matches a request with no credential,
   so an operator can make repositories public with an explicit rule. No
   rule names it by default.
10. Dev mode installs one rule: `subjects = ["dev"], repos = ["*"],
    actions = ["read", "write", "create"]`.
11. `GET /api/whoami-v2` answers `name` from the subject, or from the
    static token's `name`, and `orgs` from the `orgs` claim.

## Consequences

- A rename touches one metadata record. Registrations, packs, ref logs
  and labels stay keyed by the id, so `reindex` rebuilds a renamed
  repository from the object-store bucket alone (non-negotiable 2).
- Scope strings stay opaque to the CAS: `repo:{id}` is one more scope
  string, so M1's file scoping, dedup and per-tenant isolation apply
  unchanged.
- Because `repo:` is reserved, an operator's storage-only static token
  can't land in a repository's scope by a naming accident. A platform
  that signs CAS tokens can still name a repository's scope on purpose,
  as it can any scope; signing keys are trusted with every scope
  (ADR 0007). The config check is enforced with the static-token
  changes of M2 (the plan's Task 3).
- Per-tenant dedup groups an organisation's repositories: a fine-tune
  dedups against its base model in another repository of the same
  namespace (design §5.4).
- Static tokens become M2's user credential. M3's OIDC subjects and
  claims, openxet-issued access tokens and the webhook authorizer plug
  into the same seam: an authenticator produces a `UserPrincipal`, an
  `Authorizer` answers `allows`.
- Public repositories are an explicit `anonymous` grant, never a
  default, so a server with auth on still has no unauthenticated reads
  (design §5.9 requirement 1) until its operator writes one.
- With grant-only rules, a reviewer audits a server's access by reading
  the rules that name a repository; no rule elsewhere in the list can
  take access away.
- Tests: `crates/auth/tests/rules.rs` (`no_rule_denies`,
  `anonymous_only_with_explicit_rule`, `glob_never_crosses_the_slash`
  and the matcher cases), `names_validated` and
  `name_taken_case_insensitively_409` in `crates/hub/tests/repos.rs`,
  and `token_scope_is_repo_id_and_tenant_namespace` in
  `crates/hub/tests/tokens.rs`.

## Alternatives considered

- **Scope = `{namespace}/{name}`**, as ADR 0010 first wrote it. Rejected:
  a rename would orphan every registration and bucket key, and a new
  repository under the old name would inherit them.
- **Deny rules, or ordered rules with first-match-wins.** Rejected until
  an operator needs them: with denies on top of grants, whether a
  request is allowed depends on every rule and their order, which is
  harder to audit. A deny can be added later without changing what
  existing rules mean.
- **Only CAS credentials, one static token per repository.** Rejected:
  `hf auth login` stores one token, and a user with ten repositories
  would need ten logins.
