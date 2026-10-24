# S7: `git-lfs` / `git-xet` handshake, credential sourcing and SSH

<!-- description: How git-lfs and git-xet negotiate Xet transfers and source credentials, over HTTPS and SSH. -->

- Date: 2026-10-23
- Clients:
  - `git-lfs` 3.5.1 and 3.7.1 (release binaries, checksums verified)
  - `git-xet` 0.2.1 (tag `git-xet-v0.2.1`, `23f68bb`)
  - git 2.47.3
  - all on Linux arm64 (`python:3.13-slim`)
- Servers:
  - huggingface.co
  - Gitea 1.24.7 (`gitea/gitea:1.24`, HTTPS and OpenSSH, `LFS_ALLOW_PURE_SSH=true`)
  - an openxet-shaped LFS stub on another host, following ADR 0005
- Source citations:
  - `git-xet` references are to `xet-core` tag `git-xet-v0.2.1`.
  - `git-lfs` references are to tags `v3.7.1` and `v3.5.1`.
  - The three `git-xet` behaviours that matter here are unchanged on
    `xet-core` `main` at `f42483b` (2026-10-23). That is checked in
    "Upstream drift" below.
- Questions (design §11, task brief):
  1. What are the exact LFS batch request and response for upload and
     download against huggingface.co, with `git-xet` 0.2.1 and without it?
  2. How do `git-lfs` 3.5.1 and 3.7.x pick credentials when `lfs.url` is on
     another host than the Git remote, over HTTPS and SSH? How does
     `git-xet` source its refresh credential? (Design §10.2.)
  3. Does a signed capability `href` with no `Authorization` header satisfy
     `git-xet`'s token refresh? (ADR 0005 item 10.)
  4. What is the `git-lfs-authenticate` response format, and can
     `git-lfs-transfer` (pure SSH) negotiate the `xet` custom transfer?
- Fixtures: `compat/fixtures/lfs/`. The README there describes the format,
  the scenarios and the redaction.

Labels used below:
- **observed:** seen in the captured traffic or client output.
- **source:** read in the client source at the cited line.
- **inferred:** our conclusion, not directly verified.

## Answers

1. **huggingface.co uses `xet` for uploads only, and its refresh `href` is
   the authenticated Hub token route** (observed, both `git-lfs` versions).
   - **Request:** `POST {repo}.git/info/lfs/objects/batch` with
     `Accept: application/vnd.git-lfs+json` and body
     `{"operation","objects":[{"oid","size"}],"transfers":[…],"ref":{"name":"refs/heads/main"},"hash_algo":"sha256"}`.
     - `transfers` is `["lfs-standalone-file","basic","ssh"]`; with
       `git-xet` registered, `"xet"` is added.
     - The order changes from run to run. It is a Go map iteration
       (`tq/manifest.go` `getAdapterNames`), so servers must not depend on
       it.
     - `git-xet` is offered for **downloads too**, because
       `git xet install` sets no `direction`.
   - **Upload without `git-xet`:** `"transfer":"basic"`.
     - `upload.href` is a presigned S3 PUT (`X-Amz-Expires=900`) with no
       header.
     - `verify.href` is `{repo}.git/info/lfs/objects/verify`.
     - Objects carry `"authenticated": true`. There is no `expires_in` and
       no top-level `hash_algo`.
     - Fixtures: `hf/01-…`, `hf/03-…`.
   - **Upload with `git-xet`:** `"transfer":"xet"`.
     - `upload.href` is
       `https://huggingface.co/api/models/{ns}/{repo}/xet-write-token/refs%2Fheads%2Fmain`:
       the Hub token route, with the URL-encoded ref as the revision.
     - `upload.header` holds `X-Xet-Cas-Url`, `X-Xet-Token-Expiration`
       (unix seconds, 900 s ahead), `X-Xet-Access-Token` and
       `X-Xet-Session-Id`.
     - The `verify` action is still returned, and `git-lfs` still calls it
       after `git-xet` finishes (`200 OK`).
     - `git-xet` then sends, all with the batch's CAS token:
       - `GET {cas}/v1/chunks/default/{hash}` → 404
       - `POST {cas}/v1/xorbs/default/{hash}`
       - `POST {cas}/shards` → `{"result":1,"shard_hash":…}`
     - The batch token was valid for 15 minutes, so no refresh call was
       made.
     - Fixtures: `hf/02-…`, `hf/04-…`.
   - **The HF refresh route accepts only `Bearer`-scheme credentials** (observed, probed
     directly on the `href` above):
     - no Authorization → 401
     - `Basic` with the token as password → 401
     - a foreign `Bearer` → 401
     - the user's `Bearer` HF token → 200 `{accessToken, casUrl, exp}`,
       plus the `X-Xet-*` headers
   - **Download, with or without `git-xet`:** `"transfer":"basic"` even
     when `xet` is offered.
     - `download.href` is
       `https://us.aws.cdn.hf.co/xet-bridge-us/{repo-id}/{64-hex}?…`, signed
       by CloudFront (`Expires`, `Policy`, `Signature`, `Key-Pair-Id`) and
       carrying `user_id`, `X-Xet-Cas-Uid`, `xip`, `response-content-*` and
       `Hash-Algorithm=SHA256`.
     - There is no `header` map and **no `X-Xet-Hash`**. The 64-hex path
       segment differs from the oid; it is probably the Xet file hash
       (inferred).
     - Fixtures: `hf/05-…` to `hf/07-…`.
   - The first batch call carries no credential and gets a 401 with two
     `LFS-Authenticate` headers (`Basic realm=…`, `Bearer realm=…`).
     `git-lfs` retries with Basic credentials from the credential helper.
