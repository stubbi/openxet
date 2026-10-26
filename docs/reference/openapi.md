# OpenAPI document

Every OpenXet server describes its whole HTTP API in one OpenAPI 3.1
document: the CAS, the Hub API, Git smart HTTP, Git LFS, the file gateway
and the probes. Generate a client from it, give it to an agent, or browse
it in any OpenAPI viewer.

| Where | What |
| ----- | ---- |
| `GET /openapi.json` | On any running server. No credential: it describes routes, never data. |
| `openxet openapi` | Prints the same bytes, without a server. |
| [`docs/reference/openapi.json`](openapi.json) | The copy for this release, checked in. |

```bash
openxet openapi > openapi.json
curl -s http://127.0.0.1:8080/openapi.json | jq '.paths | keys | length'
```

## What it holds

- **Every route the server serves**, with its methods. A route is described
  where it is built, so the document lists exactly what the server answers:
  a route can't be added without its description, and a test fails if a
  served route and the document disagree.
- **Parameters**: path, query and request headers such as `Range`, each
  with what it holds. A path parameter that ends a route, such as
  `{path}` in `/api/models/{ns}/{name}/tree/{rev}/{path}`, may hold `/`.
- **Credentials** as security schemes, never as values:

  | Scheme | Credential |
  | ------ | ---------- |
  | `casToken` | A static token or a CAS token, as `Authorization: Bearer`. The CAS routes. |
  | `accessToken` | An OpenXet access token, as `Authorization: Bearer`. The Hub routes. |
  | `accessTokenBasic` | The access token as the password of HTTP Basic, as git and `git-lfs` send it. Git, LFS and the file gateway. |
  | `signedUrl` | A URL the server signed: `sig` and the other query parameters it signs are the credential. CAS transfer URLs and LFS capability URLs. |

  An operation that also lists the empty requirement `{}` answers without
  a credential what the operator's `anonymous` [rules](config.md#rules)
  allow. The document holds no example values at all.
- **Every answer**, with its body's schema, and every status a route can
  give with the `code`s it carries. Every `4xx` and `5xx` is the error body
  `ErrorBody`, `{"schema":"openxet.error.v1","code","message","hint"}`;
  the Hub, LFS and gateway routes answer `HubErrorBody`, which adds
  huggingface.co's `error` field and the `X-Error-Code` and
  `X-Error-Message` headers; Git routes answer `GitErrorText`, the message
  and hint as `text/plain`, which git prints as `remote:` lines. The one
  exception is the `416` of a file read, which carries only
  `Content-Range`, as HTTP defines it.
- **Headers** every answer carries: `X-Request-Id`, and `Cache-Control:
  no-store` on every route whose answer depends on the caller's
  credential.

## Conventions

- **Tags** group the routes: `cas`, `hub`, `openxet` (the labels API),
  `git`, `lfs`, `gateway`, `health` and `meta` (this document).
- **Hub types.** Each repository route is listed once per Hub type,
  `/api/models/…`, `/api/datasets/…`, `/api/spaces/…` and
  `/api/kernels/…`, and each Git and LFS route once unprefixed and once
  under `/datasets/…`, `/spaces/…` and `/kernels/…`. They behave alike
  through the repository's [labels](../adr/0010-two-storage-primitives.md).
- **`operationId`** is the method and the path's words:
  `get_api_models_ns_name_tree_rev`.
- **`HEAD`.** A `GET` route also answers `HEAD` with the same status and
  headers and no body; it is not listed separately. The CAS transfer route
  is the exception: its `HEAD` is listed, and refused.
- **Shared answers.** An answer several operations give is described once
  under `components/responses`, named after the first operation that gives
  it, and referred to from each.
- **Version.** `info.version` is the server's version.

The CAS routes are explained in prose in [CAS routes](routes.md), and the
LFS routes in [Git LFS routes](lfs.md).

## Keeping it current

CI runs `scripts/check-openapi.sh`, which fails when the checked-in copy
differs from what `openxet openapi` prints, and the tests in
`crates/openxet/tests/openapi.rs`, which check that the document lists
every served route, is valid against the OpenAPI 3.1 JSON Schema, and is
what `GET /openapi.json` serves. After changing a route, regenerate the
copy:

```bash
cargo run -q -p openxet -- openapi > docs/reference/openapi.json
```
