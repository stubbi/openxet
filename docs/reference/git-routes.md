# Git routes

<!-- description: The Git smart HTTP routes an OpenXet server answers for every repository: paths, credentials, protocol versions, limits and every error. -->

Git's smart HTTP protocol
(<https://git-scm.com/docs/http-protocol>) for every repository on the
server, served by the `git` binary over refs and packs OpenXet keeps
([ADR 0004](../adr/0004-git-hosting-engine.md)). This page is written
from `crates/hub/src/githttp.rs` and the engine in `crates/git`; the tests
are `crates/hub/tests/githttp.rs`, `crates/git/tests/` and the
compatibility suite's Git leg, `compat/tests/test_git.py`. How to use it
is in [Git hosting](../guides/git-hosting.md).

## Paths

```text
{hub.public_url}/{ns}/{name}
{hub.public_url}/{ns}/{name}.git
{hub.public_url}/{type}/{ns}/{name}[.git]    type: datasets, spaces, kernels
```

`{hub.public_url}` is the Hub's URL ([config](config.md#hub)). Every route
below answers under each of these bases, with and without `.git`. The
unprefixed base serves a repository whatever its labels; a typed base
serves it only if it carries the type's label (`dataset`, `space`,
`kernel`), as on huggingface.co
([ADR 0010](../adr/0010-two-storage-primitives.md)). The Git LFS routes
live under the same bases, in [Git LFS routes](lfs.md).

| Method and path | Credential | Does |
| --------------- | ---------- | ---- |
| `GET {base}/info/refs?service=git-upload-pack` | read | The refs, for a clone or fetch |
| `GET {base}/info/refs?service=git-receive-pack` | write | The refs, for a push |
| `POST {base}/git-upload-pack` | read | A fetch, or a protocol v2 `ls-refs` |
| `POST {base}/git-receive-pack` | write | A push |

Any other path under a repository's base is `404`: the dumb HTTP
protocol, `info/refs` without `service`, is not served.

## Credentials

The caller's OpenXet access token, as the password of HTTP Basic (the user
name is ignored), which is what git's credential helpers send, or as
`Authorization: Bearer`. The server's [rules](config.md#rules) decide
what it may do per repository. A CAS token, which only the
[CAS routes](routes.md) take, is refused with `403`.

- No credential: `401` with two challenges, `WWW-Authenticate: Basic
  realm="openxet"` first, so git asks its credential helpers, then
  `Bearer realm="openxet"`. An [`anonymous` rule](config.md#rules) that
  grants read lets a clone through without one.
- An unknown, malformed or expired token: `401` with the message
  `Invalid credentials in Authorization header`.
- A repository the caller may not read, and one that doesn't exist,
  answer the same `404` on every route, typed bases included.
- A push to a repository the caller may read but not write: `403`.

**The first push creates the repository.** A push to a repository that
doesn't exist, by a caller whose rules allow `create` in its namespace
and `read` and `write` on the repository, is advertised as an empty
repository, and its `git-receive-pack` creates it before the refs move,
labelled `model` or the typed base's label. `info/refs` alone never
creates one, nor does a fetch.

## Requests and answers

- **Protocol versions.** `Git-Protocol: version=2` selects protocol v2;
  without it the server speaks v0. In v2 only the `ls-refs` and `fetch`
  commands are served; any other command is refused.
- **Request bodies** must have git's own types,
  `application/x-git-upload-pack-request` or
  `application/x-git-receive-pack-request`, or the answer is `415`, as
  `git http-backend` answers: a web page can't send those types
  cross-site without a preflight, so it can't use credentials a browser
  holds for the server. A body with `Content-Encoding: gzip`, which git
  sends for a large fetch request, is inflated as it arrives.
- **Answers** are git's pkt-lines with the service's type
  (`application/x-git-upload-pack-advertisement`, `…-result` and the
  `receive-pack` equivalents), and `Cache-Control: no-store`, since every
  answer depends on the caller's credential. Each carries `X-Request-Id`.
- **Fetching by object id.** The advertisement offers
  `allow-reachable-sha1-in-want`, and every `want` is checked against the
  refs: an object no branch or tag reaches is refused, even to a caller
  who knows its id, in v0 and v2 alike. The check walks at most
  `git.reachability.max_commits` commits for `max_ms`
  ([config](config.md#gitreachability)).
- **Pushes** move every ref of one push in one transaction, so a push of
  several refs is atomic. Each ref's old value must be the current one
  (else `fetch first`), ref names must be valid, `main` can't be deleted,
  and a branch must point at a commit. Force pushes are allowed. A thin
  pack's bases must be reachable from the refs
  ([config](config.md#gitthin_bases)); one that isn't is refused with a
  hint to fetch and push again with `git -c pack.window=0 push`. A pushed
  blob of 10 MiB or more that isn't a Git LFS pointer is named in the
  push output with a `git lfs track` suggestion, and refused above
  `git.max_plain_blob_bytes` when that is set
  ([config](config.md#git)).

## Errors

Errors are `text/plain`: the message, then a line with a hint, which git
prints as `remote:` lines. The message is also in `X-Error-Message`.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `401` | `missing_credential`, `invalid_credential` | See Credentials. |
| `403` | `cas_credential` | A CAS-only token. Use an access token. |
| `403` | `forbidden` | A push the caller may not make to a repository it may read. |
| `404` | `repo_not_found` | No such repository, or one the caller may not read. |
| `404` | `wrong_label` | A typed base whose label the repository lacks, to a caller who may read it. The hint names `openxet label add`. |
| `404` | `dumb_http_not_served` | `info/refs` without `service`. |
| `408` | `body_timeout` | The request body arrived too slowly ([config](config.md#server)). |
| `413` | `push_too_large` | A push whose `Content-Length` is over `git.max_push_bytes`. A longer streamed push is refused by git itself (`pack exceeds maximum allowed size`). |
| `415` | `unsupported_media_type` | Not git's request type. |
| `429` | `too_many_requests` | The reachability check is at its concurrency limit for the repository or the caller. `Retry-After: 1`. |
| `503` | `busy` | Too many requests in the repository at once. `Retry-After: 1`. |
| `503` | `not_ready`, `unavailable`, `authorizer_unavailable` | The server is starting, the index or bucket failed, or the authorizer could not decide. `Retry-After: 1`. |
| `500` | `internal` | A server bug, logged under the `X-Request-Id`. |

A refusal of one ref inside a successful `git-receive-pack` answer, such
as `fetch first` or `deletion of the current branch prohibited`, is part
of git's own report, with status `200`.

The full schema of each route is in the [OpenAPI document](openapi.md),
tag `git`.
