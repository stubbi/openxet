# Quickstart: hf and git

<!-- description: Start an OpenXet server, upload a folder with the stock hf CLI, download it again, then clone, change and push the same repository with git. -->

Start an OpenXet server on your machine, upload a folder to it with
Hugging Face's stock `hf` command line, download it again, then clone the
same repository with `git`, change it and push. Uploads made with `hf` and
pushes made with `git` land in one Git history, and large files travel
through Xet, so a new version uploads only what changed.

Every `bash` block on this page except the install block runs in CI, in
order, exactly as written (`compat/tests/test_docs_quickstart.py`); CI
provides what the install block installs. Run them in one terminal, in an
empty directory. CI fails the walkthrough, steps 2 to 7, past 2 minutes.

## 1. Install

There is no release yet, so `openxet` is built from source. This needs
[Rust](https://rustup.rs) 1.97 or newer, Python 3.10 or newer and `git`
2.37 or newer, with your name and email set in `git config`. The `hf`
command line goes into a virtual environment:

```bash install
cargo install --locked --git https://github.com/stubbi/openxet openxet
python3 -m venv .venv && . .venv/bin/activate
pip install 'huggingface_hub>=2.2.0' 'hf_xet>=1.7.0'
```

`huggingface_hub` 2.2.0 and `hf_xet` 1.7.0 are the oldest versions
OpenXet is tested with. The server runs `git` itself, so it needs `git`
on its `PATH` too.

## 2. Start the server

```bash
openxet serve --data-dir openxet-data --port 0 --json > serve.json 2> serve.log &
SERVER=$!
until [ -s serve.json ]; do sleep 0.1; done
```

With no configuration file, `openxet serve` runs in
[dev mode](reference/config.md#dev-mode): it creates `openxet-data` and one
token for the user `dev`, who may create, read and write every
repository. `--port 0` picks a free port. Once the server is ready,
`--json` prints one line with the endpoint and the token:

```json
{"schema":"openxet.serve.v1","version":"0.0.0","endpoint":"http://127.0.0.1:56398","data_dir":"/home/me/openxet-data","scope":"default","mode":"write","token":"oxt_REDACTED","token_exp":4611686018427387904}
```

Without `--json`, the server prints a banner with the same values and the
`hf` and `git` commands to paste. Its log goes to stderr, here
`serve.log`, and never holds a token.

## 3. Point `hf` at the server

`hf` and `huggingface_hub` read the server's URL from `HF_ENDPOINT` and the
token from `HF_TOKEN`:

```bash
export HF_ENDPOINT=$(python3 -c 'import json; print(json.load(open("serve.json"))["endpoint"])')
export HF_TOKEN=$(python3 -c 'import json; print(json.load(open("serve.json"))["token"])')
hf auth whoami
```

```text
✓ Logged in
  user: dev
  endpoint: http://127.0.0.1:56398
```

`hf auth login --token` works too, and stores the token in
`~/.cache/huggingface` for later shells.

## 4. Upload a folder

Make a folder with a small README and a 4 MiB file, then upload it to the
repository `dev/demo`. `hf upload` creates the repository first:

```bash
mkdir demo
echo "# demo" > demo/README.md
head -c 4194304 /dev/urandom > demo/data.bin
hf upload dev/demo demo .
```

```text
Found 2 files to upload
✓ Uploaded
  url: http://127.0.0.1:56398/dev/demo/commit/48995c751d05a7c3ccb8ea13b2b49a4b9a9e9507
```

`data.bin` is over the 1 MiB `hub.lfs_threshold`
([config](reference/config.md#hub)), so `hf_xet` uploaded it to the
server's Xet storage in chunks, and the commit holds a Git LFS pointer to
it. `README.md` is small, so it travelled inside the commit as an ordinary
Git file.

## 5. Download it

```bash
hf download dev/demo --local-dir downloaded
cmp demo/data.bin downloaded/data.bin
```

`cmp` prints nothing: the files are identical. `downloaded` also holds
the `.gitattributes` the server wrote in the repository's first commit, the
same file huggingface.co starts a repository with.

## 6. Clone, change and push with git

git sends the token as the password of HTTP Basic; the user name can be
anything. This small credential helper answers git with the token from
`HF_TOKEN`, so the token is never written to a file:

```bash
cat > git-credential-openxet <<'EOF'
#!/bin/sh
test "$1" = get && printf 'username=openxet\npassword=%s\n' "$HF_TOKEN"
EOF
chmod +x git-credential-openxet
git clone -c credential.helper= -c credential.helper="$PWD/git-credential-openxet" "$HF_ENDPOINT/dev/demo" demo-git
git -C demo-git log --format='%h %an: %s'
```

The two `-c` options are saved in the clone's own configuration: the
empty one stops any helper from your global configuration from storing
the token, and the second one is the helper above. The log shows the
`hf upload` commit on top of the repository's initial commit:

```text
48995c7 dev: Upload folder using huggingface_hub
7965cc2 openxet: initial commit
```

With [Git LFS](https://git-lfs.com) installed, the clone holds
`data.bin` itself, which `git-lfs` downloads from the server with the same
credential; without it, `data.bin` is the small pointer file. Now change
the README and push:

```bash
echo "Changed with git." >> demo-git/README.md
git -C demo-git commit -q -am "Edit the README with git"
git -C demo-git push
hf download dev/demo README.md --local-dir after
grep "Changed with git." after/README.md
```

The push moves `main`, and `hf download` reads the new README from the
same history. To push a large file with git, use Git LFS with `git-xet`:
see [Git hosting](guides/git-hosting.md#large-files).

## 7. Stop the server

```bash
kill "$SERVER"
```

The server stops gracefully. `openxet-data` keeps the repository, its
history and the dev token, and the next
`openxet serve --data-dir openxet-data` serves them again. If you delete
`openxet-data` instead, also clear the client cache
(`~/.cache/huggingface/xet`), which remembers what the old server stored
([stale client caches](reference/config.md#stale-client-caches)).

## Next

- [Server configuration](reference/config.md): real users with
  [static tokens](reference/config.md#static-tokens) and
  [rules](reference/config.md#rules) that say who may read, write and
  create which repositories.
- [Git hosting](guides/git-hosting.md): URLs, credentials, branches and
  what the server refuses, and why.
- [Git LFS for any Git host](guides/lfs-for-any-repo.md): keep a
  repository on GitHub, GitLab or Gitea and its large files on OpenXet.
- [Hub routes](reference/hub-routes.md) and
  [Git routes](reference/git-routes.md): every route `hf`, `huggingface_hub`
  and `git` call.
- [Labels](reference/cli.md#openxet-label): a repository is a model, a
  dataset or anything else through its labels.
