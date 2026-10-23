# S3 Hub API fixtures

Real exchanges between huggingface.co and the stock clients, captured on
2026-10-23 for spike S3. The method and findings are in
`docs/research/S3-hub-api-shapes.md`.

- Clients: `huggingface_hub` 2.2.0 and `hf_xet` 1.7.0 (the versions pinned
  in `compat/`), on Python 3.13 in a Linux container (`python:3.13-slim`,
  arm64).
- Capture: mitmproxy 12.3.0 as an HTTPS proxy. Its CA was trusted only
  inside the client's isolated environment.
- Every client run used `compat/conftest.py:build_hf_env`.
- The target was private, throwaway repos that were deleted afterwards.

## Layout

There is one file per HTTP exchange, named
`<step>--<seq>-<METHOD>-<route>.json`. `<step>` is the workflow step that
caused it, and `<seq>` is its order within that step. `index.json` lists
every exchange with its step, role, method, URL and status.

```json
{"step": "09-upload-folder", "seq": 2, "role": "hub" | "cas" | "cdn",
 "request":  {"method": "...", "url": "...", "http_version": "...",
              "headers": [["Name", "value"], ...], "body": {...}},
 "response": {"status": 200, "reason": "OK", "http_version": "...",
              "headers": [["Name", "value"], ...], "body": {...}}}
```

- `role`:
  - `hub` is `huggingface.co`
  - `cas` is `cas-server.xethub.hf.co`
  - `cdn` is `us.aws.cdn.hf.co`, where xorbs are fetched from
- Headers are kept as name/value pairs, in wire order and with wire casing,
  so duplicates and order stay visible.
  - Lowercase request headers come from `hf_xet`.
  - Capitalised ones come from `huggingface_hub`.
- `body.size` is the byte length on the wire, after decoding any
  `Content-Encoding`. The body itself is stored as one of:
  - `json`: the body was compact JSON. Re-serialising it with
    `separators=(",", ":")` and `ensure_ascii=False` gives back the exact
    bytes.
  - `text`: the exact text, used for everything else. For NDJSON there is
    also `ndjson`, with each line parsed.
  - `omitted`: binary bodies (xorbs, shards), and bodies over 8 MiB that
    the proxy streamed without recording.

## Steps

| Step | Client call |
|---|---|
| 01 | `huggingface_hub.login(token=...)` |
| 02 | `HfApi.whoami()` |
| 03 | `hf auth whoami` |
| 04-06 | `create_repo` (private model), then again without and with `exist_ok=True` |
| 07-08 | `create_repo` / `hf repos create` in a namespace the token cannot write (`huggingface`) |
| 09 | `upload_folder`: `README.md`, `config.json`, `sub/small.txt`, and a 200 MiB random `big.bin` |
| 10 | `create_commit`: delete `sub/small.txt`, add `extra.txt` |
| 11-13 | `create_branch`, the same branch again, `create_tag` |
| 14-21 | `list_repo_refs` (with and without PRs), `list_repo_tree` (plain, recursive+expand, missing subpath), `list_repo_files`, `get_paths_info`, `list_repo_commits` |
| 22-25 | `model_info` (plain, at a tag with `files_metadata=True`, missing repo, missing revision) |
| 26-29 | `hf_hub_download` (small file, `big.bin` over Xet, missing file), `snapshot_download` at a branch |
| 30-31 | `delete_tag`, `delete_branch` |
| 32-33 | `hf upload`, `hf download` |
| 34-35 | `model_info` and `hf_hub_download` of the private repo without a token |
| 40-43 | dataset: create, `upload_file`, `dataset_info`, `hf_hub_download` |
| 50-57 | bucket: create, info, `batch_bucket_files` add, tree, paths-info, download, delete a file, `list_buckets` |
| 90-93 | delete the bucket, the dataset and the model, then delete the model again (404) |

The CLI's version check to `pypi.org`, during step 03, is not a Hub
endpoint and was dropped.

## Redaction

Everything is byte-exact except these, replaced as listed:
- `Authorization`, `Cookie`, `Set-Cookie` and `X-Xet-Access-Token` header
  values, and the `accessToken` field of the Xet token bodies, become
  `REDACTED`. Header names are kept.
- In signed CDN URLs, the values of `Signature`, `Policy`, `Key-Pair-Id`
  and `xip` become `REDACTED`. Parameter names, `Expires` and the other
  parameters are kept, so the URL's shape stays visible.
- Account identity:
  - the user name and organisation names become `openxet-compat`,
    `openxet-compat-org-a`, `-b` and `-c`
  - the full names become `openxet compat` and `openxet compat org a`, `b`
    and `c`
  - user and organisation object IDs, in bodies and in the CDN `user_id`
    and `X-Xet-Cas-Uid` parameters, become `000…001`, `000…002`, `000…00a`,
    `000…00b` and `000…00c`
  - avatar file names become `openxet-compat-avatar.<ext>`

  Repo object IDs (`_id`), commit SHAs, xet hashes and request IDs are not
  personal and are kept.

`compat/tests/test_fixture_redaction.py` fails if a token or a signed-URL
secret appears anywhere under `compat/fixtures/`.
