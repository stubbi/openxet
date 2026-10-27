# Hub routes

<!-- description: The Hugging Face Hub API routes an OpenXet server answers for huggingface_hub and the hf CLI: repositories, uploads and commits, refs, listings, downloads, labels, and every error. -->

The Hub API routes that stock `huggingface_hub` and the `hf` command line
call, served at the root of the server's origin beside the
[CAS routes](routes.md). Their answers match huggingface.co's field for
field, as captured in [S3](../research/S3-hub-api-shapes.md)
(`compat/fixtures/hub/`). This page is written from the code in
`crates/hub/src`; the tests are `crates/hub/tests/` and the compatibility
suite's Hub leg, `compat/tests/test_hub.py`. The full schema of every
route, request and answer is in the [OpenAPI document](openapi.md).

The other routes on the same origin: [Git routes](git-routes.md) for
`git`, [Git LFS routes](lfs.md) for `git-lfs` and `git-xet`.

## Conventions

- **Origin.** Clients set `HF_ENDPOINT` to `hub.public_url`
  ([config](config.md#hub)), by default the server's own URL. Every URL
  the Hub answers with is built from it.
- **Types.** Each repository route is served under `/api/models/`,
  `/api/datasets/`, `/api/spaces/` and `/api/kernels/`. These are views of
  one repository through its [labels](../adr/0010-two-storage-primitives.md):
  a typed route serves a repository only if it carries the type's label
  (`model`, `dataset`, `space`, `kernel`). File downloads (`resolve`) are
  under the repository's URL, with no prefix for models and
  `/datasets/`, `/spaces/` or `/kernels/` for the other types.
- **Credentials.** `Authorization: Bearer <token>`, an OpenXet access
  token, the one `hf auth login --token` stores. The server's
  [rules](config.md#rules) decide what it may do, per repository: *read*,
  *write*, or *create* in a namespace. A request without a credential is
  `401`, unless an `anonymous` rule grants the read. A CAS token, which
  only the CAS routes take, is refused with `403 cas_credential`.
- **Revisions** (`{rev}`) are a branch, a tag or a full commit id. A
  branch name holding `/` is percent-encoded, as `huggingface_hub` sends
  it.
- **Listings** (`tree`, `commits`, repository lists) answer at most
  `limit` entries, and at most `hub.page_size` (1,000 by default), in a
  stable order. When more follow, `Link: <…?cursor=…>; rel="next"` names
  the next page, which `huggingface_hub` follows on its own.
- **Every answer** carries `X-Request-Id`, and every route whose answer
  depends on the caller's credential carries `Cache-Control: no-store`.

## Errors

A Hub error is JSON with huggingface.co's `error` field and OpenXet's
structured fields, plus the headers `huggingface_hub` reads to choose and
word its exception:

```text
HTTP/1.1 404 Not Found
X-Error-Code: RepoNotFound
X-Error-Message: Repository not found

{"error":"Repository not found","schema":"openxet.error.v1","code":"repo_not_found","message":"Repository not found","hint":"Check the repository id and the token's access: a repository the token may not read answers exactly like one that doesn't exist."}
```

`code` is stable, for scripts and agents; `hint` says what to do next.
`X-Error-Code` is set where huggingface.co defines one (`RepoNotFound`,
`RevisionNotFound`, `EntryNotFound`). A `5xx` never carries its cause:
give the operator the `X-Request-Id`.

These answers can come from any route:

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `401` | `missing_credential` | No credential, and the request needs one. `WWW-Authenticate: Bearer realm="openxet"`. |
| `401` | `invalid_credential` | The token is unknown, malformed or expired. Its message, `Invalid credentials in Authorization header`, is the one `huggingface_hub` reports as an authentication error. |
| `403` | `cas_credential` | A CAS token. Use an access token. |
| `403` | `forbidden` | The caller may read the repository but not do this. The message names the repository. |
| `404` | `repo_not_found` | The repository doesn't exist, or the caller may not read it. The answer above, byte for byte the same in both cases. |
| `404` | `wrong_label` | The caller may read the repository, but it lacks the label of the route's type. `X-Error-Code: RepoNotFound`; the message names the `openxet label add` command that adds the label. To a caller who may not read the repository, this is `repo_not_found`. |
| `404` | `revision_not_found` | `{rev}` is no branch or tag, nor a commit of the repository. `X-Error-Code: RevisionNotFound`. |
| `501` | `not_implemented` | A Hub endpoint OpenXet doesn't implement; the message names it. |
| `503` | `not_ready`, `unavailable`, `authorizer_unavailable` | The server is starting, the index or bucket failed, or the authorizer could not decide. `Retry-After: 1`; `huggingface_hub` retries. |
| `500` | `internal` | A server bug, logged under the `X-Request-Id`. |

Routes with a body also answer `400 bad_request` (not what the route
takes), `408 body_timeout` and `413 payload_too_large`.

## Routes

`{type}` is `models`, `datasets`, `spaces` or `kernels`; `{repo}` is
`{namespace}/{name}`.

<!-- table-caption: Hub API routes, their credential and what they do -->
| Method and path | Credential | Does |
| --------------- | ---------- | ---- |
| `GET /api/whoami-v2` | any | Who the token acts for |
| `POST /api/repos/create` | create | Create a repository |
| `DELETE /api/repos/delete` | write and create | Delete a repository |
| `GET /api/{type}` | read | List repositories with the type's label |
| `GET /api/{type}/{repo}`, `…/revision/{rev}` | read | Repository info at a revision |
| `POST /api/{type}/{repo}/preupload/{rev}` | write | How to upload each file |
| `GET /api/{type}/{repo}/xet-write-token/{rev}` | write | A CAS token for uploads |
| `GET /api/{type}/{repo}/xet-read-token/{rev}` | read | A CAS token for downloads |
| `POST /api/{type}/{repo}/commit/{rev}` | write | Commit to a branch |
| `GET /api/{type}/{repo}/refs` | read | Branches and tags |
| `POST`, `DELETE /api/{type}/{repo}/branch/{branch}` | write | Create or delete a branch |
| `POST`, `DELETE /api/{type}/{repo}/tag/{rev}` | write | Create or delete a tag |
| `GET /api/{type}/{repo}/tree/{rev}[/{path}]` | read | List a folder |
| `POST /api/{type}/{repo}/paths-info/{rev}` | read | The entries at some paths |
| `GET /api/{type}/{repo}/commits/{rev}` | read | The history |
| `GET`, `HEAD /{prefix}{repo}/resolve/{rev}/{path}` | read | Download a file |
| `GET`, `HEAD /v1/transfer/files/{repo id}/{xet hash}` | read | The file gateway |
| `POST /api/validate-yaml` | any | Check a model card before a commit |
| `GET /api/openxet/v1/repos/{repo}/labels` | read | A repository's labels |
| `PUT`, `DELETE /api/openxet/v1/repos/{repo}/labels/{label}` | write | Add or remove a label |

`{prefix}` is empty for models and `datasets/`, `spaces/` or `kernels/`
otherwise.

### `GET /api/whoami-v2`

The token's user in huggingface.co's shape: `name` and `fullname` are the
token's subject (or a static token's `name`), and `orgs` lists the
token's `orgs` claim. `auth.accessToken.role` is `fineGrained`: what the
token may do is decided per repository by the rules. `hf auth whoami`
and `hf auth login` call it.

### `POST /api/repos/create`

Body `{"name", "organization", "type", "private"}`, as `create_repo`
sends it; without `organization`, the namespace is the caller's own.
Creation is authorized on the namespace before the name is looked up, so
a caller who may not create there gets the same `403` whether or not the
name exists. The new repository gets the label of `type` (`model` if
none) and an initial commit on `main`, by `openxet <noreply@openxet.com>`,
holding the `.gitattributes` huggingface.co starts a repository with.
`200 {"id", "name", "url", "filesCopyPending"}`; `url` is the
repository's URL under its type's prefix.

Namespaces and names match `^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$`, hold no
`..`, and a name doesn't end in `.git`; otherwise `400 invalid_name`,
whose hint names the rule. Names are unique ignoring case and across
labels: a taken name is `409 name_taken`, whose `url` lets
`create_repo(exist_ok=True)` go on. The message names the existing
repository's labels, and the `openxet label add` command that adds the
one asked for, only to a caller who may read it. `private` is stored and
answered back; who may read a repository is decided by the rules alone.

### `DELETE /api/repos/delete`

Body `{"name", "organization", "type"}`. Needs write on the repository
and create in its namespace. The repository is gone at once; its data
leaves the bucket only through garbage collection. A missing repository
is `404 repo_not_found`, which `delete_repo(missing_ok=True)` treats as
done.

### `GET /api/{type}`

The repositories with the type's label that the caller may read, ordered
by `{namespace}/{name}` ignoring case. `author` narrows the list to one
namespace; `full`, `config`, `cardData` and `expand` are accepted and
answered with what OpenXet has. Any other filter or sort is `501`.

### `GET /api/{type}/{repo}` and `…/revision/{rev}`

The repository at `main`, or at `{rev}`: `id`, `sha` (the commit),
`siblings` (every file; with `?blobs=true` also each file's `blobId`,
`size` and, for an LFS file, `lfs`), `labels`, `private`, `createdAt`,
`lastModified`, and the fields huggingface.co sends that OpenXet has no
use for, with their empty values (`likes`, `downloads`, `tags`, …).

### `POST /api/{type}/{repo}/preupload/{rev}`

How to upload each file of a coming commit, in request order:
`uploadMode` is `lfs` (through Xet) for a file of at least
`hub.lfs_threshold` (1 MiB by default) or one the revision's
`.gitattributes` routes to LFS, and `regular` otherwise. `shouldIgnore`
is always `false`. `commitOid` is the revision's commit. Needs write, as
the commit it prepares does.

### `GET /api/{type}/{repo}/xet-write-token/{rev}` and `xet-read-token/{rev}`

A CAS token scoped to the repository and the operation:

```json
{"accessToken": "REDACTED", "exp": 1792736900, "casUrl": "https://openxet.example.com"}
```

with the same values in `X-Xet-Access-Token`, `X-Xet-Cas-Url` and
`X-Xet-Token-Expiration`. `exp` is Unix seconds, `hub.token_ttl` ahead
(15 minutes by default). `casUrl` is `hub.cas_url`. `{rev}` is accepted
and ignored: a token is for the repository, so a refresh still works
after another commit moved the branch. To refresh, `hf_xet` repeats the
request.

### `POST /api/{type}/{repo}/commit/{rev}`

A commit as `huggingface_hub` sends it: `application/x-ndjson`, one
`{"key", "value"}` object per line, a `header` first, then the
operations.

| `key` | `value` | Effect |
| ----- | ------- | ------ |
| `header` | `summary`, and optionally `description` and `parentCommit` | The commit message: the summary, a blank line, the description. |
| `file` | `path`, `content` (base64), `encoding: "base64"` | A regular file, stored as a Git blob. At most `hub.lfs_threshold` decoded. |
| `lfsFile` | `path`, `algo: "sha256"`, `oid`, `size` | A file uploaded through Xet, stored as a Git LFS pointer. |
| `deletedFile` | `path` | Removes a file. |
| `deletedFolder` | `path` | Removes a folder. |

The commit is a real Git commit on branch `{rev}`, built on its tip: its
author is the token's user, `{user} <{user}@users.noreply.openxet.com>`
(or the token's `email` claim), and its committer
`openxet <noreply@openxet.com>`. It moves the branch through the same
transaction as a `git push`, so a push and an upload never overwrite each
other. Two commits to one branch at once both land; the later one is
retried on the new tip, waiting up to `hub.commit_wait` for commits
already running.

An `lfsFile` must name a file uploaded to this repository, by its SHA-256
and size; a file uploaded only to another repository is refused with
`400 bad_request`, never referenced. The body is at most
`hub.max_commit_bytes` (64 MiB by default) and 25,000 operations.
`copyFile` is not supported and is `400`.

`200 {"success": true, "commitOid", "commitUrl"}`, where `commitUrl` is
`{hub.public_url}/{prefix}{repo}/commit/{commitOid}`.

| Status | `code` | Meaning |
| ------ | ------ | ------- |
| `400` | `invalid_write` | A bad path, or objects git refuses. |
| `412` | `precondition_failed` | `parentCommit` is not the branch's tip. |
| `409` | `ref_conflict` | The branch moved while the commit was written. |
| `501` | `not_implemented` | `create_pr=true`: pull requests are not implemented. |
| `503` | `busy` | The commit lost its branch to other writers on every attempt. `Retry-After: 1`; `huggingface_hub` retries. |

### Refs: `refs`, `branch` and `tag`

`GET …/refs` answers `{"branches", "tags", "converts"}`, each ref with
`name`, `ref` and `targetCommit`; `?include_prs=true` adds
`pullRequests`, the `refs/pr/*` refs. `POST …/branch/{branch}` creates a
branch at the body's `startingPoint`, by default `main`'s tip;
`POST …/tag/{rev}` tags `{rev}` with an annotated tag named by the body's
`tag`, with its `message`, by the caller. `DELETE` removes either; `main`
can't be deleted. A ref that exists already, or moved meanwhile, is
`409 ref_conflict`, which `create_branch(exist_ok=True)` treats as done;
a bad ref name is `400 invalid_write`. Every ref change goes through the
same transaction as a `git push`.

### Reads: `tree`, `paths-info` and `commits`

- `GET …/tree/{rev}[/{path}]` lists a folder, ordered by path; with
  `?recursive=true` everything under it, and with `?expand=true` each
  entry's `lastCommit` and `securityFileStatus`. Each file has `oid`,
  `path`, `size` and `type`, and an LFS file also `lfs` (`oid`, `size`,
  `pointerSize`) and its `xetHash`. A path that names nothing is
  `404 entry_not_found` with `X-Error-Code: EntryNotFound` and
  `X-Repo-Commit`.
- `POST …/paths-info/{rev}` answers the entries at the body's `paths`,
  in request order; a path that names nothing is left out.
- `GET …/commits/{rev}` lists the history, newest first: `id`, `title`,
  `message`, `date` and `authors`.

Paths are read inside the commit's tree only: `.`, `..`, a leading `/`
and `%2F` never reach outside it.

### `GET /{prefix}{repo}/resolve/{rev}/{path}`

A file at a revision, as `hf_hub_download`, `hf download` and plain HTTP
clients read it. `HEAD` answers the same without a body.

- **A file stored in Git** answers `200` inline (or `206` for a `Range`),
  with `ETag` set to its blob id, `X-Repo-Commit`, `Accept-Ranges`, and
  `Content-Security-Policy: default-src 'none'; sandbox` with
  `X-Content-Type-Options: nosniff`, so a browser never runs what it
  reads.
- **A Xet file** answers `302` to the file gateway, with the headers
  `huggingface_hub` reads: `X-Xet-Hash`, `X-Linked-Size`,
  `X-Linked-ETag` (its SHA-256, quoted), `X-Repo-Commit`, and a `Link`
  naming the `xet-read-token` route (`rel="xet-auth"`) and the file's
  CAS reconstruction (`rel="xet-reconstruction-info"`). `hf_xet` then
  downloads it from the CAS; a plain HTTP client follows the redirect.

A path that names no file, or a folder, is `404 entry_not_found`; an LFS
pointer whose file was never uploaded here is `404 lfs_object_missing`;
both carry `X-Error-Code: EntryNotFound`. A `Range` past the end of the
file is `416` with `Content-Range: bytes */{size}`.

### `GET /v1/transfer/files/{repo id}/{xet hash}`

The file gateway: a Xet file of a repository over plain HTTP, where
`resolve` redirects and where Git LFS `basic` downloads point. It takes
the caller's credential as `Bearer`, or as the password of HTTP Basic,
which is what `git-lfs` sends; a `401` offers `Basic` first. A file is
served only to readers of the repository whose uploads registered it.

It answers HTTP `Range` (`bytes=start-end`, `bytes=start-` and
`bytes=-length`; `416` past the end), `If-Range`, `HEAD`, `ETag` (the
file's SHA-256, quoted), `Accept-Ranges`, `X-Xet-Hash`, and
`Content-Disposition` from `?name=`. A download that sends no byte for
`hub.gateway_idle_timeout` is cut off. A `{xet hash}` the repository
never registered is `404 file_not_stored`. A read larger than an answer's
share of the [memory budget](config.md#memory) is `413 answer_too_large`;
ask by range.

### `POST /api/validate-yaml`

Every commit that adds a `README.md` posts it here first. OpenXet checks
the request's shape, not the card's metadata, and answers no errors and
no warnings.

### Labels: `/api/openxet/v1/repos/{repo}/labels`

OpenXet's own API, under `/api/openxet/v1/`. `GET` lists a repository's
labels; `PUT …/labels/{label}` adds one and `DELETE …/labels/{label}`
removes one. Each answers the labels after the change:

```json
{"schema": "openxet.labels.v1", "repo": "acme/weather", "labels": ["dataset", "model"]}
```

Adding a label the repository carries, or removing one it doesn't,
changes nothing. A label is a lowercase slug of `a` to `z`, `0` to `9`
and `-`, 1 to 64 bytes, and a repository carries at most 32: otherwise
`400 invalid_label`. Labels that changed concurrently are
`409 labels_changed`; retry. Each change is written to the bucket, then
the index, then an audit row naming who changed what. The
[`openxet label`](cli.md#openxet-label) command calls these routes.

### Hub endpoints OpenXet doesn't implement

Discussions and pull requests, likes, collections, a user's likes, model
card tags and agent harnesses answer `501 not_implemented`, whose message
names the endpoint, so a client fails with a clear error rather than a
misleading `404`. Any path no route serves is `404 not_found`.