2. **`git-lfs` keys credentials by the `lfs.url` host; `git-xet` has its
   own refresh-credential lookup, configured separately.**
   - **`git-lfs` keys credentials by the `lfs.url` host** (observed, both
     versions, HTTPS and SSH remotes; source `lfsapi/auth.go:199-238` in
     3.7.1, identical logic at `:186` in 3.5.1).
     - It asks the credential helper for the `lfs.url` host. Git-remote
       credentials are used only when the remote has the same scheme and
       host as `lfs.url`.
     - The stub only ever received the stub token
       (`g2`/`s1`/`s2` stub logs).
     - 3.7 adds credential-helper `capability[]=authtype` and `state[]`
       (`creds/creds.go`), but not a different host choice (source).
   - **`git-xet` resolves the credential for its refresh `href` with its
     own lookup** (source, `git_xet/src/auth.rs`). The one switch that
     applies to it is `lfs.<endpoint derived from the remote>.access`; with
     `none`, it calls the `href` without a credential (question 3).
   - **The lookup runs when each upload starts** (source
     `xet_agent.rs:82` → `token_refresher.rs:36`; observed), even if no
     refresh will be needed. Without the `access` key and with no stored
     credential, the push stopped before `git-xet` contacted the `href`
     (`g6`).
   - Two smaller remote shapes `git-xet` can't handle (source):
     - **`file://` and local-path remotes:**
       `to_derived_http_scheme` rejects them (`git_url.rs:106-118`), and
       that runs before the `access` check.
     - **`.lfsconfig`:** `git-xet` reads only git config
       (`git2::Repository::config`, `git_repo.rs:116`), not `.lfsconfig`.
       So the `access` key below must be in the clone's git config.
3. **Yes. A signed capability `href` with no Authorization satisfies
   `git-xet`'s refresh, when `access=none` makes `git-xet` skip its
   credential lookup.**
   - **Observed:** with `lfs.https://gitea:3000/alice/r5.git/info/lfs.access=none`
     set in the clone's git config:
     - `git-xet` called the `href` with **no Authorization header**
       (`auth_class: null`).
     - It parsed `{accessToken, exp, casUrl}`.
     - It uploaded and registered the shard, and the push succeeded (`g3`).
   - `git-lfs` is unaffected, because it keys `access` by the `lfs.url`
     endpoint. The batch still used the openxet token (`g3`).
   - **When the refresh fires** (source `utils/src/auth.rs:11,121`):
     `git-xet` refreshes when `exp <= now + 30 s`. The stub's 5-second batch
     token forced an immediate refresh. With a longer CAS token, the `href`
     is not called at all during short pushes (as on huggingface.co), but
     the credential lookup of question 2 still happens.
   - **Consequence:** ADR 0005 item 10 is necessary but not sufficient.
     - Without the `access=none` key, `git-xet` runs its own lookup
       (question 2) before calling the `href` (`g6`).
     - openxet's policy therefore has two parts: capability routes never
       authenticate by header, and `openxet setup git` configures access
       explicitly by writing the key.
