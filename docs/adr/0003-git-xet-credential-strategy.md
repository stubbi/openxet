# 0003: Serve `git-xet` with capability `href`s and repo-local client config

- Status: accepted
- Date: 2026-10-23
- Scope: `git`, `hub`, `auth`, `ssh`, `client`
- Amends: ADR 0005, item 10 (batch details)

## Context

Design §6.1 makes openxet the LFS store for Git repos hosted anywhere, through
`lfs.url`. Design §10.2 notes that `git-xet`'s credential sourcing assumes
the LFS host equals the Git host. Spike S7 tested this end to end with `git-lfs` 3.5.1 and 3.7.1 and `git-xet`
0.2.1. It used huggingface.co, Gitea 1.24.7 (HTTPS and SSH) and a stub
shaped on ADR 0005. The evidence is in `docs/research/S7-git-lfs-xet.md` and
`compat/fixtures/lfs/`. What was verified:

- `git-lfs` sends only the `lfs.url` host's credential to the batch API, on
  HTTPS and SSH remotes (observed).
- `git-xet`'s credential sourcing for the batch `href` assumes the LFS
  host equals the Git host, so with `lfs.url` on another host it can't be
  relied on (observed).
- The only client-side switch is the git config key
  `lfs.<endpoint derived from the remote>.access=none`. With it, `git-xet`
  sends no Authorization at all, and a signed capability `href` alone
  completes the refresh and the upload (observed). `git-xet` does not read
  `.lfsconfig`, so the key must be in the clone's git config.
- `git-xet` 0.2.1 registers for downloads but can't do them. A server that
  answers `xet` to a download batch breaks `git lfs pull` with no fallback
  (observed). huggingface.co always answers `basic` for downloads and sends
  no `X-Xet-Hash` (observed).
- `git-xet` needs `href` and `expires_in` in the `git-lfs-authenticate`
  answer. Gitea omits `expires_in`, and `git-xet` then fails (observed).
- `git-lfs-transfer` always uses the `ssh` adapter, so `xet` is never
  negotiated over pure SSH (source and observed).

## Decision

1. **Capability `href`s.** For an upload batch that offers `xet`, openxet
   answers `xet`. Each action's `href` is a signed, absolute capability URL
   for one scope and one operation, as in ADR 0005 item 10. The `href`:
   - needs no Authorization
   - stays valid at least as long as the CAS token it returns, and for at
     least the transfer session
   - is tested with a query-string signature, which `git-xet` sends back
     unchanged
2. **Authorization headers on the `href`.** An Authorization header on a
   capability `href` is never used for authentication. It is never logged
   or echoed, and never fails the request. Audit and metrics record only
   that one was present.
3. **Downloads always answer `basic`.** This applies even when `xet` is
   offered, because the server can't tell `git-xet` 0.2.1 from a
   download-capable agent of the same name.
   - `href`s are absolute and served by the HTTP read gateway (§5.5).
   - The `basic` download action's `header` carries only `X-Xet-Hash`,
     which is not a secret. There is no CAS token, CAS URL or expiration.
     A download agent gets those from `GET {lfs-url}/xet-read-token`
     (ADR 0005 item 1).
     - `git-lfs` forwards `X-Xet-Hash` to the `href`, which ignores it.
     - Keeping the CAS token out of the action keeps it out of `GIT_TRACE`
       output and out of reach of any off-host redirect of the download
       `href`.
   - openxet's `verify` action never echoes the client's credential.
   - openxet's own download agent (design §10.8) registers under its own
     transfer name, not `xet`. The server chooses that name when it is
     offered.
   - This replaces the download half of ADR 0005 item 10.
4. **`openxet setup git`** writes repo-local git config, never global, and
   prints exactly what it changed.
   - **Repo hosted elsewhere (§6.1):**
     - `lfs.url`
     - `lfs.<endpoint derived from the Git remote, as git-xet derives it>.access=none`
     - the openxet access token in the git credential helper, under the
       openxet host only
   - **Repo hosted on openxet over SSH (§10.9), with `git-xet` installed:**
     `lfs.<ssh remote url>.sshtransfer=never`.
     - Pure SSH is only attempted when the LFS endpoint is itself SSH, so
       this mode is the only one affected.
     - No `access` key is set here. `git-xet`'s `git-lfs-authenticate`
       call then reaches openxet itself, which is the correct host.

   It is idempotent and supports `--dry-run` and `--json`. Docs state that
   each clone needs it, because `.lfsconfig` can carry `lfs.url` but not the
   `access` key `git-xet` reads.
