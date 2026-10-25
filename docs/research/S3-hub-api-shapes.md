# S3: Hub API shapes that `huggingface_hub` requires

<!-- description: The Hub API requests and answers that huggingface_hub and the hf CLI need, captured from real traffic and read from the source. -->

- Date: 2026-10-23
- Clients: `huggingface_hub` 2.2.0 and `hf_xet` 1.7.0, the versions pinned
  in `compat/`.
- Source citations:
  - `file:line` references are to the installed `huggingface_hub` 2.2.0
    package (`compat/.venv/.../site-packages/huggingface_hub/`).
  - `xet-core` references are to tag `v1.7.0` (`4b48571`).
- Question (design §11): what are the exact `whoami-v2`, `preupload`,
  `commit` and `tree` response shapes `huggingface_hub` requires? The
  spike also captured the rest of the §6 workflow list, plus buckets.
- Fixtures: `compat/fixtures/hub/`, 99 exchanges. The README there
  describes the format and the redaction.

Labels used below:
- **observed:** seen in the captured traffic.
- **source:** read in the client source at the cited line.
- **inferred:** our conclusion, not directly verified.

## Answers

1. **The upload path never asks the server whether to use Xet** (source).
   - `preupload` only answers `regular` or `lfs` per file
     (`_commit_api.py:696`).
   - Every `lfs` file then goes through Xet whenever `hf_xet` is installed:
     `if is_xet_available():` (`_commit_api.py:396`). No Git LFS batch call
     is made; none appeared in the capture (observed).
   - So OpenXet routes a file to Xet by answering `"uploadMode": "lfs"`.
2. **The Xet token endpoints must return the token twice** (observed and
   source).
   - huggingface.co returns it both in the JSON body
     `{"casUrl","exp","accessToken"}` and in the headers `X-Xet-Cas-Url`,
     `X-Xet-Access-Token` and `X-Xet-Token-Expiration`.
   - `huggingface_hub` reads only the headers (`utils/_xet.py:95-97`). It
     raises `"Xet headers have not been correctly set by the server."`
     without them (`utils/_xet.py:181`).
   - `hf_xet` reads only the body: `CasJWTInfo`, camelCase,
     `xet_client/src/hub_client/types.rs:9-15` in `xet-core` v1.7.0.
   - Who fetches it differs by path (observed):
     - On upload, `hf_xet` fetches the write token itself: lowercase
       headers and `x-xet-session-id`, in
       `09-upload-folder--03-GET-models-xet-write-token.json`.
     - On `hf_hub_download`, Python fetches the read token first
       (`file_download.py:575`), then hands the refresh URL to `hf_xet`.
3. **`POST /api/validate-yaml` is on the upload path** (source and
   observed).
   - Any commit that adds `README.md` first posts the README to it
     (`hf_api.py:5124`, `hf_api.py:11469`).
   - The client calls `response.json()` before checking the status
     (`hf_api.py:11474`), and any non-2xx then fails the upload.
   - A `501` would break `upload_folder` of a model card, so OpenXet must
     answer `200 {"errors":[],"warnings":[]}` (or real validation).
4. **Repo delete is `DELETE /api/repos/delete` with a JSON body, not
   `POST`** (observed `91-…`, `92-…`; source `hf_api.py:4768`). Design §6
   lists `POST /api/repos/delete`; that line is wrong.
5. **A `409` from `create_repo` must carry `url`** (source and observed).
   - With `exist_ok=True`, the client swallows the 409 and then reads
     `r.json()["url"]` from the 409 response (`hf_api.py:4704`,
     `hf_api.py:4723`).
   - huggingface.co sends
     `{"error":"You already created this model repo: …","url":"…"}`.
   - Without `url`, `create_repo(exist_ok=True)` would raise `KeyError`
     (inferred from the code path).
6. **Errors are typed by the `X-Error-Code` header, not the status**
   (source `utils/_http.py:871-933`).
   - The codes seen are `RepoNotFound`, `RevisionNotFound` and
     `EntryNotFound` (observed).
   - Messages come from `X-Error-Message` and the body's `error` (string
     or list) or `errors[].message` (`utils/_http.py:1035-1078`).
   - A `401` on a repo URL is mapped to `RepositoryNotFoundError` unless
     `X-Error-Message` is `"Invalid credentials in Authorization header"`
     (`utils/_http.py:933-935`).