4. **`git-lfs-authenticate` returns JSON. `git-xet` needs `href` and
   `expires_in`. `git-lfs-transfer` can't negotiate `xet`.**
   - **Format:**
     - Invocation:
       `ssh [user@]host git-lfs-authenticate <repo-path> <upload|download>`.
     - Answer: one JSON object, `{"href": "<LFS endpoint>", "header":
       {"Authorization": "REDACTED"}, "expires_in": <seconds>,
       "expires_at": "<RFC 3339>"}`, where the Authorization value is
       `<scheme> <token>`.
     - The LFS spec makes `expires_in` and `expires_at` optional
       (`docs/api/authentication.md:9-29`), and its example has no `href`.
     - `git-lfs` adds `header` to its HTTPS batch request and uses `href`
       as the endpoint.
   - **Gitea 1.24.7 answers**
     `{"header":{"Authorization":"Bearer REDACTED"},"href":"https://gitea:3000/alice/r1.git/info/lfs"}`,
     where the value is a JWT, with no expiry fields (observed, `ssh/gitea-git-lfs-authenticate-upload.json`).
   - **`git-xet` 0.2.1 requires `href` and `expires_in`.**
     - They are non-optional fields of `GitLFSAuthenticateResponse`
       (source `git_xet/src/auth/ssh.rs:24-29`).
     - Against Gitea, the upload fails with
       `TokenRefreshFailure("… missing field \`expires_in\` …")`
       (observed, `s1`).
     - `git-xet` calls `git-lfs-authenticate` with `upload` on **every**
       refresh, with no caching (`ssh.rs:30-33`).
   - **`git-lfs-transfer` cannot negotiate `xet`** (source and observed,
     both versions).
     - Over pure SSH, `SSHBatchClient.Batch` always sends `transfer=ssh`
       and hard-codes the adapter name `ssh` (`tq/ssh.go:47,60` in 3.7.1;
       `:47,62` in 3.5.1).
     - The SSH protocol carries a single `transfer` argument and has no
       offer list to choose from (`docs/proposals/ssh_adapter.md:133`).
     - With `git-xet` registered, the trace shows `packet 00 >
       transfer=ssh` and `tq: starting transfer adapter "ssh"`
       (`ssh/puressh-upload-lfs371.trace.txt`).
   - **To get `xet` when the LFS endpoint is SSH** (openxet-hosted repos
     over SSH; an HTTPS `lfs.url` never uses pure SSH), pure SSH must be
     off or fail.
     - `git-lfs` tries `git-lfs-transfer` first by default
       (`lfs.<url>.sshtransfer` defaults to `negotiate`), and falls back to
       `git-lfs-authenticate` plus the HTTPS batch only when that fails
       (`lfsapi/lfsapi.go:55-76`).
     - With `lfs.sshtransfer=never`, the push ran `git-lfs-authenticate`
       and sent an HTTPS batch offering `xet` (observed, `s5`:
       `ssh/sshtransfer-never-upload-lfs371.trace.txt` and
       `…gitea-flows.json`). Gitea's answer has no `transfer` field, which
       means `basic` per the LFS spec, and `git-lfs` started the `basic`
       adapter.

## Further findings

- **A server must not choose `xet` for downloads** (observed, `d2`).
  - `git-lfs` offers `xet` for downloads whenever `git-xet` is installed.
  - If the server picks it, `git-xet` answers `Operation not supported:
    custom transfer for download is not implemented yet` and the
    **pull fails**. `git-lfs` does not fall back to `basic`.
  - A server can't tell `git-xet` from another agent named `xet`, so
    every download must be answered with `basic` (as huggingface.co does).
  - An openxet download agent therefore needs its own transfer name
    (design §6.1).
- **Downloads work through `basic` with the right credential** (observed,
  `d1`). The stub got the openxet token on the batch, and the signed `basic`
  `href` was fetched with no Authorization.
- **`git-xet` uses the `href` exactly as given** (observed). Ours had a
  query string (`?exp=…&sig=…`), and `git-xet` sent it unchanged. A
  query-signed capability URL works.

## Implications for openxet

- **`openxet setup git`** writes these to the clone's **git config**. It
  can't use `.lfsconfig` for the second key, because `git-xet` doesn't read
  it.
  - `lfs.url`
  - `lfs.<endpoint-derived-from-the-remote>.access=none`, with which
    `git-xet` calls the capability `href` without a credential

  It also stores the openxet token in the credential helper under the
  openxet host, for `git-lfs`'s batch. Repos hosted on openxet and reached
  over SSH (§10.9) are different: their LFS endpoint is SSH, so pure SSH
  applies. There it sets `lfs.<remote>.sshtransfer=never` when `git-xet` is
  installed, and no `access` key, because the remote is openxet itself.