5. **`git-lfs-authenticate`** (design §10.1) returns
   `{"href", "header": {"Authorization"}, "expires_in"}`, and may add
   `expires_at`. All three are required, because `git-xet` 0.2.1 fails
   without `href` or `expires_in`.
6. **`git-lfs-transfer`** stays in v1 for plain `git-lfs`. It can't carry
   `xet`, so `git-xet` users of openxet-hosted repos over SSH reach `xet`
   through `git-lfs-authenticate` plus the HTTPS batch (decision 4).
7. **Upstream.** We contribute to `huggingface/xet-core`:
   - credential sourcing from the LFS endpoint, with a lazy lookup and an
     optional `expires_in`
   - download support, with `direction=upload` until it lands

   When an updated `git-xet` ships, decision 4 keeps working, and the
   `access` key becomes unnecessary for that version.

## Consequences

- **Users of `openxet setup git`** need no credential for the refresh and
  see no password prompt. Clones configured by hand still work, and
  decision 2 applies to any Authorization header the `href` receives.
- **Batch `href`s are bearer capabilities.** Their lifetime, scope (one
  repo, one operation) and signing key rotation are security properties.
  Leaking one gives that scope's CAS token until it expires.
- **`git-xet` users get no Xet downloads.** They download whole files over
  `basic` until our agent or upstream download support exists. This is the
  same as huggingface.co today.
- **Tests:**
  - Compatibility suite, M1/M2 (decisions 1, 3, 4): a Gitea-backed leg
    that pushes with `git-xet` and pulls with `git-lfs` through
    `openxet setup git`, asserting that the refresh succeeds with no
    Authorization header.
  - Same leg, without the setup (decision 2): asserts the push still
    succeeds and no Authorization value appears in openxet's logs.
  - Unit test: download batches never answer `xet`.
  - Contract test (decision 5): the `git-lfs-authenticate` answer
    deserialises into `git-xet`'s `GitLFSAuthenticateResponse` shape.

### jj-xet compatibility

Checked read-only against the jj-xet working tree at `~/repos/git/jj-xet`
on 2026-10-23. It has no commits yet, so the line numbers are for the
working tree.

- **The client is compatible with decision 3 as is.**
  - jj-xet sends a download batch offering `["xet","basic"]`
    (`src/remote.rs:297-298`).
  - Its response type has only `objects`; it never reads `transfer`
    (`src/remote.rs:342-345`).
  - It reads only `actions.download.header["X-Xet-Hash"]`
    (`src/remote.rs:321`, `:363-369`).
  - It never fetches the download `href`. Its CAS read token comes from
    `{lfs-url}/xet-read-token` (`src/remote.rs:64-69`, used for
    `Access::Read` in `src/xet.rs:421`).
- **Its mock diverges, but no test depends on the difference.** The
  `tests/openxet.rs` mock still answers `"transfer":"xet"`
  (`tests/openxet.rs:147`) and uses a token-route `href`
  (`{LFS_PATH}/xet-read-token?cap=x`, `tests/openxet.rs:135`). To match
  the contract, change the mock to `"transfer":"basic"` and an object URL
  `href`; the client needs no change.

## Alternatives considered

- **Authenticate the `href` with the user's openxet token.** Rejected:
  `git-xet` sources its credential from the Git remote's host, not from
  the openxet host.
- **Accept other hosts' credentials and map them to an identity.**
  Rejected: it makes openxet a consumer of GitHub/GitLab/Gitea secrets,
  which conflicts with "openxet never owns users" (AGENTS.md).
- **Answer `xet` for downloads, as ADR 0005 item 10 implied.** Rejected:
  observed to break `git lfs pull` for every `git-xet` 0.2.1 user.
- **Wait for the upstream change alone.** Rejected: released `git-xet`
  versions need the server and setup measures to work with a separate LFS
  host. Those measures work today and stay correct after the change.
- **Disable `git-lfs-transfer` to force the HTTPS path.** Rejected: plain
  `git-lfs` users on SSH-only networks lose LFS entirely. Setting
  `sshtransfer=never` per clone gives `git-xet` users of openxet-hosted
  repos `xet` without taking pure SSH away from others.