7. **`hf_xet` requests reconstruction ranges past the end of the file
   and expects `416`** (observed). This is CAS-side; S3 only recorded it.
   - For the 200 MiB file it sent three `GET /v2/reconstructions/{hash}`
     concurrently, with `Range: bytes=0-255999999`, `256000000-767999999`
     and `768000000-1023999999`.
   - The last two got `416` and a text body
     `request_id: …; (12) invalid range`, and the download succeeded.
   - OpenXet's CAS must answer `416` (not `200` or `500`) for a range
     starting at or past the end of the file.

## What was captured, and what was not

Captured (Linux client, every exchange intercepted including CAS and
CDN):
- the whole design §6 workflow list for a model repo
- a dataset round trip
- a bucket round trip
- the error cases: 401, 403, 404 ×3 kinds, 409 ×2, and 416

Not captured:
- **Xet CAS traffic on macOS.**
  - A first run on the macOS host failed the upload: `hf_xet` honours
    `HTTPS_PROXY` but verifies TLS against the system trust store, which
    ignores `SSL_CERT_FILE`, as in S1/S2.
  - So it rejected the proxy's certificate when it fetched
    `xet-write-token` from huggingface.co itself (observed: "Request
    middleware error … xet-write-token").
  - All captures in the fixtures come from the Linux run, where
    `SSL_CERT_FILE` is honoured.
- **Interactive `hf auth login`.** It reads the token from a TTY; the
  driver used `huggingface_hub.login(token=…)` instead, which makes the
  same `whoami-v2` call (`_login.py:429`).
- **Not exercised:**
  - `space` and `kernel` repos
  - `create_pr` and the `pullRequestUrl` commit field (source
    `_commit_api.py:1078`)
  - gated repos (`GatedRepo`)
  - `429` rate limits
  - pagination: no response was large enough to carry a `Link: rel="next"`
    header, which `utils/_pagination.py:51` follows
  - `super_squash_history`
  - `hf repos delete`
- **The legacy LFS upload path** (`post_lfs_batch_info`). It is only
  taken without `hf_xet` or for `BufferedIOBase` inputs
  (`_commit_api.py:396-410`), so the Git LFS batch API was not called.
  Spike S7 covers it.
- **A ≥ 1 GiB upload.** The brief asked for 200 MB; see lesson 1 below.

## Endpoints

Each section gives:
- the request as sent (observed)
- the response fields the client reads (source)
- the status codes it handles

Unless stated otherwise:
- Hub JSON responses are compact UTF-8 JSON with `Content-Type:
  application/json; charset=utf-8`.
- Requests carry `Authorization: Bearer <token>`.

### `GET /api/whoami-v2`

Fixtures: `01-login--01-…`, `02-…`, `03-whoami-cli--02-…`.

- Request: no body.
- Response (observed):
  - `type`, `id`, `name`, `fullname`, `isPro`, `avatarUrl`
  - `orgs[]`, each with `type`, `id`, `name`, `fullname`, `avatarUrl`
  - `auth`:
    - `type: "access_token"`
    - `accessToken`: `displayName`, `role` (`"fineGrained"`),
      `createdAt`, and `fineGrained` with `canReadGatedRepos`, `global[]`
      and `scoped[]`
    - each `scoped[]` entry has an `entity` (`_id`, `type`, `name`) and
      `permissions[]`
- Fields read:
  - `name`, required (`KeyError` if missing):
    - `_login.py:430`
    - `cli/auth.py:174`
    - namespace defaulting (`hf_api.py:7121`, `hf_api.py:8807`, and 19
      sites such as `hf_api.py:12022`)
  - `orgs[].name`, required by `hf auth whoami` (`cli/auth.py:172`). It
    iterates `info["orgs"]`, so the key must exist, even as `[]`.
  - `orgs`, optional elsewhere (`hf_api.py:8984`).
  - `auth.accessToken.role` and `auth.accessToken.displayName`, optional
    (`_login.py:432-438`). `displayName` names the stored token; without
    it, the token is stored as `oauth-<name>`.
  - `id`, optional (`_sandbox.py:1852`).
- Status: a `429` gets a dedicated message (`hf_api.py:2259`).

### `POST /api/repos/create`

Fixtures: `04-…` (200), `05-…`, `06-…`, `32-…--01` (409), `07-…`, `08-…`
(403), `40-…`.

- Request: JSON
  `{"name","organization","visibility"?,"type"?}`
  (`hf_api.py:4638`).
  - `organization` is the namespace.
  - `type` is omitted for models created through the Python API with no
    `repo_type` (`07-…`), and sent as `"model"` by `hf repos create` and
    `hf upload` (`08-…`, `32-…`).
  - `visibility` is `"private"` or absent.
- Response 200:
  `{"url","name","id","filesCopyPending"}`. Only `url` is read
  (`hf_api.py:4723`). `RepoUrl` parses the repo id from `url` relative to
  the client's endpoint, so `url` must be absolute and under OpenXet's
  public base URL (inferred from `RepoUrl`, `hf_api.py:507`).
- Errors (observed, source):
  - `409`:
    - `{"error":"You already created this model repo: <id>","url":"<repo url>"}`,
      plus `X-Error-Message` with the same text.
    - With `exist_ok`, `url` is read from the 409 response (see Answer 5).
    - A 409 whose text contains "another conflicting operation is in
      progress" is retried in a loop (`hf_api.py:4690`).
  - `403`:
    - `{"error":"You don't have the rights to create a model under the namespace \"huggingface\""}`,
      plus `X-Error-Message`.
    - With `exist_ok`, a 401, 402 or 403 falls back to `repo_info` to see
      whether the repo already exists (`hf_api.py:4707`).

### `DELETE /api/repos/delete`

Fixtures: `91-…`, `92-…` (200), `93-…` (404).

- Request: JSON `{"name","organization","type"}` (`hf_api.py:4768`).
- Response 200: `Content-Type: text/plain`, body `OK`. Nothing is read.
- 404:
  - `{"error":"Repository not found"}`, `X-Error-Code: RepoNotFound`
  - maps to `RepositoryNotFoundError`, which `missing_ok=True` swallows
    (`hf_api.py:4772`)

### `POST /api/validate-yaml`

Fixture: `09-upload-folder--01-…`.

- Request: JSON `{"content": <README text>, "repoType": "model"}`.
- Response 200:
  `{"errors":[],"warnings":[{"message","help"}]}`.
  - `warnings[].message` is shown as a Python warning
    (`hf_api.py:11475`).
  - On `400`, `errors[].message` is raised as `ValueError`
    (`hf_api.py:11483`).
  - The body must be JSON for every status (`hf_api.py:11474`).

### `POST /api/{type}s/{id}/preupload/{rev}`

Fixtures: `09-…--02-…`, `10-…--01-…`, `32-…--02-…`, `41-…--01-…`.

- Request: JSON `{"files":[{"path","sample","size"}]}`
  (`_commit_api.py:753`).
  - `sample` is base64 of the first 512 bytes (`lfs.py:114`; observed:
    684 base64 characters for `big.bin`).
  - The client also sends `gitIgnore` when the commit adds a
    `.gitignore` (`_commit_api.py:760`, not exercised).
  - `?create_pr=1` is added for PR commits.
- Response 200 (observed):
  `{"files":[{"path","uploadMode","shouldIgnore"}],"commitOid"}`.
- Fields read:
  - `files`, required list (`_commit_api.py:688`)
  - `files[].path` and `files[].uploadMode`, required, with `uploadMode`
    `"lfs"` or `"regular"` (`_commit_api.py:696`, `771`)
  - `files[].shouldIgnore`, required (`KeyError`, `_commit_api.py:772`)
  - `files[].oid`, optional (`_commit_api.py:773`); huggingface.co did not
    send it here
  - `commitOid` is **not read**
  - Zero-byte files are forced to `regular` client-side
    (`_commit_api.py:784`)
- Observed decisions: the 20-, 21-, 11-, 6- and 8-byte files were all
  `regular`, and the 200 MiB `big.bin` was `lfs`.

### `GET /api/{type}s/{id}/xet-write-token/{rev}` and `xet-read-token/{rev}`

Fixtures: `09-…--03-…` (write), `27-…--02-…` (read); buckets below.

- URL built at `utils/_xet.py:244-251`: the revision is appended for every
  type except buckets. For PR commits it is the literal `/None`, plus
  `?create_pr=1`.
- The read-token route is not built by the client for downloads. It comes
  from the resolve response's `Link: <…>; rel="xet-auth"`
  (`utils/_xet.py:69`). The read token was requested at the **commit
  SHA**, not the branch name (observed).
- Response 200 (observed):
  - body `{"casUrl":"https://cas-server.xethub.hf.co","exp":<epoch>,"accessToken":"<JWT>"}`
  - headers `X-Xet-Cas-Url`, `X-Xet-Access-Token` and
    `X-Xet-Token-Expiration` with the same three values
- Both are required; see Answer 2.

### `POST /api/{type}s/{id}/commit/{rev}`

Fixtures: `09-…--14-…`, `10-…--02-…`, `32-…--03-…`, `41-…--02-…`.

- Request: `Content-Type: application/x-ndjson` (`_commit_api.py:1055`).
  - Each line is a JSON object serialised with Python's default
    separators (`", "`, `": "`), so parsers must not assume compact
    lines.
  - Lines, in order:
    - `{"key":"header","value":{"summary","description"}}`
      (`_commit_api.py:932`). It also carries `parentCommit` when the
      caller passes one (not exercised).
    - `{"key":"file","value":{"content":<base64>,"path","encoding":"base64"}}`
      for `regular` files (`_commit_api.py:947`).
    - `{"key":"lfsFile","value":{"path","algo":"sha256","oid":<sha256 hex>,"size"}}`
      for `lfs` files (`_commit_api.py:957`). There is **no Xet hash in the
      commit**: the server must map sha256 to the Xet file hash from the
      shard `hf_xet` uploaded before the commit.
    - `{"key":"deletedFile","value":{"path"}}`, or `deletedFolder`
      (`_commit_api.py:968`).
- Response 200:
  `{"success":true,"commitOid","commitUrl","hookOutput":""}`.
  - Read: `commitUrl` and `commitOid`, required
    (`_commit_api.py:1074`, `1077`).
  - `pullRequestUrl` is required only with `create_pr`
    (`_commit_api.py:1078`).
  - Errors go through `hf_raise_for_status(..., endpoint_name="commit")`
    (`_commit_api.py:1068`), so a `400` reads "Bad request for commit
    endpoint".

### `POST /api/{type}s/{id}/branch/{branch}`, `DELETE …/branch/{branch}`

Fixtures: `11-…`, `12-…` (409), `31-…`.

- Request (`hf_api.py:6889`, `hf_api.py:6958`):
  - create: JSON `{}`, plus `startingPoint` when a revision is given
  - delete: no body
- Response:
  - 200 `{"success":true}` for both; not read
  - 409 `{"error":"Reference refs/heads/s3-branch already exists. Use overwrite=true to force it."}`
    with `X-Error-Message`, raised as `HfHubHTTPError` unless `exist_ok`
    swallows it (`hf_api.py:6900`)

### `POST /api/{type}s/{id}/tag/{rev}`, `DELETE …/tag/{tag}`

Fixtures: `13-…`, `30-…`.

- Request:
  - create: JSON `{"tag","message"?}` posted to the **revision being
    tagged** (`hf_api.py:7025`, `7029`)
  - delete: `DELETE …/tag/{tag}` (`hf_api.py:7081`)
- Response: 200 `{}` for both; not read.

### `GET /api/{type}s/{id}/refs[?include_prs=1]`

Fixtures: `14-…`, `15-…`.

- Response:
  - `{"tags":[…],"branches":[…],"converts":[]}`, each ref
    `{"name","ref","targetCommit"}`
  - plus `"pullRequests":[]` when `include_prs=1`
- Read: `branches`, `converts` and `tags` are required lists
  (`hf_api.py:4112-4114`). `pullRequests` is required with
  `include_prs` (`hf_api.py:4115`). Per ref, `name`, `ref` and
  `targetCommit` (`hf_api.py:4109`).
- Observed: the tag's `targetCommit` (`f3d7a85…`) is the **annotated tag
  object**, not the commit it points to (`9afcd3e…`).

### `GET /api/{type}s/{id}/tree/{rev}[/{path}]?recursive=&expand=`

Fixtures: `16-…`, `17-…`, `18-…` (404), `19-…`, `29-…--02-…`.

- Request: the query is always sent, as `recursive=false&expand=false`
  etc. (`hf_api.py:3969`). `list_repo_files` uses `recursive=true`.
  `snapshot_download` lists at the resolved commit SHA.
- Response: a JSON array of entries.
  - Files: `{"type":"file","oid","size","path"}`.
  - Files with `lfs` also carry `"lfs":{"oid","size","pointerSize"}` and
    `"xetHash"`.
  - With `expand=true`, entries also carry:
    - `lastCommit {id,title,date}`
    - `securityFileStatus {status, protectAiScan, avScan, pickleImportScan, virusTotalScan}`
- Read:
  - `type` (`hf_api.py:3970`); anything other than `"file"` is a folder
  - files: `path`, `size` and `oid` required (`hf_api.py:692-694`)
  - `lfs.size`, `lfs.oid` and `lfs.pointerSize` (`hf_api.py:697`)
  - `xetHash`, optional (`hf_api.py:699`); `snapshot_download` caches it
    (`_snapshot_download.py:290`)
  - `lastCommit.id`, `.title` and `.date` (`hf_api.py:703`)
  - `securityFileStatus.status`, `.avScan` and `.pickleImportScan`,
    required when the object is present (`hf_api.py:708-713`)
  - folders: `path` and `oid` (`hf_api.py:742-743`)
- `oid` is the Git blob SHA-1. For LFS files it is the SHA-1 of the
  pointer file, and `size` is the real file size.
- 404 on a missing path:
  `{"error":"sub does not exist on \"s3-branch\""}`,
  `X-Error-Code: EntryNotFound`.
- The response has no `Link` header here; pagination follows
  `Link: rel="next"` when present (`utils/_pagination.py:51`).

### `POST /api/{type}s/{id}/paths-info/{rev}`

Fixture: `20-…`.

- Request: **form-encoded**, not JSON:
  `paths=README.md&paths=big.bin&paths=sub&paths=missing.txt&expand=true`
  (`hf_api.py:4270-4273`).
- Response: the same entry shape as `tree`. Missing paths are silently
  omitted (observed: `sub`, which no longer existed, and `missing.txt`
  were absent; status 200). Read like `tree` (`hf_api.py:4279`).

### `GET /api/{type}s/{id}/commits/{rev}`

Fixture: `21-…`.

- Response: a JSON array, newest first, with header `X-Total-Count: 3`.
  Each item is `{"id","title","message","authors":[{"user","avatar"}],"date"}`.
- Read: `id`, `authors[].user`, `date`, `title` and `message`, all
  required (`hf_api.py:4191-4195`). `formatted.title` and
  `formatted.message` are optional and only requested with
  `?expand[]=formatted` (`hf_api.py:4196`, `4202`).
- The repo's first commit is a server-generated `"initial commit"` that
  adds `.gitattributes` (observed). It is created by `repos/create`.

### `GET /api/{type}s/{id}[/revision/{rev}][?blobs=true]`

Fixtures: `22-…`, `23-…`, `24-…` (404), `25-…` (404), `29-…--01-…`, `42-…`.

- Response (model):
  - `_id`, `id`, `private`, `tags`, `downloads`, `likes`, `modelId`,
    `author`, `sha`, `lastModified`, `gated`, `disabled`, `config`
  - `siblings[]`: `{"rfilename"}`, or with `blobs=true`
    `{"rfilename","blobId","size","lfs"?:{"sha256","size","pointerSize"}}`
  - `spaces`, `createdAt`, `usedStorage`
  - Datasets have the same core without `modelId`, `config` and `spaces`.
- Read:
  - `id` is the only required field (`hf_api.py:894`)
  - `sha` is required in practice by `snapshot_download`, which asserts
    it (`_snapshot_download.py:280-281`)
  - `siblings[].rfilename` is required (`hf_api.py:951`); `size`,
    `blobId` and `lfs.*` are optional (`hf_api.py:952-958`)
  - Everything else is optional (`hf_api.py:895-984`).
  - Note that the **sibling** LFS key is `sha256`, while the **tree** LFS
    key is `oid` (observed and source).
- 404s:
  - missing repo: `{"error":"Repository not found"}`,
    `X-Error-Code: RepoNotFound`
  - bad revision: `{"error":"Invalid rev id: no-such-rev"}`,
    `X-Error-Code: RevisionNotFound`
- 401 without a token on a private repo:
  - `{"error":"Invalid username or password."}`
  - `X-Error-Message: Invalid username or password.`
  - `WWW-Authenticate: Bearer realm="Authentication required", charset="UTF-8"`
  - the client raises `RepositoryNotFoundError` (observed), on purpose,
    so as not to leak existence (`utils/_http.py:933-942`)

### `HEAD|GET /{[datasets/]}{id}/resolve/{rev}/{path}`

Fixtures: `26-…`, `27-…--01-…`, `28-…`, `29-…`, `33-…`, `35-…` (401),
`43-…`.

- Request: the HEAD carries `Accept-Encoding: identity`
  (`file_download.py:1666`). It follows redirects only while they stay on
  the same origin or a known Hub origin (`utils/_http.py:776-777`).
- Small (Git blob) file, 200 (observed):
  - `ETag: "<git blob sha1>"`, `X-Repo-Commit`, `Content-Length`
  - `Content-Type: text/plain; charset=utf-8`,
    `Content-Disposition: inline; filename*=UTF-8''<name>; filename="<name>";`
  - no `X-Xet-Hash`; the following GET returns the bytes
- Xet file, **302** (observed):
  - `Location`: an absolute signed CDN URL
  - `X-Repo-Commit`, `X-Linked-Size`
  - `X-Linked-ETag: "<sha256>"`
  - `X-Xet-Hash`
  - `Link: <…/api/models/{id}/xet-read-token/{commit sha}>; rel="xet-auth", <https://cas-server…/v1/reconstructions/{xet hash}>; rel="xet-reconstruction-info"`
- Read (`file_download.py:1676-1688`):
  - `X-Repo-Commit`
  - `X-Linked-Etag`, falling back to `ETag`
  - `Location`, falling back to the request URL
  - `X-Linked-Size`, falling back to `Content-Length` only when not a
    redirect
  - `X-Xet-Hash` and the `xet-auth` link (`utils/_xet.py:66-71`); the
    legacy `X-Xet-Refresh-Route` header is accepted instead of the link
  - A missing `X-Repo-Commit` or size is an error
    (`file_download.py:1791`, `1809`).
- 404 on a missing file:
  - `X-Error-Code: EntryNotFound`, `X-Error-Message: Entry not found`
  - **with `X-Repo-Commit`**, which the client reads to cache the
    miss (`file_download.py:1773`)

### Buckets

Fixtures: `50-…` to `57-…`, `90-…`.

- `POST /api/buckets/{ns}/{name}`:
  - request: JSON `{"visibility":"private"}`
  - response 200 `{"url","name","id"}`; `url` is read
    (`hf_api.py:13822`)
  - 409 is swallowed with `exist_ok` (`hf_api.py:13808`)
- `GET /api/buckets/{ns}/{name}`:
  - response: `_id`, `id`, `author`, `private`, `createdAt`,
    `updatedAt`, `size`, `totalFiles`, `repoType:"bucket"`, `cdnRegions`
  - `id`, `private`, `createdAt`, `size` and `totalFiles` are required
    (`_buckets.py:89-93`)
- `GET /api/buckets/{ns}`: an array of the same objects (`list_buckets`).
- `GET /api/buckets/{ns}/{name}/xet-write-token` and `xet-read-token`:
  - no revision segment (`utils/_xet.py:245`)
  - the same body and headers as repo tokens
  - fetched by `hf_xet` (observed)
- `POST /api/buckets/{ns}/{name}/batch`:
  - NDJSON, one operation per line:
    - `{"type":"addFile","path","xetHash","mtime","contentType"}`
    - `{"type":"deleteFile","path"}`
  - response `{"success":true,"processed","succeeded","failed":[]}`
  - `failed` is read on a 200 (partial failure) or a 422 (all failed)
    (`hf_api.py:14770-14771`)
- `GET /api/buckets/{ns}/{name}/tree?recursive=true`: an array of
  `{"type":"file","path","size","xetHash","mtime","mtimeNanos","uploadedAt","contentType"}`.
  `type`, `path`, `size` and `xetHash` are required
  (`_buckets.py:212-215`).
- `POST …/paths-info`: a **JSON** body `{"paths":[…]}` (unlike repo
  `paths-info`). It returns the same entry shape and omits missing paths
  (observed).
- `HEAD /buckets/{ns}/{name}/resolve/{path}`:
  - 302 with `X-Xet-Hash`, `X-Linked-Size`, `X-Linked-ETag` and the
    `xet-auth` link
  - no `X-Repo-Commit`
  - read at `hf_api.py:14818-14830`
- `DELETE /api/buckets/{ns}/{name}`: `204`, empty body.

### `GET /api/agent-harnesses`

Fixture: `03-whoami-cli--01-…`.

- The `hf` CLI fetches it at most once a day to detect AI agents
  (`utils/_detect_agent.py:197`).
- Any failure returns `None` and is ignored (`utils/_detect_agent.py:204`),
  so a `501` is safe.

### CAS and CDN (recorded for reference; not S3's scope)

- `GET /v1/chunks/default/{hash}` (global dedup query):
  - `404` with a text body `request_id: …; (1) not found` for new data
  - carries `Authorization: Bearer <Xet JWT>`
- `POST /v1/xorbs/default/{hash}`: about 64 MiB bodies; response
  `{"was_inserted":true}`.
- `POST /v2/shards`: a binary shard; response
  `Content-Type: application/x-ndjson` streaming progress, ending
  `{"type":"result"}`.
- `GET /v2/reconstructions/{hash}` with `Range`: see Answer 7.
- CDN xorb GETs carry no `Authorization`; the signed URL is the
  credential. CDN URLs carry `user_id`, `repo_id`, `X-Xet-Session-Id`,
  `Expires`, `Policy`, `Signature`, `Key-Pair-Id` and `Hash-Algorithm`.

## Status codes and error bodies the client handles

| Status | Body / headers seen | Client behaviour (source) |
|---|---|---|
| 3xx | `Location` | Never raised (`utils/_http.py:868`). Followed for on-Hub targets (`utils/_http.py:776`). |
| 400 | — | `BadRequestError`, naming the endpoint (`utils/_http.py:955`) |
| 401 | `{"error":"Invalid username or password."}`, `WWW-Authenticate: Bearer realm=…` | `RepositoryNotFoundError` on repo URLs (`utils/_http.py:933`) |
| 403 | `{"error":"You don't have the rights …"}`, `X-Error-Message` | `HfHubHTTPError` `"403 Forbidden: <X-Error-Message>."` (`utils/_http.py:961`) |
| 404 | `X-Error-Code: RepoNotFound` / `RevisionNotFound` / `EntryNotFound` | Typed errors (`utils/_http.py:880-933`) |
| 409 | `{"error":…,"url"?:…}`, `X-Error-Message` | Swallowed by `exist_ok`; retried on "conflicting operation" (`hf_api.py:4690`, `4704`) |
| 416 | text body (CAS) | `HfHubHTTPError` naming the range (`utils/_http.py:989`). `hf_xet` handles it itself for reconstructions. |
| 429 | `RateLimit`, `RateLimit-Policy` | Message built from the ratelimit headers (`utils/_http.py:969`) |

Every huggingface.co error body seen was JSON `{"error": "<message>"}`,
with the same text in `X-Error-Message`. The exceptions are HEAD
responses, which carry only the headers, and the CAS, which answers with
`text/plain`.

## Competitive-matrix lessons

**Lesson 1: `preupload` must route large files to Xet, and a ≥ 1 GiB
`hf upload` must use the Xet path.**
- In 2.2.0 the server's only lever is `"uploadMode":"lfs"`. The Xet
  choice is client-side (`_commit_api.py:396`), and no LFS batch call
  follows (source, observed).
- Observed for the 200 MiB file:
  - `uploadMode:"lfs"`
  - then `xet-write-token`
  - then 4 xorbs and a v2 shard on the CAS
  - then a commit with an `lfsFile` line
- Caveat: `big.bin` also matches `*.bin filter=lfs` in the
  server-generated `.gitattributes`. This capture cannot tell whether
  huggingface.co decided by size or by pattern. That decision is ours to
  make (design §6, "small files"); answering `lfs` above a size threshold
  is sufficient for the client.
- The ≥ 1 GiB case was not run here; the compat suite's lesson-1 test
  must prove it via `hf_xet` activity.

**Lesson 2: `whoami-v2` returns every field `huggingface_hub` reads,
including `orgs`.**
- Required: `name`, and `orgs` (a list, possibly empty) whose items have
  `name`.
- Used if present:
  - `auth.accessToken.displayName` and `role`
  - `id`
- Recommended: return the full observed shape, including `type`,
  `fullname`, `avatarUrl`, `isPro` and `auth.type`, for other clients
  such as `huggingface.js`.

**Lesson 3: `resolve` returns `X-Xet-Hash`, `X-Repo-Commit`,
`X-Linked-Size`, `X-Linked-Etag` and an `xet-auth` link, with absolute
redirect URLs.**
- Confirmed on huggingface.co for Xet files (observed `27-…--01-…`), and
  for buckets (without `X-Repo-Commit`).
- Two refinements:
  - Small files are served inline (200, no Xet headers), and their `ETag`
    is the Git blob SHA-1.
  - The 404 also carries `X-Repo-Commit`.
- The client stores `Location` verbatim (`file_download.py:1683`) and
  uses `urljoin` only to decide whether to follow it
  (`utils/_http.py:776`).
- `parse_xet_file_data_from_response` rewrites only `huggingface.co`
  prefixes in the `xet-auth` URL (`utils/_xet.py:75`).
- So absolute URLs on OpenXet's own base URL are the safe contract
  (inferred: we did not test a relative `Location`).

**Lesson 7: LFS batch responses use exactly `X-Xet-Cas-Url`,
`X-Xet-Access-Token`, `X-Xet-Token-Expiration` (a real epoch) and
absolute `href`s.**
- Not exercised: `huggingface_hub` with `hf_xet` never calls the LFS
  batch API (observed, source), so S7 owns it.
- What S3 confirms: the same three header names, with an epoch-seconds
  expiry (for example `X-Xet-Token-Expiration: 1792732550`), are what
  huggingface.co's Hub token endpoints return and what
  `huggingface_hub` parses (`utils/_xet.py:95-97`). So one token
  implementation can serve both the Hub routes and the LFS batch `action`
  headers (inferred).

**Lesson 12: repo creation is authorized per namespace; scoped tokens
can't create elsewhere, and unauthorized requests get a clean 403 that
`hf` renders well.**
- Observed: a fine-grained token scoped to its own user got `403` when
  creating in `huggingface`. Body:
  `{"error":"You don't have the rights to create a model under the namespace \"huggingface\""}`,
  with the same `X-Error-Message`.
- `hf repos create` rendered it as (observed stderr):
  ```
  Error: (Request ID: Root=…)

  403 Forbidden: You don't have the rights to create a model under the namespace "huggingface".
  Cannot access content at: https://huggingface.co/api/repos/create.
  Make sure your token has the correct permissions.
  ```
- The rendering comes from `utils/_http.py:961`. The useful text comes
  entirely from `X-Error-Message`, so OpenXet's 403 must set it, and
  should name the namespace and the missing permission.
- Watch out: with `exist_ok=True`, a 403 triggers a `repo_info` probe
  (`hf_api.py:4707`). If that probe succeeds, the client reports success.
  The authorizer must therefore keep read and create decisions
  consistent.

## Consequences for the Hub layer (inferred)

- Implement `POST /api/validate-yaml`, at least as a JSON stub. Without
  it, uploads of model cards fail.
- Fix design §6: `DELETE /api/repos/delete`, not `POST`.
- Token routes return the token in both the body and the `X-Xet-*`
  headers.
- `409` on repo create includes `url`.
- `resolve` 404s include `X-Repo-Commit`.
- Tag refs point at annotated tag objects.
- Both commit NDJSON and bucket batch NDJSON use non-compact JSON lines.
  Parse each line as JSON, never by byte pattern.
- The CAS returns `416` for reconstruction ranges past the end of the
  file.

## Method

- **Proxy:** mitmproxy 12.3.0 (`uv tool`), with a scratch capture script
  that dumped every flow as JSONL. Its CA was trusted only through
  `SSL_CERT_FILE` and `REQUESTS_CA_BUNDLE` in the client's environment,
  never in a system store.
- **Environment:** the client ran in `python:3.13-slim` on a throwaway
  colima profile, with `uv sync --frozen` of `compat/`. Each run's
  environment was built by `compat/conftest.py:build_hf_env`, plus the
  proxy and CA variables.
- **Driver:** one scratch script drove all steps in order, through
  `huggingface_hub` and the `hf` CLI. Before each step it sent a marker
  request so the capture could be split by step.
- **Account:** the target was private repos under a personal account
  (`<user>/openxet-compat-s3-<id>`: model, dataset and bucket). All of
  them were deleted at the end, and their absence was verified through
  the API (404 on each, and an empty listing).
- **Redaction:** a scratch script produced the fixtures. Its rules are in
  `compat/fixtures/hub/README.md`.
  `compat/tests/test_fixture_redaction.py` scans every fixture and every research note. It fails
  on the raw capture (337 hits across 9 secret kinds) and passes on the
  redacted one.