- **Batch responses:**
  - For uploads offering `xet`, answer `xet` with a signed capability
    `href`.
  - For every download, answer `basic`, with absolute `href`s.
  - The `basic` download action's `header` carries only `X-Xet-Hash`, for
    agents that read it (`git-lfs` sends it to the `href`, which ignores
    it). It carries no CAS token: Xet download agents use
    `{lfs-url}/xet-read-token`. That keeps the token out of `GIT_TRACE`
    output.
  - No action `header` or response ever carries the client's own credential (§5.9 req 6).
- **The capability `href` must:**
  - accept requests with no Authorization
  - ignore and never log any Authorization it receives
  - outlive the transfer session
- **The `git-lfs-authenticate` answer** includes `href`,
  `header.Authorization` and `expires_in` (and may add `expires_at`).
  Otherwise `git-xet` fails against openxet-hosted repos over SSH (§10.9).
- **`git-lfs-transfer`** (§10.1) is still worth serving for plain
  `git-lfs` users. It only applies when the LFS endpoint is SSH, which means
  openxet-hosted repos over SSH; an HTTPS `lfs.url` never uses it
  (`lfsapi/lfsapi.go:60`). Its cost is that it hides `xet` from `git-xet`
  users there unless they set `sshtransfer=never`.
- These are decided in ADR 0003, which amends ADR 0005 item 10.

## Upstream drift

On `xet-core` `main` at `f42483b` (2026-10-23), `git_xet` has moved to
`xet_client` / `xet_pkg` crate paths. The behaviours above are unchanged:

- `auth.rs` credential sourcing is unchanged.
- `token_refresher.rs` `new_git_token_refresher` still resolves the
  credential when each upload starts.
- `xet_agent.rs:70-72` still returns "custom transfer for download is not
  implemented yet".
- `auth/ssh.rs:29` still has `pub expires_in: u32`.

No existing `xet-core` issue covers `git-xet` downloads; a search for
"git-xet download" returned nothing.

## Method

- **Environment:**
  - colima profile `openxet-spike-s7` (2 CPU, 4 GiB), deleted afterwards.
  - Three containers on one Docker network:
    - Gitea 1.24.7 over HTTPS (throwaway CA), with OpenSSH
    - the stub (Python, HTTPS, same CA)
    - the client (`python:3.13-slim` plus git, OpenSSH client and
      mitmproxy 12.3.0)
- **Isolation:**
  - Every run had a fresh `HOME`, `GIT_CONFIG_NOSYSTEM=1` and
    `GIT_TERMINAL_PROMPT=0`.
  - `git-xet` was registered with the three repo-local keys
    `git xet install` writes (`app/install.rs:43-101`).
  - Credentials were in a repo-local `credential.helper store --file=…`.
  - The mitmproxy CA was trusted only through `GIT_SSL_CAINFO` and
    `SSL_CERT_FILE` in the container. On Linux, `git-xet`'s rustls
    verifier honours `SSL_CERT_FILE` (spike S2), so every `git-xet` request
    was captured.
- **Tracing:** `GIT_TRACE=1 GIT_CURL_VERBOSE=1 GIT_TRANSFER_TRACE=1
  GIT_TRACE_PACKET=1`.
- **Stub:**
  - It logged a classification of each Authorization header, never the
    value, by comparing it with the known throwaway test values.
  - It decoded only the non-secret claim names of JWTs.
- **huggingface.co:**
  - One private model repo `<user>/openxet-compat-s7-<id>`, deleted at the
    end.
  - The deletion was verified: `GET /api/models/…` → 404, and an
    authenticated listing for `openxet-compat-s7` was empty.
  - The Hub token was only in a mode-600 file and a mode-600 credential
    store inside the container, both removed after the runs.
- **macOS:** not captured, because `git-xet`'s rustls verifier ignores
  `SSL_CERT_FILE` there (S2). Credential selection is platform-independent
  code (inferred from the source cited above).
- **Redaction:** a scratch script produced the fixtures with the rules in
  `compat/fixtures/lfs/README.md`.
  - `compat/tests/test_fixture_redaction.py` passes on them.
  - A second scan for the exact secret values (raw and Basic-base64) and
    the account name found nothing.
