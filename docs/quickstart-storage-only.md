# Quickstart: storage only

Run OpenXet's Xet storage server (the CAS) on your machine, then move a
file through it with Hugging Face's own `hf_xet` and with the `openxet`
command line. This is the storage layer on its own, the way a platform
embeds it: files are addressed by their Xet hash, with no repositories
around them. For repositories with `hf upload`, `huggingface_hub` and
`git`, follow the [Hub quickstart](quickstart-hub.md) instead.

Every `bash` and `python` block on this page except the install block
runs in CI, in order, exactly as written
(`compat/tests/test_docs_quickstart.py`); CI provides what the install
block installs. Run them in one terminal, in an empty directory.

**Time to first upload: 1 min 43 s** on a machine with only Rust and
Python installed (Apple M3, 8 cores, measured 2026-10-24): 99 s to
download and build `openxet` with an empty Cargo cache, 3 s to install
`hf_xet`, and 0.5 s for steps 2 to 5 together. This end-to-end figure,
install included, is measured by hand per release, against the 2-minute
target of
[design §5.8](superpowers/specs/2026-10-23-openxet-design.md#58-scale-targets-initial-validated-by-benchmarks-revised-with-data).
CI times only steps 2 to 5, and fails them past the same 2 minutes.

## 1. Install

There is no release yet, so `openxet` is built from source. This needs
[Rust](https://rustup.rs) 1.97 or newer and Python 3.8 or newer.
`hf_xet` goes into a virtual environment, which also makes it the
`python3` of the steps below:

```bash install
cargo install --locked --git https://github.com/stubbi/openxet openxet
python3 -m venv .venv && . .venv/bin/activate
pip install 'hf_xet>=1.7.0'
```

`hf_xet` 1.7.0 is the oldest version OpenXet supports.

## 2. Start the server

```bash
openxet serve --data-dir openxet-data --port 0 --json > serve.json 2> serve.log &
SERVER=$!
until [ -s serve.json ]; do sleep 0.1; done
```

With no configuration file, `openxet serve` runs in
[dev mode](reference/config.md#dev-mode): it creates `openxet-data` and
one write token for the scope `default`. `--port 0` picks a free port.
Once the server is ready, `--json` prints one line, which the steps below
read the endpoint and the token from:

```json
{"schema":"openxet.serve.v1","version":"0.0.0","endpoint":"http://127.0.0.1:56398","data_dir":"/home/me/openxet-data","scope":"default","mode":"write","token":"oxt_REDACTED","token_exp":4611686018427387904}
```

Without `--json`, the server prints a banner with the same values and
ready-to-paste upload commands instead. Its log goes to stderr, here
`serve.log`; the log never holds a token.

## 3. Upload and download with `hf_xet`

Make a 4 MiB test file:

```bash
head -c 4194304 /dev/urandom > model.bin
```

Then upload it and download it again with `hf_xet`, as `huggingface_hub`
drives it:

```python
import json

import hf_xet

server = json.load(open("serve.json"))
cas = dict(
    endpoint=server["endpoint"],
    token=server["token"],
    token_expiry_unix_secs=server["token_exp"],
)
session = hf_xet.XetSession()

with session.new_upload_commit(**cas) as commit:
    upload = commit.start_upload_file("model.bin")
info = upload.result().xet_info
print(f"uploaded model.bin as {info.hash} ({info.file_size} bytes)")

with session.new_file_download_group(**cas) as group:
    group.start_download_file(hf_xet.XetFileInfo(info.hash, info.file_size), "model.hf_xet")
assert open("model.hf_xet", "rb").read() == open("model.bin", "rb").read()
print("downloaded model.hf_xet: identical")
```

The file is named by its Xet hash, which is all a download needs.

## 4. Upload and download with `openxet`

The `openxet upload` and `openxet download` commands read the endpoint and
the token from the environment:

```bash
export OPENXET_ENDPOINT=$(python3 -c 'import json; print(json.load(open("serve.json"))["endpoint"])')
export OPENXET_TOKEN=$(python3 -c 'import json; print(json.load(open("serve.json"))["token"])')
openxet upload model.bin
```

`hf_xet` has already stored every chunk of `model.bin`, so nothing is sent
again:

```text
PATH       XET HASH                                                             SIZE  SHA256
model.bin  e8e4019d55c5f8f86d74f62e5fd852ae14b1692dadb730a5a21b944c82902a91  4194304  75a9ac9da18ee57a29d9cf976951b064f00440758c287fb9f4e13a431785ad95
Uploaded 0 B of 4.0 MiB (100% deduplicated).
```

With `--json`, every command prints one JSON object with a versioned
`schema`, for scripts and agents. This uploads, takes the hash from the
answer and downloads the file by it:

```bash
HASH=$(openxet upload model.bin --json | python3 -c 'import json, sys; print(json.load(sys.stdin)["files"][0]["hash"])')
openxet download "$HASH" --output model.openxet --json
cmp model.bin model.openxet
```

```json
{"schema":"openxet.download.v1","hash":"e8e4019d55c5f8f86d74f62e5fd852ae14b1692dadb730a5a21b944c82902a91","path":"model.openxet","size":4194304}
```

A failure is one `openxet.error.v1` object on stderr, with a stable `code`,
a `hint` naming the next action and a documented
[exit code](reference/exit-codes.md).

## 5. Stop the server

```bash
kill "$SERVER"
```

The server stops gracefully. `openxet-data` keeps the data and the dev
token, and the next `openxet serve --data-dir openxet-data` reuses both. If
you delete `openxet-data` instead, also clear the client cache
(`~/.cache/huggingface/xet`), which remembers what the old server stored
([stale client caches](reference/config.md#stale-client-caches)).

## Next

- [Server configuration](reference/config.md): real credentials (static
  tokens, and CAS tokens a platform signs), dedup visibility and transfer
  URLs.
- [Command line](reference/cli.md) and [exit codes](reference/exit-codes.md).
- [CAS routes](reference/routes.md): every route, status and error code.
- [Quickstart: hf and git](quickstart-hub.md): the same server with
  repositories, `hf upload` and `git push`.
