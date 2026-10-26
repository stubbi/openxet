# Git LFS routes

The Git LFS endpoint of every repository, for `git-lfs` with `git-xet`
registered, and for Xet-native clients such as jj-xet. It follows the Git
LFS batch API
(<https://github.com/git-lfs/git-lfs/blob/main/docs/api/batch.md>), with
the decisions of [ADR 0003](../adr/0003-git-xet-credential-strategy.md)
and [ADR 0005](../adr/0005-xet-native-client-contract.md). This page is
written from the code in `crates/hub/src/lfs`; the tests are
`crates/hub/tests/lfs.rs`.

## The LFS URL

```
{hub.public_url}/{ns}/{name}.git/info/lfs
```

This is what `lfs.url` points at, and what `git-lfs` derives from a Git
remote on this server. The same routes answer without `.git`, and under
the typed prefixes `datasets/`, `spaces/` and `kernels/`, which see the
repository through that [label](../adr/0010-two-storage-primitives.md).
The unprefixed URL serves a repository whatever its labels.

## Credentials

Every route but the capability route takes the caller's OpenXet access
token, either as the password of HTTP Basic (the user name is ignored),
which is what git's credential helpers give `git-lfs`, or as
`Bearer <token>`. [Rules](config.md#rules) decide what it may do.

- No credential, or one the server doesn't know: `401` with
  `WWW-Authenticate` and `LFS-Authenticate` set to
  `Basic realm="openxet"`, so `git-lfs` asks its credential helpers.
- A repository the caller may not read answers exactly as one that
  doesn't exist: `404 repo_not_found`.
- A write the caller may not make on a repository it may read: `403
  forbidden`.

Errors are JSON, `{"error", "schema": "openxet.error.v1", "code",
"message", "hint"}`, with an `X-Error-Message` header and, where Hugging
Face defines one, `X-Error-Code`, as on the Hub API. No answer and no log line carries the caller's
credential. Every answer is `Cache-Control: no-store`.

## Routes

| Method and path | Credential | Does |
| --------------- | ---------- | ---- |
| `POST {lfs-url}/objects/batch` | read for downloads, write for uploads | The batch API |
| `POST {lfs-url}/objects/verify` | write | Whether an upload completed |
| `GET {lfs-url}/xet-read-token` | read | A CAS read token for the repository |
| `GET {lfs-url}/xet-write-token` | write | A CAS write token for the repository |
| `GET /v1/capabilities/{repo id}/write?exp=…&sub=…&sig=…` | the signed URL | Refreshes an upload's CAS token |

### `POST {lfs-url}/objects/batch`

Request and answer are `application/vnd.git-lfs+json`. A batch names at
most 1,000 objects (`git-lfs` sends 100); `hash_algo`, if sent, must be
`sha256`, else `409 unsupported_hash_algorithm`. An object is a SHA-256
and a size; the repository has it when a shard uploaded to it registered
a file with that SHA-256 and size (ADR 0005 item 7). An object whose oid
is not 64 lowercase hex characters, or whose size is negative, gets the
object error `422`.

**Uploads that offer `xet`** answer `"transfer": "xet"`. An object the
repository has gets no actions. Every other object gets:

- `authenticated: true`
- `actions.upload.href`: an absolute capability URL, see below
- `actions.upload.header`: `X-Xet-Cas-Url`, `X-Xet-Access-Token`,
  `X-Xet-Token-Expiration` (Unix seconds, as a string) and
  `X-Xet-Session-Id`, in exactly that casing. The token is a CAS write
  token for the repository living `hub.token_ttl`, never past the
  capability.
- `actions.upload.expires_in`: `hub.capability_ttl` in seconds
- `actions.verify.href`: `{lfs-url}/objects/verify`, with no header:
  `git-lfs` sends the credential it holds for the host.

All objects of one batch share one capability, token and session.

**Uploads that don't offer `xet`** (plain `git-lfs`) answer each object
the repository doesn't have with the object error `501` and a message
naming `git-xet` and `git xet install`. OpenXet stores LFS files through
Xet, and an `href` that would fail later is never handed out.

**Downloads** always answer `"transfer": "basic"`, whether `xet` is
offered or not: `git-xet` registers for downloads but can't do them (ADR
0003 decision 3). An object the repository has gets
`actions.download.href`, the file gateway URL
`{hub.public_url}/v1/transfer/files/{repo id}/{xet hash}`, and
`actions.download.header` holding only `X-Xet-Hash`. There is no
`authenticated` field, so `git-lfs` sends its credential for the host to
the gateway, and the answer carries no token of any kind. An object the
repository doesn't have, including one registered only to another
repository, gets the object error `404`.

**The first push.** `git-lfs` uploads before git pushes. An upload batch
to a repository that doesn't exist creates it, without a commit, when the
caller may create in the namespace, labelled as the URL it came through
(`model` for the unprefixed URL). A download batch, a token route or a
refused batch never creates one.

### `POST {lfs-url}/objects/verify`

Body `{"oid", "size"}`. `200 {}` when the repository has the object, else
`404 object_not_uploaded`, so an upload that never completed is not a
silent success.

### `GET {lfs-url}/xet-read-token`, `GET {lfs-url}/xet-write-token`

For clients that chunk and upload before they know any oid (ADR 0005).
The answer is the Hub token route's:

```json
{"accessToken": "REDACTED", "exp": 1792736900, "casUrl": "https://openxet.example.com"}
```

with the same values in the `X-Xet-Access-Token`, `X-Xet-Cas-Url` and
`X-Xet-Token-Expiration` headers. `exp` is Unix seconds, `hub.token_ttl`
ahead. The token is scoped to the repository and the operation, never to
objects. To refresh, repeat the request.

### `GET /v1/capabilities/{repo id}/write?exp=…&sub=…&sig=…`

The `href` of an upload action, which `git-xet` calls to refresh its CAS
token during a long upload. The signature is the whole authorization (ADR
0003 decision 2): the route needs no `Authorization` header, and one that
is sent is never used, logged or echoed, and never fails the request. The
log records only whether one was present.

The answer is the token routes' answer for the repository and pusher the
capability was issued to, with a token living `hub.token_ttl` but never
past the capability's `exp`. The `href` therefore outlives every token it
issues. A capability that is altered, presented for another repository,
operation or subject, or past its `exp`, is refused with `403
capability_refused`; push again for a fresh one. Capabilities are signed
with `transfer.key` ([config](config.md#keys-and-tokens-on-disk)), so any
node sharing the key answers one another node issued.

## Configuration

`hub.token_ttl` and `hub.capability_ttl` ([config](config.md#hub)) set
the two lifetimes. `capability_ttl` must be at least `token_ttl`.

## Client setup

With the repository hosted on this server, install `git-xet`, run
`git xet install`, and store the access token as git's password for the
host (`git credential approve`). With the Git repository hosted elsewhere and its LFS
objects here, set `lfs.url` to this server's LFS URL and, in each clone,
`lfs.<endpoint git-xet derives from the remote>.access=none`, so that
`git-xet` calls the capability `href` without a credential (ADR 0003
decision 4).
