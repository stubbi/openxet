# M2: Hub API, Git hosting and LFS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

<!-- description: The M2 plan: the Hub-compatible API, Git hosting on the git binary, the Git LFS endpoint for git-xet, generic labels, the resolve gateway, the xet:// address contract, purge, OpenAPI and one router. -->

**Goal:** Stock `huggingface_hub` 2.2.0 (`hf auth login`, `hf upload`,
`hf download`, listing, branches and tags), `git clone` / `git push` over
HTTPS, and `git push` / `git clone` with `git-xet` 0.2.1 through the LFS
endpoint all work unmodified against `openxet serve`, on one shared Git
history per repository, with every M2 row of the security tests passing.

**Architecture:** Three new crates sit next to the M1 CAS behind one router
(issue #46): `openxet-hub` (the Hub API, LFS batch, token routes, labels
and the file gateway), `openxet-git` (the engine of ADR 0004: packs in the
object-store bucket, refs in the index, per-request snapshot repos served
by the `git` binary, gix in-process off the wire path) and `openxet-uri`
(the `xet://` contract of ADR 0009). `openxet-auth` gains user credentials
and the authorizer seam; `openxet-index` and `openxet-storage` gain the
repository, ref, pack and label tables and records, bucket first. Every
Hub write is a real Git commit through the same fenced commit transaction
as a push.

**Tech Stack:** Rust 1.97 (edition 2024); axum 0.8, tokio, hyper; sqlx 0.9
(SQLite now, Postgres in M3); `gix` 0.90 (`gix-pack` 0.77) for in-process
object reads, the reachability walk and Hub commit packs; the `git` binary
≥ 2.37 for the wire protocol; `utoipa` 6 for OpenAPI; proptest, cargo-fuzz,
criterion; pytest + uv with `huggingface_hub` 2.2.0, `hf_xet` 1.7.0,
`git-lfs` 3.7.1, `git-xet` 0.2.1 and Gitea 1.24 (compat).

**Spec:** `docs/superpowers/specs/2026-10-23-openxet-design.md` (§3, §5.3,
§5.4, §5.5, §5.9, §6, §6.1, §7.1, §10.4, §10.9, §12 M2, §17), with
ADR 0003 (LFS and capability `href`s), ADR 0004 (the Git engine; its test
checklist is an M2 deliverable), ADR 0005 (Xet-native clients), ADR 0009
(`xet://`) and ADR 0010 (two primitives, labels, typed views). The wire
shapes are S3 (`docs/research/S3-hub-api-shapes.md`, binding for every Hub
route, with the fixtures in `compat/fixtures/hub/`), S7 (LFS, fixtures in
`compat/fixtures/lfs/`) and S9 (Git). The URI deliverable follows
`docs/superpowers/plans/2026-10-24-resource-uri-milestones.md`.

## Global Constraints

**Protocol and compatibility**
- Hub responses match S3 field for field: every field S3 marks as read by
  the client is present with the recorded type; nothing S3 records as
  observed is dropped without a ruling. `X-Error-Code` values are HF's
  (`RepoNotFound`, `RevisionNotFound`, `EntryNotFound`) where HF defines
  one.
- Hub-layer JSON errors are `{"error": <message>, "schema":
  "openxet.error.v1", "code", "message", "hint"}` with headers
  `X-Error-Code` and `X-Error-Message` (M2-R6). CAS routes keep the M1
  body.
- LFS batch `actions.*.header` keys use exactly `X-Xet-Hash`,
  `X-Xet-Cas-Url`, `X-Xet-Access-Token`, `X-Xet-Token-Expiration`,
  `X-Xet-Session-Id` (ADR 0005 item 10). Downloads always answer `basic`
  (ADR 0003 decision 3). Pointers are the canonical 3-key form.
- Token routes return the token in the JSON body **and** the three
  `X-Xet-*` headers, with `exp` in Unix seconds (S3 answer 2).
- `git` ≥ 2.37 on `PATH` is a runtime dependency of the `git` role; start-up
  checks it (ADR 0004 item 8). `gix` is never on the wire path.
- `xet-core` crates stay at `=1.7.0`; `xet_core_structures` only in
  `crates/protocol`, `xet_data` and friends only in `crates/client`
  (ADRs 0001 and 0006, `scripts/check-client-core-ban.sh`).
- New dependencies take their latest stable release (M1-R15): `gix`
  0.90.0, `gix-pack` 0.77.0, `utoipa` 6.0.0, `utoipa-axum` 0.3.0 at the
  time of writing; check `cargo search` when adding.

**Security (design §5.9, every row a release blocker)**
- No unauthenticated reads: every Hub, Git, LFS and gateway route needs a
  credential, except where an `anonymous` rule grants read (M2-R3) and
  the probes. Capability `href`s are the exception by design (ADR 0003
  decision 2): the signature is the credential.
- Credentials are authorized per repository on every route; a repository
  the caller may not read answers exactly the unknown-repository response
  (M2-R6). Creation is authorized per namespace before the name is looked
  up (ADR 0010 item 5).
- Nothing from an `Authorization` header, a Basic password, a capability
  `sig` or a CAS token is logged or echoed. Logs record route templates,
  never raw paths or queries (M1-R20). Capability routes never
  authenticate by header (ADR 0003 decision 2).
- Git reads check reachability on every path (R22) and every ref writer,
  Hub API included, commits through the fenced transaction (ADR 0004
  item 3).
- R21/R26: no public text describes or hints at an unfixed third-party
  security weakness.

**Durability**
- Bucket first, index second: packs, commit objects, repository metadata,
  labels, the bucket ref log and purge records reach the object store
  before the index records them and before the client is answered
  (design §5.3, ADR 0004 item 3, ADR 0010 consequences).
- Nothing is deleted outside GC (non-negotiable 3). M2 implements pack
  state transitions and the pack sweep as library code with tests; the
  `openxet gc` and `openxet reindex` commands that run them ship in M4.

**Process**
- One branch and PR per task (`m2/tN-<slug>`), squash-merged; independent
  tasks run in parallel in their own worktrees (M1-R31) following the
  dependency graph below. A task edits only the files it owns; shared
  files (`Cargo.toml`, `Cargo.lock`, `docs/SUMMARY.md`,
  `.github/workflows/ci.yml`, `AGENTS.md`, `CHANGELOG.md`) are edited by
  the task that needs them and rebased by whoever merges second.
- Commits: `git commit -s`, Conventional subject with a component scope,
  ending with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- TDD: RED before GREEN, never weaken a test. Every new CLI command has
  `--json` with a versioned schema, documented exit codes, `--dry-run`
  where it changes state, and an entry in the agent leg's
  `JSON_COMMANDS`.
- Every route is built through the route builder (clippy
  `disallowed-methods`), so it lands in the credential, scoping and echo
  matrices automatically.
- Review gates: each task gets a fresh reviewer against this plan and the
  spec before merge; every security test named here is listed in
  `docs/reference/security-tests.md` with its file, and
  `scripts/check-security-tests-map.py` must pass.

## Review Focus

These are the conditions a user is most likely to hit that the spec
implies but no feature test exercises naturally. Each is pinned by a test
in the task that owns it.

1. **A large file already uploaded to another repository.** `hf upload`
   to repo B of a file the same user uploaded to repo A sends only a
   shard (dedup), and the commit must succeed because the shard registers
   the file to B; a commit naming a sha256 registered only to A must fail
   with a hint, never silently reference A's file. Pinned by
   `commit_rejects_lfs_file_registered_to_another_scope` (Task 12) and
   the compat test `test_upload_dedups_across_repos` (Task 19).
2. **Two clients committing to one branch at once.** Both `hf upload`
   calls succeed, one after the other, with no lost commit; the loser
   retries inside OpenXet from a fresh snapshot (ADR 0004 item 3). Pinned
   by `concurrent_hub_commits_both_land` (Task 12).
