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
directory other users may enter. Exit code 0 if the configuration is valid,
1 with error code `config` if not.

```json
{"schema":"openxet.config-check.v1","valid":false,"file":"openxet.toml","dev_mode":null,
 "problems":[{"key":"cas.url_lifetime","value":"5m","source":"file openxet.toml",
              "rule":"must be a whole number followed by s, m, h or d, such as 30s, 60m, 24h or 30d, from 30m to 24h inclusive",
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

Writes a new Ed25519 private key to `FILE` (mode `0600`, PKCS#8 PEM) and
its public key to `FILE.pub` (SPKI PEM), and prints the public key, its
`kid` and the `[auth]` entry that trusts it. `FILE` defaults to
`$XDG_CONFIG_HOME/openxet/token.key`, or `~/.config/openxet/token.key`, never
the working directory, where a key would be one `git add` from a commit.
It never overwrites a file: if either exists, it fails with error code
`exists` and writes nothing. The public key is written first, so a failure
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
