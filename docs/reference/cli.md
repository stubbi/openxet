# Command line

Every command takes `--json`, which prints one JSON object with a versioned
`schema` on stdout instead of text, and reports failures as one
`openxet.error.v1` object on stderr ([exit codes](exit-codes.md)). No
command prompts: stdin is never read.

| Command | Does | `--json` schema |
| ------- | ---- | --------------- |
| `openxet version` | Prints version and build information | `openxet.version.v1` |
| `openxet serve` | Serves the Xet CAS | `openxet.serve.v1` |
| `openxet config check` | Validates and explains the server configuration | `openxet.config-check.v1` |
| `openxet token keygen` | Writes a key pair for signing CAS tokens | `openxet.token-keygen.v1` |
| `openxet token mint` | Signs a CAS token | `openxet.token-mint.v1` |
| `openxet token generate` | Generates a static token and its config entry | `openxet.token-generate.v1` |
| `openxet upload` | Uploads files and prints their Xet hashes | `openxet.upload.v1` |
| `openxet download` | Downloads a file by its Xet hash | `openxet.download.v1` |

## `openxet serve`

```
openxet serve [--config FILE] [--data-dir DIR] [--port N] [--log-format text|json] [--json]
```

Serves the CAS until `SIGTERM` or `SIGINT` (see
[Lifecycle](config.md#lifecycle)). Without `--config`, or with a file that
has no `[auth]` section, it runs in [dev mode](config.md#dev-mode) and
prints a token. `--port 0` picks a free port; the ready line names it.

Once the server is ready, stdout gets the banner, or with `--json` one
line:

```json
{"schema":"openxet.serve.v1","version":"0.0.0","endpoint":"http://127.0.0.1:8080","data_dir":"./openxet-data","scope":"default","mode":"write","token":"oxt_REDACTED","token_exp":4611686018427387904}
```

`data_dir` is absolute. `scope`, `mode`, `token` and `token_exp` are `null`
outside dev mode. Logs
go to stderr. Exit codes: 0 after a graceful stop, 1 if startup fails (the
error names what to fix) or after a second stop signal.

## `openxet config check`

```
openxet config check [--config FILE] [--json]
```

Loads the configuration as `serve` would, from the file and the
`OPENXET_<SECTION>__<KEY>` environment, checks the files it names (the
data directory can be written; `transfer.key` and `dev-token` are well
formed and private), and reports either every problem or every effective
setting with its source. Values under `[auth]` are always `<redacted>`.
`warnings` lists what `serve` accepts but warns about, such as a data
directory other users may enter. To prove the data directory (or, if
it doesn't exist yet, its nearest existing parent) can be written, it
creates one empty file named `.openxet-check-<16 hex>` there with
`O_CREAT|O_EXCL` and removes it at once; nothing else is written. Exit code 0 if the configuration is valid,
1 with error code `config` if not.

```json
{"schema":"openxet.config-check.v1","valid":false,"file":"openxet.toml","dev_mode":null,
 "problems":[{"key":"cas.url_lifetime","value":"5m","source":"file openxet.toml",
              "rule":"must be a whole number followed by s, m, h or d, such as 30s, 60m, 24h or 30d, from 30m to 60m inclusive",
              "hint":"Use 60m unless you need otherwise. …"}],
 "warnings":[],
 "settings":[{"key":"server.bind","value":"127.0.0.1:8080","source":"default"}, …]}
```

`dev_mode` is `true` or `false` for a valid configuration and `null` for an
invalid one.

## `openxet token keygen`

```
openxet token keygen [--key FILE] [--json]
```

Writes a new Ed25519 private key to `FILE` (mode `0600`, PKCS#8 v1 PEM,
RFC 8410 §7, which PyJWT loads as is) and
its public key to `FILE.pub` (SPKI PEM), and prints the public key, its
`kid` and the `[auth]` entry that trusts it. `FILE` defaults to
`$XDG_CONFIG_HOME/openxet/token.key`, or `~/.config/openxet/token.key`, never
the working directory, where a key would be one `git add` from a commit.
It never overwrites a file: if either exists, it fails with error code
`exists` (exit 2) and writes nothing. The public key is written first, so a failure
never leaves a private key without its public half.

```json
{"schema":"openxet.token-keygen.v1","kid":"8f1c2a3b4d5e6f70","private_key":"/home/me/.config/openxet/token.key","public_key":"/home/me/.config/openxet/token.key.pub","public_key_pem":"-----BEGIN PUBLIC KEY-----\n…"}
```

## `openxet token mint`

```
openxet token mint --scope S --mode read|write [--ttl 1h] [--subject NAME] [--key FILE] [--json]
```

Signs a CAS token with the key `token keygen` wrote (by default the one it
writes by default). `--ttl` is from `1m` to `30d` (default `1h`); a server
also refuses a token that expires beyond its `auth.max_token_lifetime`
(`24h` by default). `--subject` (default `openxet-cli`) becomes the `sub`
claim. Without `--json`, stdout is the token alone, so
`TOKEN=$(openxet token mint …)` works.

```json
{"schema":"openxet.token-mint.v1","token":"eyJ…","kid":"8f1c2a3b4d5e6f70","subject":"openxet-cli","scope":"team/models","mode":"read","iat":1792800000,"exp":1792803600}
```

## `openxet token generate`

```
openxet token generate --mode read|write --subject NAME [--scope S] [--json]
```

Generates a static token (`oxt_` and 43 base64url characters; see
[Token format](config.md#token-format)) and prints it with its
`[[auth.static_tokens]]` entry, which holds only the token's SHA-256. The
token is printed this once; openxet never stores it. `--subject` is
required, so that each token's actions are attributed to it; `--scope`
defaults to `default`.

```json
{"schema":"openxet.token-generate.v1","token":"oxt_REDACTED","sha256":"…","scope":"ci","mode":"write","subject":"ci-bot","config":"[[auth.static_tokens]]\nsha256 = \"…\"\n…"}
```

## `openxet upload`

```
openxet upload <PATH>... [--endpoint URL] [--token TOKEN] [--cache-dir DIR] [--json]
```

Uploads each file and prints its Xet hash, size and SHA-256. Chunking,
xorbs and shards come from `xet-data` 1.7.0, the same client core as
`hf_xet` (ADR 0006), so a file uploaded here has the hash `hf_xet` gives it
and either tool downloads it. Chunks the server or the local cache already
holds are not sent again.

With the server `openxet serve` prints in dev mode:

```bash
export OPENXET_ENDPOINT=http://127.0.0.1:8080 OPENXET_TOKEN=oxt_REDACTED
openxet upload model.bin config.json
```

```
PATH         XET HASH                                                              SIZE  SHA256
model.bin    36f01138cb24db2e9c72d0c9fdb7dff31322b73b2e60b4d365bb1e3fb54d0f23  50000000  ba2f32d4…
config.json  …
Uploaded 1.2 MiB of 47.7 MiB (97% deduplicated).
```

```json
{"schema":"openxet.upload.v1","files":[{"path":"model.bin","hash":"36f0…0f23","size":50000000,"sha256":"ba2f…e3ec"}],"bytes_uploaded":1258291,"bytes_deduplicated":48741709}
```

- **Dedup savings.** `bytes_uploaded` counts the files' content bytes in
  chunks the server didn't have, which this upload sent (before
  compression); `bytes_deduplicated` counts those the server or the local
  cache already had. They add up to the files' total size.
- **Empty files.** An empty file's `sha256` is the digest of zero bytes,
  `e3b0c442…b855`. `hf_xet` 1.7.0 reports, and writes into the shard, all
  zeros for it instead; openxet sends the same zeros, so only what is shown
  differs.

- **Paths** are taken literally, in order: no globbing and no directory
  walking. `path` in the output is the path as given. If any path is
  missing, not a regular file, or unreadable, nothing is uploaded: the
  command exits 2 with error code `invalid_input` and lists every such
  path in `files`, each with a `problem` of `not_found`, `not_a_file` or
  `unreadable`:

  ```json
  {"schema":"openxet.error.v1","code":"invalid_input","message":"1 path(s) can't be uploaded: data/ (not a regular file); nothing was uploaded","hint":"…","files":[{"path":"data/","problem":"not_a_file"}]}
  ```

- **All or nothing.** Every file is registered by one shard upload at the
  end, after every xorb is stored, so a failed upload registers none.
  Uploading the same files again is safe and sends no data the server has.

## `openxet download`

```
openxet download <HASH> --output PATH [--size N] [--force] [--endpoint URL] [--token TOKEN] [--cache-dir DIR] [--json]
```

Downloads the file with Xet hash `HASH` (as `openxet upload` printed it) to
`PATH`, and prints `Downloaded <hash> to <path> (<size>).`. With `--size`,
a file of another size fails with `size_mismatch` (exit 1), and nothing is
written.

```json
{"schema":"openxet.download.v1","hash":"36f0…0f23","path":"model.bin","size":50000000}
```

- **No overwrite.** If `PATH` exists, the command exits 2 with error code
  `output_exists` and downloads nothing, unless `--force` is given.
- **Atomic.** The file is written to a new file next to `PATH`, named
  `.<name>.<random>.openxet-partial` and created exclusively (a symlink
  planted at any name is never followed), then synced and renamed (with
  `--force`) or linked (without) to `PATH`, so `PATH` is either absent or
  complete. A failed or interrupted download removes the partial file.
  Only `SIGKILL` or a power loss can leave one behind: a hidden
  `.<name>.XXXXXX.openxet-partial` next to the output, which is safe to
  delete.
- **Directories.** Missing parent directories of `PATH` are created, with
  the usual mode for your umask, as `hf_xet` does.
- **Filesystems without safe landing.** Without `--force`, the file is
  placed with an exclusive rename or a hard link, so it can never replace a
  file that appeared meanwhile. Where the filesystem supports neither (for
  example SMB or exFAT on macOS, FAT on Linux), the download fails with
  `no_clobber_unsupported` and writes nothing; pass `--force` there.

## Endpoint, token and cache

`upload` and `download` take:

| Option | Default | Meaning |
| ------ | ------- | ------- |
| `--endpoint URL` | `$OPENXET_ENDPOINT` | The CAS endpoint, an `http` or `https` URL such as the one `openxet serve` prints. |
| `--token TOKEN` | `$OPENXET_TOKEN` | The bearer token: a static token (`oxt_…`) or a CAS token from `openxet token mint`. Prefer the variable: command-line arguments are visible to other users of the machine. |
| `--cache-dir DIR` | as `hf_xet` | The Xet cache root, in place of `HF_XET_CACHE`. |

- **Missing settings** exit 2 before anything runs: `missing_endpoint`
  names `OPENXET_ENDPOINT`, `missing_token` names `OPENXET_TOKEN`.
- **Token expiry.** A CAS token (a JWT) is checked against its `exp` claim,
  read without verifying the signature (the server verifies). Within 30
  seconds of expiry the command fails with `token_expired` before sending
  anything, as `hf_xet` would mid-transfer. Static tokens don't expire.
- **Unreachable endpoint.** The command dials where its requests go first,
  and fails at once with `unreachable_endpoint` if nothing answers within
  10 seconds. That is the endpoint, or the proxy reqwest would use for it
  (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY`, else the system
  settings); a proxy is named in the message.
- **Cache.** Without `--cache-dir`, the cache root is chosen exactly as by
  `hf_xet`: `$HF_XET_CACHE`, else `$HF_HOME/xet`, else
  `$XDG_CACHE_HOME/huggingface/xet`, else `~/.cache/huggingface/xet`. It
  holds one directory per endpoint, with the shards of past uploads
  (kept 21 days) that let later uploads skip chunks the server has. If a
  server is reset while the cache remembers its data, uploads fail with
  `upload_rejected` and the server's message says to clear the cache: remove
  the endpoint's directory under the cache root, or point `--cache-dir` at
  a new, empty directory. A cache root openxet creates is mode `0700`,
  since its shards list your files' hashes; an existing one, which
  `hf_xet` may share, is left as it is.
- **No telemetry.** The client never sends telemetry, whatever
  `HF_XET_TELEMETRY_ENABLED` says. Requests carry
  `User-Agent: openxet/<version>`.
- **Output.** With `--json`, stdout has the result and stderr nothing but
  an `openxet.error.v1` object on failure. Without it, a progress line is
  drawn on stderr when stderr is a terminal. The token is never printed,
  and server text (an upload rejection's reason) is printed without
  control characters.
- **Interrupts.** `SIGINT` (Ctrl-C) or `SIGTERM` stops the transfer, removes
  a partial download, and exits 130 with error code `interrupted`. An
  interrupted download wrote nothing to its output. An interrupted upload
  may or may not have been registered, if the signal came as the server
  took its shard; re-running `openxet upload` is safe, since uploads are
  idempotent and skip what the server already has.

Other `HF_XET_*` variables tune the client core as they tune `hf_xet`, for
example `HF_XET_CLIENT_RETRY_MAX_ATTEMPTS`.
