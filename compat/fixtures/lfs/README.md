# S7 Git LFS fixtures

Real exchanges between `git-lfs` / `git-xet` and huggingface.co, a local
Gitea, and an openxet-shaped stub, captured on 2026-10-23 for spike S7. The
method and findings are in `docs/research/S7-git-lfs-xet.md`.

- Clients: `git-lfs` 3.5.1 and 3.7.1 (release binaries, linux-arm64,
  checksums verified against the release `sha256sums.asc`) and `git-xet`
  0.2.1 (`git-xet-v0.2.1`, linux-aarch64), with git 2.47.3, in a Linux
  container (`python:3.13-slim`, arm64).
- `git-xet` was registered with repo-local config only, with the three keys
  `git xet install` writes (`lfs.customtransfer.xet.path=git-xet`,
  `.args=transfer`, `.concurrent=true`). HOME was a fresh directory per run.
- Capture: mitmproxy 12.3.0 as an HTTPS proxy inside the client container.
  Its CA was trusted only inside that environment (`GIT_SSL_CAINFO`,
  `SSL_CERT_FILE`).

## Layout

### `hf/`: huggingface.co

One file per HTTP exchange, named `<step>--<seq>-<METHOD>-<route>.json`, in
the S3 format (`compat/fixtures/hub/README.md`). `index.json` lists them.
Git smart-HTTP exchanges (`info/refs`, `git-*-pack`) are left out.

| Step | What ran |
|---|---|
| 01 / 03 | `git push` of one LFS file, plain `git-lfs` 3.7.1 / 3.5.1 |
| 02 / 04 | `git push` of one LFS file, `git-lfs` 3.7.1 / 3.5.1 with `git-xet` |
| 05 | `git lfs pull` of all four files, plain `git-lfs` 3.7.1 |
| 06 / 07 | `git lfs pull`, `git-lfs` 3.7.1 / 3.5.1 with `git-xet` |

- `role`:
  - `hub` is `huggingface.co`
  - `cas` is `cas-server.xethub.hf.co`
  - `s3` is the presigned LFS upload bucket
  - `cdn` is `us.aws.cdn.hf.co`
- Request and response bodies of objects, xorbs and shards are `omitted`.

### `gitea/`: third-party Git host with `lfs.url` on another host

Gitea 1.24.7 hosted the Git repos, over HTTPS and SSH. `lfs.url` pointed at
a stub (`https://stub:8443/{owner}/{repo}.git/info/lfs`) shaped on ADR 0005:

- `POST …/objects/batch` needs Basic auth with the "openxet token" as the
  password, and answers 401 with `LFS-Authenticate` otherwise.
  - Uploads answer `xet` when offered. The action `href` is a signed
    capability URL (`/cap/{scope}/{op}?exp=…&sig=…`) that needs no
    Authorization.
  - The batch `X-Xet-Token-Expiration` is now + 5 s, so `git-xet`
    refreshes at once (it refreshes 30 s before expiry).
  - Otherwise the stub answers `basic`.
- `GET /cap/…` returns `{accessToken, exp, casUrl}` and the `X-Xet-*`
  headers. It ignores any Authorization header.
- `/cas/…` is a fake CAS that accepts xorbs and shards.

Per scenario there are two files:

- `<scenario>.stub-log.jsonl`: one line per request the stub received.
  `auth_class` labels the Authorization header by the throwaway test value
  it matched, without storing it; `null` means none was sent.
- `<scenario>.stub-flows.json`: the same requests as seen by mitmproxy, with
  bodies, redacted. Chunk and xorb requests are left out.

| Scenario | Remote | `git-lfs` | `git-xet` | Variation |
|---|---|---|---|---|
| `g2-https-{371,351}-plain` | HTTPS | 3.7.1 / 3.5.1 | no | credential store has Gitea and stub entries |
| `g3-https-371-xet-accessnone` | HTTPS | 3.7.1 | yes | `lfs.https://gitea:3000/alice/r5.git/info/lfs.access=none` |
| `g6-https-371-xet-nogiteacred` | HTTPS | 3.7.1 | yes | no Gitea credential anywhere `git-xet` looks |
| `s1-ssh-{371,351}-xet-stuburl` | SSH | 3.7.1 / 3.5.1 | yes | Gitea's own `git-lfs-authenticate` |
| `s2-ssh-371-plain-stuburl` | SSH | 3.7.1 | no | |
| `d1-dl-371-xet` | HTTPS | 3.7.1 | yes | `git lfs pull`; stub answers `basic` |
| `d2-dl-371-xet-serverxet` | HTTPS | 3.7.1 | yes | `git lfs pull`; stub answers `xet` for the download |

### `ssh/`

- `gitea-git-lfs-authenticate-upload.json`: Gitea 1.24.7's answer to
  `ssh git@gitea git-lfs-authenticate alice/r1.git upload`.
- `puressh-upload-lfs371.trace.txt`: the `git-lfs-transfer` packet trace of
  a push with `git-xet` registered and no `lfs.url` (Gitea
  `LFS_ALLOW_PURE_SSH=true`).
- `sshtransfer-never-upload-lfs371.trace.txt` and
  `sshtransfer-never-upload-lfs371.gitea-flows.json`: the same push with
  `lfs.sshtransfer=never` (scenario `s5`). `git-lfs-authenticate` runs, then
  an HTTPS batch to Gitea offering `xet`, which Gitea answers with no
  `transfer` field (meaning `basic`).

## Redaction

- Every credential value is replaced by `REDACTED`; names stay. That covers:
  - `Authorization`, including action headers
  - `X-Xet-Access-Token` and `accessToken`
  - JWTs
  - the stub's fake CAS tokens
- In URLs, the values of these query parameters are replaced:
  - signatures: `X-Amz-Signature`, `X-Amz-Credential`,
    `X-Amz-Security-Token`, `Signature`, `Policy`, `Key-Pair-Id`, `sig`
  - client address: `xip`
  - account ids: `user_id`, `X-Xet-Cas-Uid`
  - in the SSH trace: `id` and `token`
- The account name is replaced by `openxet-compat`. The Gitea user `alice`,
  her password and the stub token were throwaway values in a local
  container.
- `body.size` and `Content-Length` describe the original bodies.
- `compat/tests/test_fixture_redaction.py` scans these files, including
  for base64 Basic credentials. A second scan
  for the exact token values (raw and as Basic base64) and the account name
  found nothing before commit.