3. **A `git push` with a large blob that is not an LFS pointer.** It is
   accepted and kept as a Git object, the push output names the file and
   size and suggests `git lfs track` (the `openxet migrate-lfs` command
   comes with M6's client); above `git.max_plain_blob_bytes`, when set, it
   is refused with the same message. Pinned by `plain_large_blob_reported`
   and `plain_large_blob_refused_above_limit` (Task 8).
4. **Path edge cases on `resolve` and `tree`.** Paths with spaces, UTF-8,
   `%2F`, a leading slash, `.` and `..` segments, a `tree` on a file path,
   and a `resolve` on a directory each answer the S3 shape (`EntryNotFound`
   with `X-Repo-Commit`, or the entry) and never a `500` or a path outside
   the commit's tree. Pinned by `resolve_path_edges` (Task 13) and
   `tree_path_edges` (Task 12).
5. **Token refresh mid-upload after the branch moved.** `hf_xet` refreshes
   its write token through `…/xet-write-token/{rev}` where `{rev}` is the
   branch name; another commit has moved the branch meanwhile, and the
   refresh must still succeed (tokens carry no revision, M1-R11). A
   capability `href` must still answer after the CAS token it first
   returned has expired. Pinned by `write_token_refresh_ignores_revision`
   (Task 12) and `capability_href_outlives_first_token` (Task 14).

## Decisions this plan makes (rulings, recorded in design §17 by Task 1)

- **M2-R1. Crate layout.** `openxet-http` (the route builder, request ids,
  the error body and the middleware stack, extracted from `openxet-cas`),
  `openxet-hub`, `openxet-git` and `openxet-uri` are new crates. The
  authorizer lives in `openxet-auth`. The `openxet serve` command composes the CAS,
  Hub and Git routes into one `axum::Router` with one fallback, one state
  per crate and one middleware stack (issue #46). `clippy.toml` also bans
  `Router::fallback` and `Router::fallback_service` outside the builder.
- **M2-R2. Repository identity.** A repository has an immutable id: a
  UUIDv7, rendered as 32 lowercase hex characters. Its CAS scope is
  `repo:{id}` and its tenant is its namespace (design §5.4). Bucket keys
  for its data carry the id (`repos/{id}/…`); namespace and name live in
  the repository's metadata record, so a rename rewrites one record and
  orphans nothing. Namespace and name each match
  `^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$`, contain no `..`, and the name
  doesn't end in `.git`; `{namespace}/{name}` is unique case-insensitively
  and stored as written. Names are unique across labels (ADR 0010 item 5).
- **M2-R3. User credentials and the authorizer.** A credential is either
  a **CAS credential** (M1: scope, tenant, mode) or a **user credential**
  (subject plus claims: string lists under free names, such as `orgs` and
  `groups`). A static token without `scope` is a user credential; one with
  `scope` is a CAS credential as before; one with both is both, which is
  what the dev token is. The `Authorizer` decides `read`, `write` and
  `create` per repository or namespace for a user credential. M2 ships
  the config-rules authorizer: `[[auth.rules]]` entries with `subjects`
  and/or `claims` matchers, `repos` globs (`org/*`, `*`) and `actions`;
  `create` on `org/*` allows creating in namespace `org`. The pseudo
  subject `anonymous` matches requests with no credential, so an operator
  can make repositories public; no rule names it by default. Dev mode
  installs the rule `subjects = ["dev"], repos = ["*"], actions = ["read",
  "write", "create"]`. `whoami-v2` answers `name` from the subject (or the
  static token's `name`), `orgs` from the `orgs` claim. OIDC, the webhook
  authorizer and OpenXet-issued access tokens come in M3; nothing here
  prevents them.
- **M2-R4. Routing a file to Xet.** `preupload` answers `lfs` when the
  file's size is at least `hub.lfs_threshold` (default `1MiB`, bounds
  `1KiB` to `10MiB`) or its path matches an LFS pattern of the target
  revision's `.gitattributes` (for a new repository, the generated one),
  and `regular` otherwise. A regular file's decoded content is at most
  `hub.lfs_threshold`; a commit's NDJSON body is at most
  `hub.max_commit_bytes` (default `64MiB`) and at most 25,000 operations.
  The M2 benchmark may revise the default, with the number.
- **M2-R5. The initial commit.** `POST /api/repos/create` writes a
  server-generated `initial commit` holding huggingface.co's default
  `.gitattributes` (the S3 capture's content), authored and committed by
  `openxet <noreply@openxet.com>`, on branch `main`, so `commits`, `refs`
  and `git clone` of a new repository behave as on huggingface.co.
- **M2-R6. Hub error shape and the unknown-repository answer.** Hub
  errors carry both HF's `error` field and OpenXet's structured fields
  (Global Constraints). The unknown-repository answer is one constant
  body and header set: `404`, `X-Error-Code: RepoNotFound`,
  `X-Error-Message: Repository not found`, body
  `{"error":"Repository not found","schema":"openxet.error.v1","code":"repo_not_found","message":"Repository not found","hint":"Check the repository id and the token's access: a repository the token may not read answers exactly like one that doesn't exist."}`,
  and it is the answer for a missing repository, a repository the caller
  may not read, and a typed view whose label the repository lacks when
  the caller may not read it (ADR 0010 item 4). A missing credential on
  a repository route is `401` with `WWW-Authenticate: Bearer
  realm="openxet"` and `X-Error-Message: Authentication required: pass an
  openxet access token as the Bearer token (hf auth login)`; an invalid
  one is `401` with `X-Error-Message: Invalid credentials in Authorization
  header`, which `huggingface_hub` reports as an auth error rather than a
  missing repository (S3 answer 6).
- **M2-R7. Hub commits.** A Hub commit is a real commit built with gix
  from the parent's tree: `file` operations become blobs, `lfsFile`
  operations become canonical LFS pointers whose sha256 must be
  registered to the repository's scope (M1's `file_scopes.sha256`),
  `deletedFile` and `deletedFolder` remove entries. The author is the
  subject (`{subject} <{subject}@users.noreply.openxet.com>` unless the
  credential carries an `email` claim), the committer is
  `openxet <noreply@openxet.com>`, the message is `summary`, a blank line
  and `description`. `parentCommit`
  mismatch answers `412 precondition_failed`. The response's `commitUrl`
  is `{public_url}/{prefix}{ns}/{name}/commit/{oid}`.
- **M2-R8. Listings paginate deterministically.** `tree`, `commits` and
  the repository listings answer at most 1,000 entries per page, ordered
  by path (tree) or by commit order (commits), with
  `Link: <…?cursor=…>; rel="next"` when more follow; the cursor is the
  last entry's key, base64url-encoded. `X-Total-Count` is set where S3
  observed it.
- **M2-R9. The file gateway.** Xet files resolve with `302` to
  `{public_url}/v1/transfer/files/{repo id}/{xet hash}?name={filename}`,
  which the `GET` serves with the caller's credential (Bearer, or Basic
  with the token as the password), with HTTP `Range` (all three forms,
  `416` past EOF), `ETag: "{sha256}"`, `If-Range`, `HEAD`,
  `Content-Disposition` and `Accept-Ranges`. LFS `basic` download `href`s
  are the same URL with no `authenticated` field, so `git-lfs` attaches
  the credential it holds for that host (its documented behaviour). Small
  files are served inline from the Git blob with `ETag: "{blob sha1}"`.
- **M2-R10. LFS uploads without `git-xet`.** Until server-side chunking
  (M4), an upload batch that doesn't offer `xet` answers each object with
  an LFS object error `{"code": 501, "message": "…install git-xet…"}`
  naming the install command, never a `basic` `href` that fails later.
- **M2-R11. Smart HTTP details.** `info/refs?service=`, `git-upload-pack`
  and `git-receive-pack` are served at `/{prefix}{ns}/{name}.git/…` and
  without `.git`; a request body with `Content-Encoding: gzip` is
  inflated (git sends them); `Git-Protocol: version=2` selects v2; Basic
  auth takes the token as the password (ADR 0005 item 2) and Bearer is
  accepted too. The first push to a missing repository creates it if the
  authorizer allows `create` in the namespace, with the `model` label as
  the Hub's default view does.
- **M2-R12. Labels API.** OpenXet's own API lives under
  `/api/openxet/v1/`: `GET /api/openxet/v1/repos/{ns}/{name}/labels`,
  `PUT …/labels/{label}`, `DELETE …/labels/{label}`. The CLI is
  `openxet label add|remove|list`. Each change writes the repository's
  metadata record to the bucket, then the index, then an `audit_log` row
  (`actor`, `action`, `repo`, `detail`, `at`), the table M4's audit API
  exposes.
- **M2-R13. Purge.** `openxet purge <file-hash>` (needs the server
  config) writes `purges/{hash}` to the bucket, then deletes the file's
  rows from every scope, never dedup rows (those belong to the xorbs,
  whose bytes leave through GC), writes an `audit_log` row, and prints what it removed; `reindex` (M4) skips purged hashes.
  It takes `--dry-run`, `--yes` and `--json` (`openxet.purge.v1`).
- **M2-R14. The URI grammar, in brief** (the full specification is Task
  16's document): `xet://{host}[:{port}]/[{kind}/]{namespace}/{name}[@{revision}][/{path}]`,
  where `{kind}` is absent for a repository (whatever its labels),
  `datasets`, `spaces` or `kernels` for a typed view and `buckets` for a
  bucket (resolved from M5 on); `@{revision}` is a ref or OID (names
  can't contain `@` under M2-R2, so nothing needs escaping); the path is percent-encoded UTF-8
  under RFC 3986; there is no userinfo. Legacy forms without a host
  (`xet://user/repo/branch/path`, pyxet) and forms naming `xethub.com`
  are rejected with an error that says how to write the address; nothing
  is ever sent to a default server.
- **M2-R15. OpenAPI.** `openxet openapi` prints the OpenAPI 3.1 document
  generated from the route builders' `utoipa` annotations; it is
  checked in at `docs/reference/openapi.json`, served at
  `GET /openapi.json`, and CI fails when the two differ.
- **M2-R16. Git budgets** take ADR 0004's defaults
  (`git.reachability.max_commits` 200,000, `max_ms` 2,000,
  `max_concurrent_per_repo` 2, `max_concurrent_per_principal` 4;
  `git.thin_bases.max_objects` 2,000,000, `max_ms` 2,000) and Task 20
  records the measured values that replace the thin-base placeholders.
  The node cache is `git.cache_dir` (default `{data_dir}/git-cache`) with
  snapshots under it, `git.cache_max_bytes` (default `20GiB`) and
  `git.max_plain_blob_bytes` (default unset).

## Dependency graph and file ownership

```
wave 1: T1 docs/ADR · T2 openxet-http + one router · T3 auth · T4 index · T5 storage · T16 uri
wave 2: T6 git core (T4, T5) · T11 hub skeleton (T2, T3, T4, T5)
wave 3: T7 upload-pack (T6) · T8 receive-pack (T6) · T17 purge + label CLI (T11)
wave 4: T9 repack/sweep/reindex (T8) · T10 hub objects (T8) · T12 hub API (T11, T10)
wave 5: T13 resolve + gateway (T12) · T14 LFS (T12) · T15 git HTTP (T7, T8, T11)
wave 6: T18 OpenAPI (T12, T14, T15) · T19 compat (T13, T14, T15) · T20 benchmarks (T13, T15)
wave 7: T21 docs and project state (all)
```

Owned files:

```
crates/http/                      T2: Routes, Endpoint, RequestId, ErrorBody, middleware, Clock
crates/cas/src/{lib,routes,error,request_id}.rs  T2 (uses crates/http)
crates/auth/src/{lib,static_tokens,rules,capability}.rs  T3
crates/index/src/{repos,git,sqlite}.rs, migrations/0006_*.sql  T4
crates/storage/src/{lib,records,local}.rs  T5
crates/git/src/{lib,store,snapshot,process,upload,interpose,reach}.rs  T6, T7
crates/git/src/{receive,hook,thin,publish}.rs  T8
crates/git/src/{repack,lease,sweep,reindex}.rs  T9
crates/git/src/objects.rs  T10
crates/hub/src/{lib,error,repos,whoami,labels,views,authz}.rs  T11
crates/hub/src/{tokens,preupload,commit,refs,tree,info}.rs  T12
crates/hub/src/{resolve,gateway}.rs  T13
crates/hub/src/lfs/  T14
crates/hub/src/githttp.rs  T15
crates/uri/  T16
crates/openxet/src/{purge_cmd,label_cmd}.rs  T17
crates/openxet/src/openapi_cmd.rs, docs/reference/openapi.json  T18
compat/tests/test_hub.py, test_git.py, test_lfs_gitea.py, test_jj_xet.py  T19
crates/git/benches/, crates/hub/benches/  T20
docs/  T21 (plus each task's reference pages)
```

---

### Task 1: Record M2's decisions (ADR 0012, design edits, security map rows)

**Files:**
- Create: `docs/adr/0012-repository-identity-and-authorization-rules.md`
- Modify: `docs/superpowers/specs/2026-10-23-openxet-design.md` (§5.4 scope ids and user credentials, §6 threshold and error shape, §12 M2 links this plan, §17 rulings M2-R1…R16), `docs/reference/security-tests.md` (M2 rows name their files per this plan), `docs/SUMMARY.md` (this plan and the ADR), `docs/adr/0010-two-storage-primitives.md` (consequences: point at M2-R2 for scope ids)

**Interfaces:**
- Produces: M2-R1…R16 as public text; ADR 0012 as the citation for M2-R2 and M2-R3.

- [ ] **Step 1: Write ADR 0012** from M2-R2 and M2-R3: context (ADR 0010
  asked the M2 plan for immutable scope ids; §5.4 needs an authorizer and
  user credentials before OIDC), decision (ids, scope `repo:{id}`, tenant
  = namespace, name rules; user vs CAS credentials; `[[auth.rules]]` with
  the matcher semantics, first-match-wins is **not** used: a request is
  allowed if any rule allows it, since rules only grant), consequences
  (static tokens become the M2 user credential; M3's OIDC subjects and
  the webhook authorizer plug into the same seam; `anonymous` is an
  explicit grant), alternatives (scope = `{ns}/{name}`: rejected, a rename
  would orphan registrations; deny rules: rejected until an operator
  needs them, since a deny-on-top model is harder to audit).
- [ ] **Step 2: Edit the design.** §5.4: the scope id, the tenant, user
  credentials and rules in one paragraph each, citing ADR 0012. §6: the
  error shape (M2-R6), the `lfs_threshold` rule (M2-R4), the initial
  commit (M2-R5), pagination (M2-R8), the gateway URL (M2-R9), the
  no-`git-xet` upload answer (M2-R10). §12 M2: link this plan. §17: one
  row per ruling.
- [ ] **Step 3: Security map.** Replace each M2 `pending` row with the file
  this plan assigns (Tasks 7–15), keeping the rows the checker expects; the
  checker tolerates files that don't exist yet only for `pending`, so
  mark them `pending (M2, <file>)` and let each task drop the `pending`.
  Check `python3 -I scripts/check-security-tests-map.py --help` for the
  exact syntax it accepts, and extend the script if it needs a planned
  state.
- [ ] **Step 4: Verify.** `scripts/build-site.sh` passes (navigation and
  descriptions); `git grep -n -i -E "weak|vulnerab|exploit" docs/adr/0012*`
  shows only OpenXet's own policy.
- [ ] **Step 5: Commit** as `docs(adr): 0012 repository identity and authorization rules; design M2 rulings`.

### Task 2: `openxet-http` and one router (issue #46)

**Files:**
- Create: `crates/http/{Cargo.toml,src/lib.rs,src/routes.rs,src/request_id.rs,src/error.rs,src/middleware.rs,src/clock.rs}`, `crates/http/tests/compose.rs`
- Modify: `crates/cas/src/{lib,routes,error,request_id}.rs` (use the new crate; keep `openxet_cas::{CasError, X_REQUEST_ID, RequestId, Clock, SystemClock}` as re-exports), `crates/openxet/src/serve.rs`, `clippy.toml`, `Cargo.toml` (workspace member)

**Interfaces:**
- Produces:
  - `openxet_http::Routes<S>`: the M1 builder, generic over the state:
    `new()`, `route(path, Handlers<S>)`, `merge(Routes<S>)`,
    `with_state(S) -> Mounted`, where `Mounted` holds a `Router<()>` and
    the endpoints; `on<H, T>(Method, H) -> Handlers<S>`, `.private()`.
  - `openxet_http::compose(parts: Vec<Mounted>, body_deadline: BodyDeadline) -> Router`:
    merges every part and applies, once, the fallback (`404 no_route`),
    the method-not-allowed fallback, the body deadline, catch-panic, the
    trace layer with the request-id span, sensitive headers and request-id
    assignment.
  - `openxet_http::ErrorBody { schema, code, message, hint }` with
    `IntoResponse` building the M1 JSON, and a `HubErrorBody` wrapper
    adding `error` plus the `X-Error-*` headers (used by Task 11).
  - `openxet_http::endpoints(parts) -> Vec<Endpoint>` under `test-support`.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests** in `crates/http/tests/compose.rs`:
  - `two_parts_share_one_fallback`: two `Routes` with different states,
    composed, answer `404` with the error body on an unknown path and
    `405` on a known path with the wrong method; the response has one
    `X-Request-Id`.
  - `middleware_applied_once`: a handler that reads the request-id
    extension sees the same id the response carries; a panicking handler
    answers `500 internal` without the panic text.
  - `endpoints_list_every_part`: `endpoints()` lists each part's routes
    with its methods.
- [ ] **Step 2: Run** `cargo nextest run -p openxet-http`. Expected: FAIL.
- [ ] **Step 3: Implement.** Move the M1 code; `openxet_cas::router(state)`
  becomes `openxet_cas::routes() -> Routes<AppState>` plus
  `openxet_cas::mounted(state) -> Mounted`, and `serve.rs` calls
  `compose(vec![cas])`. Add `axum::Router::fallback` and
  `fallback_service` to `clippy.toml`'s `disallowed-methods`, with the
  `#[expect]` inside `compose`.
- [ ] **Step 4: Run** `cargo nextest run --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings`. Expected: PASS;
  every M1 CAS test unchanged.
- [ ] **Step 5: Commit** as `refactor(server): openxet-http route builder and one composed router (#46)`.

### Task 3: User credentials, the rules authorizer and capability URLs

**Files:**
- Create: `crates/auth/src/rules.rs`, `crates/auth/src/capability.rs`, `crates/auth/tests/rules.rs`, `crates/auth/tests/capability.rs`
- Modify: `crates/auth/src/{lib,static_tokens,jwt}.rs`, `crates/openxet/src/config.rs` (`[[auth.rules]]`, static-token `scope` optional, `claims`, `name`, `email`), `crates/openxet/src/serve.rs` (dev rule), `docs/reference/config.md` (Static tokens, Rules)

**Interfaces:**
- Produces:

```rust
pub enum Credential { Cas(Principal), User(UserPrincipal), Both(Principal, UserPrincipal) }
pub struct UserPrincipal { pub subject: String, pub name: String, pub email: Option<String>, pub claims: BTreeMap<String, Vec<String>> }
pub const ANONYMOUS: &str = "anonymous";
impl UserPrincipal { pub fn anonymous() -> Self }
pub enum Action { Read, Write, Create }
pub struct Target<'a> { pub namespace: &'a str, pub name: Option<&'a str> }   // name None for Create
#[async_trait] pub trait Authorizer: Send + Sync + 'static {
    async fn allows(&self, who: &UserPrincipal, action: Action, target: Target<'_>) -> Result<bool, AuthzError>;
}
pub struct Rule { pub subjects: Vec<String>, pub claims: BTreeMap<String, Vec<String>>, pub repos: Vec<Glob>, pub actions: Vec<Action> }
pub struct RulesAuthorizer(Vec<Rule>);           // allows if any rule matches subject-or-claims AND repo glob AND action
impl Authenticator { pub fn authenticate(&self, bearer: &str, now: u64) -> Result<Credential, AuthError> }  // was Principal
pub struct CapabilitySigner { .. }               // HMAC-SHA256 with the transfer key
impl CapabilitySigner {
    pub fn sign(&self, scope: &str, op: Mode, exp: u64) -> CapabilityQuery;   // exp, sig as base64url
    pub fn verify(&self, scope: &str, op: Mode, query: &CapabilityQuery, now: u64) -> Result<(), AuthError>;
}
```

  - `TokenSigner::mint(sub, scope, tenant, mode, ttl)` (M1) is what the
    Hub token routes call; the server holds one signing key at
    `{data_dir}/cas-token.key`, generated like the transfer key (M1-R29).
- Consumes: M1 `Principal`, `StaticToken`, `UrlSigner`.

- [ ] **Step 1: Write the failing tests:**
  - `rules.rs`: `subject_rule_allows_exact_repo`, `glob_rule_allows_namespace_and_create`
    (`org/*` + `create` allows `Create` on `org`, not on `other`),
    `claims_rule_matches_any_listed_value`, `no_rule_denies`,
    `anonymous_only_with_explicit_rule`, `glob_never_crosses_the_slash`
    (`*` alone matches everything; `org/*` never matches `org/a/b`, which
    can't exist anyway, and `o*/x` matches `org/x`).
  - `capability.rs`: `sign_then_verify`, `wrong_scope_or_op_or_exp_fails`,
    `expired_fails`, `sig_is_constant_time_compared` (two wrong sigs of
    equal length take the same path; assert via a `subtle` usage test or
    code review note), `query_round_trips_through_url`.
  - static tokens: `token_without_scope_is_user_only`,
    `token_with_scope_and_claims_is_both`, `dev_token_is_both`.
  - config: `rules_are_checked_field_by_field` (bad action, bad glob,
    empty matcher → one problem each with key paths like
    `auth.rules[0].actions`), `claims_values_are_string_lists`,
    `static_token_scope_with_repo_prefix_is_a_problem` (key path
    `auth.static_tokens[n].scope`; ADR 0012).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Globs: `*` matches any run of characters
  except `/`; `**` is not supported (names have one slash). The
  `Credential` change touches every M1 CAS call site: CAS routes take
  `Credential::cas()` and answer `403 insufficient_scope` with the hint
  "this is a user token; get a CAS token from the Hub token route" for a
  `User`-only credential.
- [ ] **Step 4: Run** the auth, CAS and OpenXet suites. Expected: PASS;
  `credentials_never_logged` still passes.
- [ ] **Step 5: Commit** as `feat(auth): user credentials, the rules authorizer and capability URLs (ADR 0012)`.

### Task 4: Index tables for repositories, refs, packs and labels

**Files:**
- Create: `crates/index/src/repos.rs`, `crates/index/src/git.rs`, `crates/index/migrations/0006_repositories.sql`, `crates/index/tests/repos.rs`, `crates/index/tests/git_store.rs`
- Modify: `crates/index/src/{lib,sqlite,types}.rs`

**Interfaces:**
- Produces two traits implemented by `SqliteIndex` (and Postgres in M3):

```rust
pub struct RepoId([u8; 16]);  // UUIDv7; Display = 32 hex; FromStr
pub struct Repo { pub id: RepoId, pub namespace: String, pub name: String, pub private: bool, pub labels: Vec<String>, pub created_at: u64, pub full_repack_gen: u64 }
#[async_trait] pub trait Repos {
    async fn create_repo(&self, repo: &Repo) -> Result<(), IndexError>;             // NameTaken if the case-insensitive key exists
    async fn repo_by_name(&self, namespace: &str, name: &str) -> Result<Option<Repo>, IndexError>;
    async fn repo_by_id(&self, id: RepoId) -> Result<Option<Repo>, IndexError>;
    async fn list_repos(&self, filter: RepoFilter, cursor: Option<&str>, limit: usize) -> Result<Page<Repo>, IndexError>;  // filter: label, namespace
    async fn set_labels(&self, id: RepoId, labels: &[String]) -> Result<(), IndexError>;
    async fn delete_repo(&self, id: RepoId) -> Result<bool, IndexError>;           // rows only; data leaves through GC
    async fn file_by_sha256(&self, scope: &str, sha256: &[u8; 32]) -> Result<Option<FileRecord>, IndexError>;
    async fn purge_file(&self, file: &MerkleHash) -> Result<u64, IndexError>;      // rows removed from every scope
    async fn audit(&self, entry: &AuditEntry) -> Result<(), IndexError>;
}
pub enum PackState { Pending, Committed, Lost, Expired, Superseded, Recovered }
pub struct Pack { pub id: PackId, pub repo: RepoId, pub name: String, pub state: PackState, pub created_at: u64, pub bytes: u64, pub midx_gen: Option<u64> }
pub struct Snapshot { pub refs: Vec<(String, Oid)>, pub packs: Vec<Pack> /* committed only */, pub full_repack_gen: u64 }
pub struct RefUpdate { pub name: String, pub old: Option<Oid>, pub new: Option<Oid> }   // None old = create, None new = delete
pub struct CommitTx<'a> { pub repo: RepoId, pub updates: &'a [RefUpdate], pub pack: Option<PackId>, pub full_repack_gen: u64, pub actor: &'a str, pub request: &'a str }
pub enum CommitError { RefMismatch(String), PackNotPending, Fenced, Database(..) }
#[async_trait] pub trait GitStore {
    async fn snapshot(&self, repo: RepoId) -> Result<Snapshot, IndexError>;        // one read transaction
    async fn insert_pack(&self, pack: &Pack) -> Result<(), IndexError>;             // state Pending
    async fn commit(&self, tx: CommitTx<'_>) -> Result<(), CommitError>;            // ADR 0004 item 3, atomically
    async fn mark_pack(&self, id: PackId, from: PackState, to: PackState) -> Result<bool, IndexError>;   // guarded UPDATE … WHERE state = from
    async fn ref_log_since(&self, repo: RepoId, since_seq: u64) -> Result<Vec<RefLogRow>, IndexError>;
    async fn acquire_lease(&self, repo: RepoId, holder: &str, ttl: Duration) -> Result<Option<Lease>, IndexError>;  // epoch increases per acquisition
    async fn publish_repack(&self, p: RepackPublish<'_>) -> Result<(), CommitError>;  // lease check, outputs pending→committed, inputs committed→superseded, optional gen bump + ref_log high-water check
    async fn unplaced(&self, repo: RepoId) -> Result<Vec<Unplaced>, IndexError>;
    async fn record_unplaced(&self, rows: &[Unplaced]) -> Result<(), IndexError>;
    async fn clear_unplaced(&self, repo: RepoId, covered: &[Oid]) -> Result<u64, IndexError>;
}
```

  - Tables (migration 0006): `repos` (id BLOB PK, namespace, name,
    name_key = lower(namespace)||'/'||lower(name) UNIQUE, private,
    labels JSON, created_at, full_repack_gen), `refs` (repo, name, oid,
    PK (repo, name)), `ref_log` (seq PK, repo, ref, old, new, actor,
    request, at), `packs` (id, repo, name, state, created_at, bytes,
    midx_gen, superseded_at), `repo_leases` (repo PK, holder, epoch,
    expires_at), `reindex_unplaced` (repo, kind, oid, pack, found_at),
    `audit_log` (seq, actor, action, repo, detail JSON, at), `purges`
    (file BLOB PK, at, actor), plus an index on `file_scopes (scope,
    sha256)`.
- Consumes: M1 `Index`, `FileRecord`.

- [ ] **Step 1: Write the failing tests:**
  - `repos.rs`: `create_then_lookup_by_name_and_id`,
    `name_taken_case_insensitively` (`Org/Model` after `org/model` fails
    with `IndexError::NameTaken`), `list_filters_by_label_and_paginates`
    (1,500 repos, pages of 1,000 then 500, stable order),
    `set_labels_replaces`, `file_by_sha256_is_per_scope`,
    `purge_removes_every_scope_row_and_dedup_candidates`,
    `audit_rows_are_append_only`.
  - `git_store.rs`: `snapshot_reads_refs_and_committed_packs_only`,
    `commit_applies_all_updates_or_none` (two updates, second mismatched
    → neither applied, `RefMismatch`), `commit_requires_pending_pack`,
    `commit_fenced_by_full_repack_gen`, `commit_writes_one_ref_log_row_per_update`,
    `mark_pack_is_guarded` (`Pending → Lost` succeeds once; `Expired →
    Committed` never), `lease_epoch_increases_and_expires`,
    `publish_repack_rejects_stale_epoch_and_advanced_ref_log`,
    `unplaced_rows_persist_and_clear_by_coverage`,
    `concurrent_commits_one_winner` (8 tasks, 20 rounds, on SQLite, as
    S9's test; the Postgres run comes with M3).
- [ ] **Step 2: Run** `cargo nextest run -p openxet-index`. Expected: FAIL.
- [ ] **Step 3: Implement.** `commit` is one `BEGIN IMMEDIATE`
  transaction: `UPDATE refs … WHERE oid = $old` (or `INSERT` for a create
  with `old = None` failing if present, `DELETE … WHERE oid = $old`),
  `UPDATE packs SET state = 'committed' WHERE id = $pack AND state =
  'pending'`, `SELECT full_repack_gen` check, `INSERT ref_log` rows; any
  row count ≠ 1 rolls back with the matching `CommitError`.
- [ ] **Step 4: Run.** Expected: PASS, with the M1 index tests unchanged
  and `rebuild_scope_tables` still passing.
- [ ] **Step 5: Commit** as `feat(index): repositories, refs, packs, leases, audit and purge tables (migration 0006)`.

### Task 5: Bucket records for repositories, packs, ref logs and purges

**Files:**
- Create: `crates/storage/tests/repo_records.rs`
- Modify: `crates/storage/src/{lib,records,local}.rs`

**Interfaces:**
- Produces:

```rust
pub enum Key { /* M1 variants, plus: */
    RepoMeta { repo: RepoIdHex },                       // repos/{id}/meta
    Pack { repo: RepoIdHex, name: String, ext: PackExt },   // repos/{id}/packs/{name}.{pack|idx|rev|bitmap|mtimes}
    Midx { repo: RepoIdHex, gen: u64, ext: MidxExt },   // repos/{id}/midx/{gen}.{multi-pack-index|bitmap|rev}
    RefLog { repo: RepoIdHex, seq: u64 },               // repos/{id}/reflog/{seq:020}
    Purge(MerkleHash),                                  // purges/{hex}
}
pub struct RepoMeta { pub v: u8, pub id: String, pub namespace: String, pub name: String, pub private: bool, pub labels: Vec<String>, pub created_at: u64 }
pub struct RefLogRecord { pub v: u8, pub seq: u64, pub entries: Vec<RefLogEntry> }   // derived from ref_log rows (ADR 0004 item 3)
#[async_trait] pub trait Bucket { /* M1, plus: */
    async fn put_replace(&self, key: Key, bytes: Bytes) -> Result<(), StorageError>;   // durable overwrite, for RepoMeta only
    async fn put_stream(&self, key: Key, body: BoxStream<..>, len: u64) -> Result<(), StorageError>;   // packs
    async fn get_to_file(&self, key: Key, path: &Path) -> Result<u64, StorageError>;   // hydration; fsync
    async fn last_modified(&self, key: Key) -> Result<Option<u64>, StorageError>;      // reindex created_at
    async fn list_under(&self, repo: RepoIdHex, kind: KeyKind) -> Result<Vec<Key>, StorageError>;
}
```

- Consumes: M1 `Bucket`, `LocalBucket`, `create_if_absent` semantics of M1-R37.

- [ ] **Step 1: Write the failing tests:** `repo_meta_replace_is_durable_and_atomic`
  (a crash-simulating `put_replace` never leaves a truncated record: the
  local store writes a temp file, fsyncs, renames), `pack_round_trips_through_file`,
  `list_under_filters_kind`, `purge_record_is_create_only`,
  `keys_display_as_documented`, `last_modified_reports_put_time`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement**, documenting every new key in the crate docs'
  object list.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** as `feat(storage): repository metadata, pack, ref-log and purge records`.

### Task 6: `openxet-git` core: node cache, snapshots and the git process

**Files:**
- Create: `crates/git/{Cargo.toml,src/lib.rs,src/store.rs,src/snapshot.rs,src/process.rs,src/config.rs}`, `crates/git/tests/{snapshots.rs,process.rs}`, `crates/git/testdata/` (a script building the two S9 repos, `tf10k`-like from a fixed seed and `lfs`, into fixtures on demand)

**Interfaces:**
- Produces:

```rust
pub struct GitConfig { pub cache_dir: PathBuf, pub cache_max_bytes: u64, pub reachability: Budget, pub thin_bases: Budget, pub max_plain_blob_bytes: Option<u64> }
pub struct Engine { .. }   // Arc'd by the server: index (GitStore + Repos), bucket, config, git binary path
impl Engine {
    pub async fn start(index, bucket, cfg: GitConfig) -> Result<Self, GitError>;   // git ≥ 2.37 check, st_dev + probe-link check, sweep of leaked snapshots, captured v2 advertisement
    pub async fn snapshot(&self, repo: RepoId) -> Result<SnapshotRepo, GitError>;  // hydrates missing committed packs, writes packed-refs + pinned config, hard-links exactly the set
}
pub struct SnapshotRepo { pub dir: TempDir, pub refs: Vec<(String, Oid)>, pub packs: Vec<Pack>, pub full_repack_gen: u64 }
pub fn git_command(git: &Path, snapshot: &SnapshotRepo, protocol: Option<u8>) -> tokio::process::Command;   // sanitised env (ADR 0004 item 7)
pub const PINNED_CONFIG: &[(&str, &str)];   // ADR 0004 item 1
```

- Consumes: Task 4 `GitStore`, Task 5 pack keys.

- [ ] **Step 1: Write the failing tests:**
  - `snapshot_exposes_only_its_pack_set` (ADR 0004): with packs in every
    state in the cache, the snapshot links exactly the committed ones, has
    no `objects/info/alternates`, and an in-flight snapshot keeps its
    links after another pack is superseded.
  - `snapshot_dir_same_filesystem` and `stale_snapshots_swept`.
  - `hydrated_pack_mtime_pinned` (the hydration half).
  - `repo_paths_from_ids`: a repo named `../x` never yields a path outside
    the cache (paths come from ids only).
  - `git_version_floor`: a fake `git` printing `git version 2.36.0` fails
    `start` with the actionable error.
  - `git_env_sanitised`: the spawned env is exactly the ADR list; an
    injected `GIT_PROTOCOL=version=2;evil` is dropped; `HOME` config
    changes nothing.
  - `upload_pack_config_pinned`: `git config --list` in a snapshot shows
    every `PINNED_CONFIG` value.
  - `eviction_counts_links`: a file linked by a snapshot is not counted as
    freed.
- [ ] **Step 2: Run** `cargo nextest run -p openxet-git`. Expected: FAIL.
- [ ] **Step 3: Implement.** Hydration downloads `.pack`, `.idx`, `.rev`,
  `.bitmap`, `.mtimes` and the current MIDX generation when every pack it
  covers is in the set; the cache is `{cache_dir}/repos/{id}/pack-*`,
  snapshots `{cache_dir}/snapshots/{uuid}/`. Pack mtimes are set to the
  row's `created_at`.
- [ ] **Step 4: Run.** Expected: PASS on macOS and Linux.
- [ ] **Step 5: Commit** as `feat(git): node cache, per-request snapshot repos and the sanitised git process (ADR 0004)`.

### Task 7: upload-pack: advertisement, `ls-refs`, the interposer and R22

**Files:**
- Create: `crates/git/src/{upload.rs,interpose.rs,reach.rs,pkt.rs}`, `crates/git/tests/{upload_pack.rs,reachability.rs}`, `fuzz/fuzz_targets/pkt_line.rs`

**Interfaces:**
- Produces:

```rust
impl Engine {
    pub async fn advertise(&self, repo: RepoId, protocol: u8, service: Service) -> Result<Bytes, GitError>;   // v0/v1 from git on a snapshot; v2 from the captured, filtered advertisement
    pub async fn upload_pack(&self, repo: RepoId, protocol: u8, who: &str, body: BodyStream) -> Result<ResponseStream, GitError>;   // interposes, checks wants, forwards to `git upload-pack --stateless-rpc`
}
pub fn parse_v2_command(pkt: &[u8]) -> Result<V2Command, PktError>;   // ls-refs | fetch{wants, haves, ..} | Other(name)
pub struct Reachability { .. }   // one walk per request, gix commit-graph assisted, budgets + per-repo/per-principal semaphores
impl Reachability { pub async fn check(&self, snapshot: &SnapshotRepo, wants: &[Oid], who: &str) -> Result<(), Refused>; }
```

- Consumes: Task 6.

- [ ] **Step 1: Write the failing tests:**
  - `v2_command_allowlist` (ADR 0004): the advertisement lists only
    `agent`, `ls-refs`, `fetch` without `filter`, `server-option`,
    `object-format`; `command=object-info`, `command=bundle-uri` and
    `command=nonsense` get `ERR` and git is never spawned (assert via a
    spawn counter). Parametrised over the git on `PATH` and
    `OPENXET_TEST_GIT_237` when set (Task 19 adds the CI job).
  - `v2_want_unreachable_refused` (§5.9 req 8): the four cases of ADR
    0004, plus a reachable non-tip want succeeding.
  - `v0_wants_checked_too`: a `GIT_PROTOCOL` v0 request with an unreachable
    want is refused before git runs.
  - `reachability_budget_enforced`: commit, time and both concurrency
    limits; a tree or blob want passes only as a direct ref target.
  - `ls_refs_matches_git`: in-process `ls-refs` output equals `git
    upload-pack --advertise-refs` of the same snapshot (peeled tags
    included, `ref-prefix` honoured, `symrefs` for `HEAD`).
  - `fetch_round_trip`: a real `git clone` and `git fetch` over a stdio
    harness that feeds `upload_pack`, byte-identical checkout, protocol
    v0, v1 and v2.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** The interposer parses pkt-lines up to the
  first `done`/flush of the want section, collects wants, runs
  `Reachability::check`, then replays the buffered bytes and pipes the
  rest to git. `HEAD` resolves to `refs/heads/main`, or the repository's
  stored default branch when a later milestone adds one.
- [ ] **Step 4: Run**, plus `cargo +nightly fuzz run pkt_line -- -max_total_time=60`.
  Expected: PASS.
- [ ] **Step 5: Commit** as `feat(git): upload-pack with in-process ls-refs, the v2 allowlist and R22 reachability`.

### Task 8: receive-pack: the hook, pack publish and the fenced commit

**Files:**
- Create: `crates/git/src/{receive.rs,hook.rs,thin.rs,publish.rs}`, `crates/git/tests/{receive_pack.rs,hook.rs,thin_bases.rs}`, `crates/openxet/src/hook_cmd.rs` (the hidden `openxet proc-receive` subcommand the hook directory points at)

**Interfaces:**
- Produces:

```rust
impl Engine {
    pub async fn receive_pack(&self, repo: RepoId, protocol: u8, who: &UserPrincipal, body: BodyStream) -> Result<ResponseStream, GitError>;
}
pub struct HookServer { .. }   // one per request: socketpair, single-use token bound to (repo, principal, request id)
pub fn classify_sent(idx: &Path, client_pack_bytes: u64, header_count: u32) -> SentObjects;   // ADR 0004 item 3: entries past the client's data are appended bases
pub async fn check_writer_rule(snapshot: &SnapshotRepo, new_tips: &[Oid], sent: &SentObjects, budget: &Budget) -> Result<Path, Refused>;   // fast paths then fallback walk; Refused carries the `--no-thin` hint
pub struct Publisher { .. }   // insert_pack(pending) → put_stream → GitStore::commit → ok/ng; failure → mark Lost
```

- Consumes: Tasks 4, 5, 6.

- [ ] **Step 1: Write the failing tests:**
  - `push_round_trip`: `git push` of new commits over the stdio harness
    lands as a `committed` pack in the bucket with its row, refs moved,
    `ref_log` rows written; a fresh node's snapshot serves them and
    `git fsck --strict` is clean.
  - `hook_channel_private`, `hook_cannot_choose_repo`,
    `hook_pack_path_confined`, `hook_policy_rechecked` (ADR 0004).
  - `failed_commit_marks_pack_lost`, `cas_requires_pending_pack`,
    `push_cannot_use_foreign_pending_objects`, `concurrent_push_one_winner`
    (8 clients, 2 engines on one index and bucket, 20 rounds).
  - `ref_log_in_cas_transaction` (§5.9 req 8), the push half.
  - `writer_cannot_make_unreachable_reachable` (§5.9 req 8): the four
    routes of ADR 0004, the refusal text equal to git's missing-object
    text, the same push succeeding when it sends the object, a stock thin
    push taking the fast path, and the three known-limitation clients
    each either succeeding or refused with the `--no-thin` hint and then
    succeeding with `--no-thin`; the test asserts which path each took.
  - `plain_large_blob_reported` and `plain_large_blob_refused_above_limit`
    (Review Focus 3): a 20 MiB non-pointer blob pushed without LFS.
  - `publish_then_cas_survives_faults`, the push cases (kill between
    publish and CAS, between CAS and ack), with `fault-injection`
    feature points in `Publisher`; the repack cases come in Task 9.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** The hook binary is `openxet` itself: the hook
  directory holds `proc-receive` as a symlink to the running executable,
  created at `Engine::start`. The hook reads git's proc-receive protocol
  on stdin, forwards commands and the quarantine pack path over
  `OPENXET_HOOK_FD` with `OPENXET_HOOK_TOKEN`, and relays `ok`/`ng`.
  Branch policy in M2: an update's old value must equal the snapshot's
  ref (the stale-update fence); a non-fast-forward update is accepted,
  since a forced push (`+ref`) can't be told apart on the wire; `main`
  can't be deleted. Branch protection is a later feature.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** as `feat(git): receive-pack through the hardened proc-receive hook and the fenced commit transaction`.

### Task 9: Repack, lease, the pack sweep and reindex (library)

**Files:**
- Create: `crates/git/src/{repack.rs,lease.rs,sweep.rs,reindex.rs,reflog.rs}`, `crates/git/tests/{repack.rs,lifecycle_props.rs,reindex.rs}`

**Interfaces:**
- Produces:

```rust
pub async fn repack_geometric(engine: &Engine, repo: RepoId, lease: &Lease) -> Result<RepackReport, GitError>;
pub async fn repack_full(engine: &Engine, repo: RepoId, lease: &Lease, grace: Duration) -> Result<RepackReport, GitError>;   // keep refs from ref_log window + unplaced; cruft; gen bump; aborts on ref_log advance
pub async fn sweep_packs(engine: &Engine, repo: RepoId, grace: Duration, now: u64, dry_run: bool) -> Result<SweepReport, GitError>;   // pending→expired, deletes superseded/lost/expired past grace with side files and old MIDX gens
pub async fn derive_bucket_ref_log(engine: &Engine, repo: RepoId) -> Result<u64, GitError>;   // ref_log rows → RefLog records, within 60 s (a server task calls it)
pub async fn reindex_repo(engine: &Engine, repo: RepoId) -> Result<ReindexReport, GitError>;   // rebuilds refs, ref_log, packs; records unplaced
pub async fn reindex_restore(engine: &Engine, repo: RepoId, oid: Oid, refname: &str) -> Result<(), GitError>;
pub async fn reindex_discard(engine: &Engine, repo: RepoId, dry_run: bool) -> Result<DiscardReport, GitError>;
```

  The M4 `openxet gc` and `openxet reindex` commands call these; M2
  exposes them to tests and to a background task in `serve` that runs
  `derive_bucket_ref_log` every 10 s and geometric repacks when a repo
  has more than 50 committed packs.
- Consumes: Task 8.

- [ ] **Step 1: Write the failing tests**, one per remaining ADR 0004
  checklist item and named test: `repack_publish_lease_fenced`,
  `geometric_repack_drops_nothing_and_takes_no_fence`,
  `full_repack_fence_both_directions`, `keep_refs_cover_ref_log_window`,
  `hydrated_pack_mtime_pinned` (the re-pin half), `mtimes_rev_midx_travel_through_bucket`,
  `gc_removes_pack_side_files`, `sweep_deletes_only_past_grace`,
  `lost_packs_not_hydrated`, `pending_packs_aged_out_by_gc`,
  `push_cannot_use_superseded_objects`, `ref_log_in_cas_transaction` (the
  derivation and retention half), `reindex_unplaced_persisted`,
  `reindex_hold_never_blocks_repack`, `recovered_packs_never_exposed`,
  `publish_then_cas_survives_faults` (the five repack and Hub-ref cases;
  the Hub cases call Task 10's functions, so this test lands in Task 10's
  PR if Task 9 merges first), and `lifecycle_props`: a proptest state
  machine over random interleavings of push, Hub ref create/update/delete,
  Hub commit, geometric and full repack, lease takeover, sweep, hydration
  on a second engine, and reindex with restore and discard, asserting
  I1 after every step (every ref's closure in committed packs, checked
  with `git rev-list --objects` on a fresh snapshot) and that nothing R22
  refuses is served.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Repacks run in a working copy built like a
  snapshot plus `refs/openxet/keep/<n>`; outputs follow the pushed-pack
  pattern (row `pending`, upload, publish).
- [ ] **Step 4: Run**, with `PROPTEST_CASES=200` locally once. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(git): repack with lease and fence, pack sweep, bucket ref log and reindex (ADR 0004 checklist)`.

### Task 10: Hub object layer: reads at a revision and Hub commits with gix

**Files:**
- Create: `crates/git/src/objects.rs`, `crates/git/tests/hub_objects.rs`

**Interfaces:**
- Produces:

```rust
pub enum Revision<'a> { Ref(&'a str), Oid(Oid) }
pub struct Resolved { pub commit: Oid, pub tree: Oid }
impl Engine {
    pub async fn resolve_revision(&self, snap: &SnapshotRepo, rev: Revision<'_>) -> Result<Resolved, GitError>;   // ref name, or an OID passing R22 (tip, peeled tag, or walk); tree/blob OIDs refused as RevisionNotFound
    pub async fn read_tree(&self, snap: &SnapshotRepo, commit: Oid, path: &str, recursive: bool) -> Result<Vec<Entry>, GitError>;   // Entry { path, kind: File{oid, size, lfs: Option<Pointer>} | Dir{oid} }
    pub async fn read_blob(&self, snap: &SnapshotRepo, commit: Oid, path: &str) -> Result<Blob, GitError>;   // Blob { oid, bytes } or Pointer
    pub async fn log(&self, snap: &SnapshotRepo, from: Oid, cursor: Option<Oid>, limit: usize) -> Result<Vec<CommitInfo>, GitError>;
    pub async fn last_commit_for_paths(&self, snap: &SnapshotRepo, commit: Oid, paths: &[&str]) -> Result<Vec<Option<CommitInfo>>, GitError>;   // for expand=true
    pub async fn hub_commit(&self, repo: RepoId, who: &UserPrincipal, req: HubCommit<'_>) -> Result<Oid, GitError>;   // ADR 0004 item 6 steps 1–6, retried 3× on Fenced/RefMismatch
    pub async fn hub_ref_write(&self, repo: RepoId, who: &UserPrincipal, update: RefUpdate, tag: Option<TagInfo>) -> Result<(), GitError>;   // create/update/delete with the writer rule; annotated tags built with gix
}
pub struct HubCommit<'a> { pub branch: &'a str, pub parent: Option<Oid>, pub summary: &'a str, pub description: &'a str, pub ops: Vec<Op> }
pub enum Op { File { path: String, content: Bytes }, LfsFile { path: String, sha256: [u8; 32], size: u64 }, DeleteFile(String), DeleteFolder(String) }
pub fn canonical_pointer(sha256: &[u8; 32], size: u64) -> Vec<u8>;   // ADR 0005 item 9
pub fn parse_pointer(blob: &[u8]) -> Option<Pointer>;              // tolerates unknown keys, never writes them
```

- Consumes: Tasks 6, 8 (`Publisher`, `check_writer_rule`).

- [ ] **Step 1: Write the failing tests:** `hub_read_by_sha_obeys_r22`
  (§5.9 req 8), `hub_ref_write_fenced` (§5.9 req 8),
  `hub_commit_on_snapshot` (ADR 0004), `hub_commit_builds_expected_tree`
  (files, pointers, deletes, nested folders, a folder delete removing
  every entry; `git fsck --strict` clean on a fresh snapshot),
  `pointer_canonical_form` (ADR 0005), `concurrent_hub_commits_both_land`
  (Review Focus 2: 8 tasks commit distinct files to `main`; all land,
  each retry took a fresh snapshot), `ref_log_in_cas_transaction` (the
  Hub half), `writer_cannot_make_unreachable_reachable` (the Hub routes:
  `create_branch` at an unreachable OID, a commit naming an unreachable
  tree entry).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Hub commit packs are written with
  `gix_pack::data::output` from the new objects only, indexed with
  `git index-pack --strict` in the quarantine, connectivity-checked with
  `git rev-list --objects <new> --not <tips>` against the snapshot
  (quarantine as object dir, snapshot as its only alternate), then
  published like a push.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** as `feat(git): Hub reads at a revision and Hub commits as fenced pushes (gix in-process)`.

### Task 11: `openxet-hub` skeleton: repositories, views, labels, whoami

**Files:**
- Create: `crates/hub/{Cargo.toml,src/lib.rs,src/error.rs,src/authz.rs,src/repos.rs,src/views.rs,src/labels.rs,src/whoami.rs,src/misc.rs,src/test_support.rs}`, `crates/hub/tests/{repos.rs,views.rs,labels.rs,security.rs}`
- Modify: `crates/openxet/src/{serve.rs,config.rs}` (`[hub]` section, mounting), `docs/reference/config.md` (`[hub]`)

**Interfaces:**
- Produces:

```rust
pub struct HubState { pub index: Arc<dyn Index + Repos + GitStore>, pub bucket: Arc<dyn Bucket>, pub auth: Arc<Authenticator>, pub authz: Arc<dyn Authorizer>, pub signer: Arc<TokenSigner>, pub caps: Arc<CapabilitySigner>, pub engine: Arc<openxet_git::Engine>, pub cfg: HubConfig, pub clock: Arc<dyn Clock> }
pub struct HubConfig { pub public_url: Url, pub cas_url: Url, pub lfs_threshold: u64, pub max_commit_bytes: u64, pub token_ttl: Duration /* 15 min */, pub page_size: usize }
pub fn routes() -> Routes<HubState>;   // merges every module's routes
pub enum HubError { .. }                // IntoResponse: M2-R6 body and headers
pub struct RepoAccess { pub repo: Repo, pub who: UserPrincipal, pub view: View }   // extractor: resolves {prefix}{ns}/{name} or /api/{type}s/{id}, authorizes read, applies the typed-view rule; never leaks existence
pub enum View { Model, Dataset, Space, Kernel, Any }
pub async fn require(state: &HubState, who: &UserPrincipal, action: Action, repo: &Repo) -> Result<(), HubError>;
pub async fn create_repo(state: &HubState, who: &UserPrincipal, ns: &str, name: &str, private: bool, label: &str) -> Result<Repo, HubError>;   // authorize Create first, then name check, RepoMeta record, index row, audit; Task 12 adds the initial commit of M2-R5 once Task 10 has merged
```

  - Routes: `GET /api/whoami-v2`, `POST /api/repos/create`, `DELETE
    /api/repos/delete`, `GET /api/{type}s` (listing filtered by label,
    paginated), `GET /api/{type}s/{ns}/{name}` (repo info without
    siblings until Task 12 adds them), `POST /api/validate-yaml` (`200
    {"errors":[],"warnings":[]}`), `GET /api/agent-harnesses` (`501`),
    the labels API (M2-R12), and `501` with the endpoint name for the
    documented not-implemented Hub routes (model cards, discussions,
    likes, search, collections).
- Consumes: Tasks 2, 3, 4, 5.

- [ ] **Step 1: Write the failing tests:**
  - `repos.rs`: `create_answers_s3_shape` (fixture `04`), `create_409_carries_url`
    (fixtures `05`, `06`), `create_in_foreign_namespace_403_names_namespace`
    (fixture `08`, text from S3 lesson 12), `delete_is_text_ok_and_404_shape`
    (fixtures `91`–`93`), `untyped_create_adds_model_label`,
    `names_validated` (each M2-R2 rule → `400` with the rule in the hint),
    `name_taken_case_insensitively_409`.
  - `views.rs`: `typed_listing_filters_by_label`,
    `wrong_type_with_read_access_shows_label_hint`,
    `unprefixed_git_address_serves_any_label` (through `RepoAccess` with
    `View::Any`).
  - `security.rs`: `repo_create_authorized_per_namespace`,
    `wrong_type_without_read_access_matches_unknown_repo` (byte compare
    apart from `X-Request-Id` and `Date`), `create_collision_authorized_first`,
    `unreadable_repo_matches_unknown_repo_on_every_route` (the Hub half
    of `credential_scoped_to_repo`, as a matrix over `endpoints()`),
    `responses_never_echo_credential` (Hub half), `anonymous_reads_only_with_rule`.
  - `whoami.rs`: `whoami_has_every_field_the_client_reads` (fixture `01`;
    `name`, `orgs[].name`, `auth.accessToken.{displayName,role}`, `id`,
    `type`, `fullname`, `avatarUrl`, `isPro`).
  - `labels.rs`: `label_add_remove_list_round_trip`,
    `label_rules_enforced` (slug, 64 bytes, 32 per repo),
    `label_change_reaches_bucket_before_index_and_writes_audit`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Fixture-driven assertions read
  `compat/fixtures/hub/*.json` and compare the keys S3 lists as read,
  with a helper in `test_support.rs` shared by Task 12.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** as `feat(hub): repositories, typed views, labels and whoami on the Hub API (ADR 0010)`.

### Task 12: The Hub upload and read API

**Files:**
- Create: `crates/hub/src/{tokens.rs,preupload.rs,commit.rs,refs.rs,tree.rs,info.rs,ndjson.rs}`, `crates/hub/tests/{tokens.rs,upload.rs,read.rs}`, `fuzz/fuzz_targets/commit_ndjson.rs`

**Interfaces:**
- Produces the routes of design §6 with S3's shapes:
  `GET /api/{type}s/{id}/xet-{read|write}-token/{rev}` (body and headers;
  `write` needs `Action::Write`; the revision is accepted and ignored,
  `None` included), `POST …/preupload/{rev}`, `POST …/commit/{rev}`
  (NDJSON, M2-R7), `POST|DELETE …/branch/{branch}`, `POST …/tag/{rev}`,
  `DELETE …/tag/{tag}`, `GET …/refs`, `GET …/tree/{rev}[/{path}]`,
  `POST …/paths-info/{rev}` (form-encoded), `GET …/commits/{rev}`,
  `GET …/{id}[/revision/{rev}]` with `siblings`, `sha`, `lastModified`
  and `blobs=true`.
  - `pub fn parse_commit_ndjson(body: &[u8], limits: &Limits) -> Result<HubCommit, HubError>` in `ndjson.rs`, the fuzz target's entry.
- Consumes: Tasks 10, 11, M1 `TokenSigner`.

- [ ] **Step 1: Write the failing tests:**
  - `tokens.rs`: `token_in_body_and_headers` (fixtures `09-…-03`,
    `27-…-02`), `write_token_needs_write_access`, `token_scope_is_repo_id_and_tenant_namespace`,
    `write_token_refresh_ignores_revision` (Review Focus 5),
    `read_token_lets_cas_reconstruct_only_this_repo` (end to end through
    the composed router).
  - `upload.rs`: `preupload_routes_by_size_and_gitattributes` (M2-R4),
    `zero_byte_and_tiny_files_regular`, `commit_round_trip` (fixture
    `09-…-14`: regular files, an `lfsFile` whose sha256 was registered by
    a shard upload to this repo, deletes), `commit_rejects_lfs_file_registered_to_another_scope`
    (Review Focus 1: `400` whose hint says to upload the file to this
    repository), `commit_parent_mismatch_412`, `commit_limits_enforced`,
    `commit_ndjson_accepts_python_separators`, `readme_commit_posts_validate_yaml_first`
    (fixture `09-…-01`, the stub answer), `branch_and_tag_shapes`
    (fixtures `11`–`13`, `30`, `31`; `409` text for an existing branch),
    `tag_target_is_the_annotated_tag_object` (fixture `14`).
  - `read.rs`: `tree_shapes` (fixtures `16`–`19`; `lfs` and `xetHash`
    fields; `EntryNotFound` with the S3 text), `tree_expand_last_commit`
    (fixture `17`), `tree_path_edges` (Review Focus 4), `paths_info_omits_missing`
    (fixture `20`), `commits_newest_first_with_total` (fixture `21`),
    `repo_info_shapes` (fixtures `22`, `23`, `25`; sibling `lfs.sha256`
    vs tree `lfs.oid`), `listings_paginate_with_link_next` (M2-R8).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** `xetHash` in tree entries comes from
  `file_by_sha256(scope)`; a pointer whose sha256 is unknown to the scope
  lists without `xetHash` and resolves as `basic` only.
- [ ] **Step 4: Run**, plus `cargo +nightly fuzz run commit_ndjson -- -max_total_time=60`.
  Expected: PASS.
- [ ] **Step 5: Commit** as `feat(hub): token routes, preupload, commits, refs, tree and repo info (S3 shapes)`.

### Task 13: `resolve` and the file gateway

**Files:**
- Create: `crates/hub/src/{resolve.rs,gateway.rs}`, `crates/hub/tests/{resolve.rs,gateway.rs}`

**Interfaces:**
- Produces: `HEAD|GET /{prefix}{ns}/{name}/resolve/{rev}/{path}` (S3
  lesson 3 headers; `302` for Xet files with `Location` per M2-R9,
  `X-Repo-Commit`, `X-Linked-Size`, `X-Linked-ETag`, `X-Xet-Hash`,
  `Link: <…/xet-read-token/{commit}>; rel="xet-auth", <{cas_url}/v1/reconstructions/{hash}>; rel="xet-reconstruction-info"`;
  `200` inline for blobs; `404 EntryNotFound` with `X-Repo-Commit`), and
  `HEAD|GET /v1/transfer/files/{repo id}/{xet hash}` streaming the
  reconstruction (reusing `openxet_cas`'s planner and the bucket's range
  stream) with the S4 conformance list.
- Consumes: Tasks 10, 12, M1 reconstruction planning (expose
  `openxet_cas::plan_file(index, scope, hash, range) -> Plan` under a
  `pub` API in this task).

- [ ] **Step 1: Write the failing tests:** `resolve_small_file_inline`
  (fixture `26`), `resolve_xet_file_302_with_headers` (fixture `27-…-01`),
  `resolve_404_carries_repo_commit` (fixture `28`), `resolve_path_edges`
  (Review Focus 4), `resolve_anonymous_private_401_shape` (fixture `35`),
  `gateway_conformance` (HEAD; `bytes=a-b`, `bytes=a-`, `bytes=-n`; `416`
  with `Content-Range: bytes */size`; `ETag` and `If-Range` match and
  mismatch; `Content-Disposition`; byte-exact against the stored file for
  the reference file and a 3-xorb file), `gateway_needs_credential_scoped_to_repo`
  (a token for another repository gets the unknown-repo answer),
  `no_unauthenticated_reads` (the Hub extension of M1's test: every
  Hub, gateway and LFS URL OpenXet emits, requested without a credential,
  is refused), `duckdb_reads_through_resolve` (ignored unless `duckdb` is
  on `PATH`; documents S4's recipe).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** The gateway holds at most one 1 MiB block per
  transfer (M1-R27) and reserves nothing from the memory budget beyond
  the plan.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** as `feat(hub): resolve with Xet headers and the HTTP file gateway`.

### Task 14: The Git LFS endpoint for `git-xet` and Xet-native clients

**Files:**
- Create: `crates/hub/src/lfs/{mod.rs,batch.rs,capability.rs,tokens.rs}`, `crates/hub/tests/lfs.rs`, `compat/fixtures/lfs/jj-xet-expectations.json` (the expectations of jj-xet's `tests/openxet.rs` mock, transcribed)

**Interfaces:**
- Produces, under `/{prefix}{ns}/{name}[.git]/info/lfs`:
  `POST objects/batch` (Basic or Bearer; `401` with `LFS-Authenticate`
  headers when missing; uploads offering `xet` answer `transfer: xet`
  with a capability `href` and the five headers; uploads without `xet`
  answer M2-R10; downloads answer `basic` with the gateway `href` and
  `header: {"X-Xet-Hash"}` only, or an object error `404` for an oid not
  registered to the scope), `POST objects/verify` (`200 {}`),
  `GET xet-read-token` and `GET xet-write-token` (ADR 0005), and the
  capability route `GET /v1/capabilities/{repo id}/{op}?exp&sig` (ADR
  0003 decision 2: no header authentication, ignored and never logged).
- Consumes: Tasks 3 (`CapabilitySigner`), 12 (token minting).

- [ ] **Step 1: Write the failing tests:** `upload_batch_answers_xet_with_capability_href`
  (fixture `hf/02`; header casing exact, `X-Xet-Token-Expiration` an
  epoch 15 min ahead), `download_batch_always_basic` (ADR 0003 unit test;
  `xet` offered and not), `lfs_download_action_has_no_token` (§5.9 req
  6), `capability_href_refreshes_without_authorization`,
  `capability_ignores_and_never_logs_authorization` (valid sig + garbage
  header succeeds; bad sig + header fails; the log holds neither),
  `capability_href_outlives_first_token` (Review Focus 5),
  `batch_without_xet_answers_object_error_501` (M2-R10),
  `batch_unknown_oid_is_object_404`, `first_push_creates_repo_when_create_allowed`
  (through `RepoAccess`), `lfs_token_routes_match_adr_0005` (errors
  401/403/404 with the error body; Basic and Bearer), `jj_xet_mock_expectations`
  (replays the transcribed expectations), `pointer_sha256_unknown_lists_without_xet_hash`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.** Capability lifetime is 60 min; the CAS token it
  returns lives `token_ttl`; both are configurable in `[hub]`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** as `feat(hub): Git LFS batch with xet uploads, basic downloads and capability hrefs (ADRs 0003, 0005)`.

### Task 15: Git smart HTTP routes and the composed server

**Files:**
- Create: `crates/hub/src/githttp.rs`, `crates/hub/tests/githttp.rs`, `crates/openxet/tests/hub_serve.rs`
- Modify: `crates/openxet/src/{serve.rs,config.rs}` (`[git]` section, engine start, the ref-log and repack background tasks of Task 9, the `cas-token.key`), `docs/reference/config.md` (`[git]`)

**Interfaces:**
- Produces: `GET /{prefix}{ns}/{name}[.git]/info/refs?service=git-{upload|receive}-pack`,
  `POST …/git-upload-pack`, `POST …/git-receive-pack` (M2-R11), wired to
  `Engine::{advertise,upload_pack,receive_pack}`; the dumb-protocol paths
  answer `404`. The `openxet serve` command mounts CAS, Hub and Git routes in one
  router and prints the Hub endpoint lines in the banner (`HF_ENDPOINT=`,
  `hf auth login --token`, `git clone` example).
- Consumes: Tasks 7, 8, 11.

- [ ] **Step 1: Write the failing tests:** `clone_push_fetch_over_http`
  (real `git` against the composed router on a port: v0 via
  `GIT_PROTOCOL=version=0`, v2 default; gzip request bodies with a
  large push; typed prefix and `.git` variants; Basic password = token),
  `receive_pack_needs_write`, `upload_pack_needs_read_and_hides_private`
  (unknown-repo answer), `dumb_protocol_not_served`,
  `cold_node_serves_from_bucket` (second engine, empty cache, clone
  byte-identical), `banner_lists_hub_and_git_lines`,
  `restart_keeps_repos_tokens_and_capabilities`,
  `credentials_never_logged` (extended: Basic passwords, capability
  `sig`, CAS tokens through every new route at `trace` level).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo nextest run --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(server): Git smart HTTP and the composed CAS, Hub and Git server`.

### Task 16: `openxet-uri`: the `xet://` specification, parser and formatter (M2-URI)

**Files:**
- Create: `crates/uri/{Cargo.toml,src/lib.rs,src/parse.rs,src/format.rs,src/legacy.rs,src/resolve.rs}`, `crates/uri/tests/{vectors.rs,props.rs,legacy.rs,endpoints.rs}`, `crates/uri/testdata/vectors.json`, `docs/reference/xet-uri.md` (the specification), `docs/research/xet-uri-legacy-audit-2026-10.md`, `fuzz/fuzz_targets/xet_uri.rs`
- Modify: `docs/SUMMARY.md`

**Interfaces:**
- Produces:

```rust
pub struct XetUri { pub host: String, pub port: Option<u16>, pub kind: Kind, pub namespace: String, pub name: String, pub revision: Option<String>, pub path: Option<String> }
pub enum Kind { Repository, Dataset, Space, Kernel, Bucket }
impl FromStr for XetUri { type Err = UriError; }     // UriError { code, message, hint }: actionable, never silently rewrites
impl Display for XetUri;                             // canonical form; parse(format(u)) == u
impl XetUri { pub fn https(&self, scheme: &str) -> Url; }   // the HTTPS resolve/web URL on the same host
pub fn classify_legacy(input: &str) -> Legacy;       // Accepted(XetUri) | Translated{from, to, note} | Rejected{reason, hint}
pub struct Endpoints { .. }                          // credential selection by exact (host, port), never by default; redirects can't broaden
```

- Consumes: Task 1's rulings only.

- [ ] **Step 1: Write the specification** `docs/reference/xet-uri.md`:
  grammar (ABNF), resource kinds, mutable and immutable references,
  escaping and Unicode (NFC not applied; bytes percent-encoded), ports
  and IPv6 literals, the HTTPS mapping, reverse-proxy prefixes (a base
  path after the authority is not supported in M2 and is rejected with a
  hint; M6 revisits), local development (`xet://127.0.0.1:8080/…`), what
  a URI never carries (credentials), and the legacy policy with its
  matrix. Write the legacy audit note from the pinned Git-Xet and pyxet
  sources named in ADR 0009's research note, with versions and the
  fsspec registration facts; record that IANA registration has not
  happened.
- [ ] **Step 2: Write the failing tests:** golden vectors (at least 40:
  every kind, revisions with `/`, a `@` in a name rejected, UTF-8 paths,
  IPv6, ports, each malformed class), `round_trip` and
  `canonical_is_idempotent` proptests, `legacy_matrix` (every row of the
  audit's matrix), `endpoints_isolate_credentials` (two fixtures with the
  same names and different content: resolution picks the URI's host;
  an embedded credential is rejected; the default endpoint's token is
  never sent to another host; a redirect to another host doesn't carry
  it).
- [ ] **Step 3: Run.** Expected: FAIL.
- [ ] **Step 4: Implement**, then `cargo +nightly fuzz run xet_uri -- -max_total_time=60`.
- [ ] **Step 5: Run.** Expected: PASS; the docs build passes.
- [ ] **Step 6: Commit** as `feat(uri): the xet:// specification, parser, formatter and legacy policy (ADR 0009, M2-URI)`.

### Task 17: `openxet purge` and `openxet label`

**Files:**
- Create: `crates/openxet/src/{purge_cmd.rs,label_cmd.rs}`, `crates/openxet/tests/{purge.rs,label.rs}`
- Modify: `crates/openxet/src/cli.rs`, `compat/tests/test_agent_cli.py` (`JSON_COMMANDS`), `docs/reference/cli.md`, `docs/reference/exit-codes.md` (if a new code is needed; prefer none)

**Interfaces:**
- Produces: `openxet purge <file-hash> [--config] [--dry-run] [--yes] [--json]`
  (M2-R13; without `--yes` on a TTY it asks, with `--json` it never
  prompts and requires `--yes`), schema `openxet.purge.v1`
  `{schema, file, scopes_removed, dry_run}`; `openxet label add|remove
  <ns/name> <label>` and `openxet label list <ns/name>` against a
  running server (`--endpoint`, `--token`, as the client commands),
  schema `openxet.labels.v1` `{schema, repo, labels}`.
- Consumes: Tasks 4, 11.

- [ ] **Step 1: Write the failing tests:** `purge_dry_run_changes_nothing`,
  `purge_removes_file_from_every_scope_and_cas_answers_404`,
  `purge_writes_bucket_record_first`, `purge_json_requires_yes`,
  `label_round_trip_json`, `label_errors_are_structured_with_hints`.
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run**, plus the agent leg. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(client): openxet purge and openxet label`.

### Task 18: OpenAPI generated from code

**Files:**
- Create: `crates/openxet/src/openapi_cmd.rs`, `docs/reference/openapi.json`, `docs/reference/openapi.md`, `scripts/check-openapi.sh`
- Modify: `crates/http/src/routes.rs` (collect `utoipa` paths per route), every route module of `crates/{cas,hub}/src` (annotations), `crates/openxet/src/cli.rs`, `.github/workflows/ci.yml`, `AGENTS.md` (Commands), `docs/SUMMARY.md`

**Interfaces:**
- Produces: `openxet openapi` printing the document; `GET /openapi.json`
  serving the same bytes; `scripts/check-openapi.sh` diffing them against
  the checked-in file (CI gate).
- Consumes: Tasks 12, 14, 15.

- [ ] **Step 1: Write the failing test:** `openapi_lists_every_served_route`
  (every `Endpoint` from `endpoints()` appears as a path and method, with
  a response schema, and the error body schema is referenced by every
  4xx/5xx), `openapi_is_valid_3_1` (parsed by `utoipa`'s own types and
  checked against the JSON schema of OpenAPI 3.1 vendored under
  `crates/openxet/testdata/`).
- [ ] **Step 2: Run.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run**, then `scripts/check-openapi.sh`. Expected: PASS.
- [ ] **Step 5: Commit** as `feat(server): OpenAPI document generated from the routes, checked in CI (M2-R15)`.

### Task 19: Compatibility suite: Hub, Git, Gitea-LFS and jj-xet legs

**Files:**
- Create: `compat/tests/{test_hub.py,test_git.py,test_lfs_gitea.py,test_jj_xet.py}`, `compat/gitea/` (compose file, pinned image digest, seed script), `scripts/install-git-floor.sh` (builds git 2.37.7 from source into a cached prefix)
- Modify: `compat/conftest.py` (configured-mode server with user tokens and rules; `git`, `git-lfs`, `git-xet` tool versions in the report header), `compat/pyproject.toml` (markers `hub`, `git`, `lfs`), `compat/README.md`, `.github/workflows/ci.yml` (jobs `compat-hub`, `compat-git`, `compat-lfs` with a Gitea service, `git-floor`), `AGENTS.md` (Commands)

**Interfaces:**
- Consumes: the composed server (Task 15), the LFS endpoint (Task 14).

- [ ] **Step 1: Write the tests** (RED against a stub server is not
  meaningful here; RED is the suite failing before the server has the
  route, which it does if written before Task 15 merges; otherwise run
  once against M1's binary and record the failures):
  - `test_hub.py` (`-m hub`, stock `huggingface_hub` and `hf`):
    `test_auth_login_whoami`, `test_upload_folder_and_snapshot_download`
    (regular and Xet files, byte-exact), `test_one_gib_upload_takes_xet_path`
    (S3 lesson 1; the file's name matches no pattern; asserts `hf_xet`
    activity through the hook of `conftest`), `test_upload_dedups_across_repos`
    (Review Focus 1; the second upload's xorb bytes accepted are under
    2 MB), `test_branches_tags_refs_commits`, `test_list_tree_paths_info`,
    `test_hf_download_cli`, `test_untyped_create_is_model`,
    `test_typed_route_view_filters_by_label`, `test_wrong_type_route_is_repo_not_found`,
    `test_wrong_type_with_read_access_shows_label_hint`,
    `test_wrong_type_without_read_access_matches_unknown_repo`,
    `test_create_collision_answers_409_with_url`, `test_kernel_read_with_stock_client`
    (created and listed via `/api/kernels`, written over Git),
    `test_anonymous_private_download_is_401_shape`, `test_errors_render_in_hf_cli`
    (the `403` of S3 lesson 12 renders with the namespace).
  - `test_git.py` (`-m git`): `test_clone_push_pull_https`,
    `test_hub_commit_and_git_push_share_history`, `test_two_nodes_concurrent_push_one_winner`,
    `test_cold_node_clone_byte_identical`, `test_force_pushed_commit_not_fetchable_by_sha`.
  - `test_lfs_gitea.py` (`-m lfs`, ADR 0003's leg): a Gitea-hosted repo
    with `lfs.url` on OpenXet and the two git config keys written as
    `openxet setup git` (M6) will write them; `test_git_xet_push_refresh_without_authorization`
    (a 5 s `token_ttl` forces the refresh; the server log shows the
    capability request with no `Authorization`), `test_git_lfs_pull_basic`,
    `test_plain_git_lfs_push_gets_install_hint` (M2-R10).
  - `test_jj_xet.py`: replays `jj-xet-expectations.json` over HTTP.
- [ ] **Step 2: Wire CI**: `compat-hub` and `compat-git` on the debug
  binary; `compat-lfs` with Gitea as a service container pinned by digest
  and `git-lfs` 3.7.1 and `git-xet` 0.2.1 downloaded with checksum
  verification; `git-floor` builds git 2.37.7 once (cached) and runs
  `cargo nextest run -p openxet-git` with `OPENXET_TEST_GIT_237`.
- [ ] **Step 3: Run** every leg locally (Gitea via colima). Expected: PASS;
  every server log passes `assert_no_credential_in_log`.
- [ ] **Step 4: Security map**: every M2 row now names its file; the map
  check passes for Rust and pytest.
- [ ] **Step 5: Commit** as `test(compat): Hub, Git, Gitea-LFS and jj-xet legs with stock clients`.

### Task 20: Benchmarks and gates for M2

**Files:**
- Create: `crates/git/benches/fetch.rs` (S9's server-only fetch replay), `crates/hub/benches/metadata.rs` (token, resolve, tree p50/p99), `crates/git/tests/thin_bases_budget.rs`
- Modify: `crates/cas/tests/latency_gate.rs` (or a sibling `hub_latency_gate`), `.github/workflows/ci.yml`, `docs/reference/benchmarks.md`, `AGENTS.md` (Commands)

- [ ] **Step 1: Write the gates:** `hub_latency_gate`: token, resolve
  (HEAD) and tree p99 < 10 ms on an index of 10,000 repositories and a
  repository of 10,000 files, release build (design §5.8); `fetch_gate`:
  serving the S9-style 10k-commit clone costs at most 2× git's standalone
  CPU (S9's 0.18 s baseline measured on the runner in the same run).
- [ ] **Step 2: Run them 10 of 10 times in CI** (M1-R35) before marking
  them required.
- [ ] **Step 3: Measure the thin-base budget** (`thin_bases_budget.rs`):
  the fallback walk over the 10k-commit repo; set
  `git.thin_bases.max_objects` and `max_ms` defaults from the numbers and
  record them in ADR 0004 item 3 (replacing "placeholders") and in
  `benchmarks.md`, with the machine.
- [ ] **Step 4: Commit** as `perf(hub,git): metadata and fetch latency gates, thin-base budget measured`.

### Task 21: Docs and project state

**Files:**
- Create: `docs/quickstart-hub.md` (from `openxet serve` to `hf upload`, `hf download` and `git clone`, every block run by the docs leg), `docs/guides/git-hosting.md`, `docs/guides/lfs-for-any-repo.md` (the `lfs.url` setup, the two config keys, what `openxet setup git` will do in M6), `docs/reference/hub-routes.md`, `docs/reference/git-routes.md`, `docs/reference/lfs.md`
- Modify: `docs/reference/routes.md` (CAS only; link the new pages), `docs/reference/config.md`, `docs/reference/cli.md`, `docs/reference/security-tests.md`, `docs/README.md`, `docs/SUMMARY.md`, `CHANGELOG.md`, `AGENTS.md` (Current state: M2 complete, M3 next; Commands), `README.md`, `compat/tests/test_docs_quickstart.py` (runs both quickstarts), `website/index.html` only if a capability claim changes (per `design.md`)

- [ ] **Step 1: Write the pages** from the code and the reference pages
  each task added; every example runs; every table that shares a heading
  carries a `table-caption`.
- [ ] **Step 2: Run** `scripts/build-site.sh`, the docs leg, the
  security-map check and `scripts/check-openapi.sh`. Expected: PASS.
- [ ] **Step 3: Final review** of the whole M2 branch set by a fresh
  reviewer against design §12 M2, §5.9 and the ADR 0004 checklist (every
  box ticked with the test that covers it, in the ADR itself).
- [ ] **Step 4: Commit** as `docs: Hub quickstart, Git hosting and LFS guides, route references, M2 state`.

## Out of M2, by design

Assigned to later milestones by the design, not deferred for convenience:
- **M3:** OIDC and the webhook authorizer, OpenXet-issued access tokens
  and `openxet login`, Postgres (the `Repos` and `GitStore` traits are
  ready for it), presigned direct downloads, rate limits and quotas,
  delegated agent tokens.
- **M4:** the `openxet gc` and `openxet reindex` commands (M2 ships their
  library functions and tests), server-side chunking (plain `git-lfs`
  uploads, M2-R10), Git over SSH, the audit API over `audit_log`,
  metering.
- **M4, with the audit log:** post-receive webhooks for integrators
  (§10.9), since both attribute ref changes to an actor.
- **M5:** the bucket primitive and its routes.
- **M6:** `openxet setup git`, the download transfer agent, the URI in
  CLI, SDKs and UI (M6-URI), the web UI, MCP.
- **The `git-reachability` follow-up** (ADR 0004): bitmap-backed
  reachability and the `have`/`shallow` oracle, tracked as an issue
  opened by Task 7.
