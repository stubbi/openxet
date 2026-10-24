# Server configuration

`openxet serve` reads one TOML file (`--config FILE`), then environment
variables, then command-line flags. Each layer overrides the one before it,
key by key. With no file and no `[auth]` section, the server runs in
[dev mode](#dev-mode).

```bash
openxet config check --config openxet.toml          # every problem, or every effective setting
openxet config check --config openxet.toml --json   # the same as openxet.config-check.v1
```

`config check` reports every problem at once, each with its key path,
value, source (`file …`, `env …`, `flag …` or `default`), the rule it
breaks and a hint. For a valid configuration it also checks the files it
names, as `serve` would: that `data_dir` can be written (it creates and
at once removes one empty `.openxet-check-<16 hex>` file there, the only
write it makes), that `data_dir` is not a symbolic link to nothing, and that
`transfer.key` and `dev-token`, if present, are well formed and private. A
data directory other users may enter is a warning. `serve` refuses an
invalid configuration with exit code 1 and error code `config`, and points
at `config check`.

`config check` never shows a value under `[auth]`: it prints `<redacted>`
instead, in text and JSON alike. A value that looks like an openxet token
(`oxt_…`), such as a token pasted where its `sha256` belongs, is redacted
wherever it appears, and the hint says to rotate the token and configure its
hash.

## Sources

| Source | Form | Example |
| ------ | ---- | ------- |
| File | `[section]` and `key = value` | `[cas]` `url_lifetime = "2h"` |
| Environment | `OPENXET_<SECTION>__<KEY>`, case-insensitive, `__` between section and key | `OPENXET_CAS__URL_LIFETIME=2h` |
| Flags | `--port N` replaces the port of `server.bind`; `--data-dir DIR` and `--log-format text\|json` replace their keys | `--port 0` |

An environment value is taken as written for string keys. For the other
keys it is a TOML value, as in the file: `OPENXET_SERVER__BODY_MIN_RATE=8192`,
`OPENXET_AUTH__JWT_PUBLIC_KEYS='["/etc/openxet/issuer.pub"]'`, or
`OPENXET_AUTH__STATIC_TOKENS='[{sha256 = "…", scope = "ci", mode = "write", subject = "ci"}]'`.
Only variables with `__` are configuration: `OPENXET_ENDPOINT` and
`OPENXET_TOKEN`, which the client commands read, are not. An unknown key in
the file or the environment is a problem, not ignored.

Durations are a whole number and one unit: `s`, `m`, `h` or `d`, such as
`30s`, `60m`, `24h` or `30d`.

## Keys

### `[server]`

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `bind` | `127.0.0.1:8080` | an IP address and port | Where to listen. `0.0.0.0:8080` or `[::]:8080` listens on every interface and then needs `public_url`. `hf_xet`'s default CAS endpoint is `http://localhost:8080`. |
| `public_url` | `http://` and the bound address | absolute `http` or `https` URL of an origin root | The URL clients reach the server at. Every transfer URL is built from it. |
| `data_dir` | `./openxet-data` | a directory | Holds `index.sqlite`, `bucket/`, `transfer.key` and, in dev mode, `dev-token`. |
| `log_format` | `text` | `text` or `json` | Log lines on stderr. |
| `log_level` | `info` | `error`, `warn`, `info`, `debug` or `trace` | openxet's own log level. Other libraries log warnings and errors only. `debug` logs one line per request. |
| `header_read_timeout` | `30s` | `1s` to `5m` | How long a connection may take to send a request line and headers. It also closes a keep-alive connection that sits idle this long; clients open a new one. |
| `body_read_timeout` | `300s` | `1s` to `1h` | The longest gap allowed between two parts of a request body. It matches `hf_xet`'s own 300 s read timeout. |
| `body_min_rate` | `4096` | `1` to `1048576` | The least average rate of a request body, in bytes per second, after a first `body_read_timeout`: a body of `n` bytes may take `body_read_timeout + n / body_min_rate`. The default lets a 64 MiB xorb take up to 4.6 hours over a very slow link. |
| `shutdown_grace` | `30s` | `1s` to `10m` | On `SIGTERM` or `SIGINT`, how long in-flight requests get to finish. |

A body that breaks `body_read_timeout` or `body_min_rate` is refused with
`408 body_timeout`, and the connection is closed. This also bounds how long
a refused upload's body is read and discarded before the refusal is sent.

`public_url` must be the origin root, with no path, query, fragment or user
info. Clients append `/v1/...` to it and resolve `/v1/telemetry` against
the origin (`docs/research/S10-cas-wire-contract.md`), so the CAS can't
live under a path prefix. Behind a proxy that adds one, give openxet its own
host name instead. A trailing `/` is allowed and ignored.

### `[cas]`

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `dedup_visibility` | `global` | `global` or `per-scope` | Whether one scope's xorbs are visible to another. See [Dedup visibility](#dedup-visibility). |
| `url_lifetime` | `60m` | `30m` to `24h` | How long a signed transfer URL is valid. See [Transfer URLs](#transfer-urls). |
| `heartbeat_interval` | `5s` | `1s` to `2m` | How often a `/v2/shards` upload stream repeats its progress while the upload is verified and committed. `hf_xet` fails a stream that is silent for 300 s. |
| `transfer_key_file` | `<data_dir>/transfer.key` | a file path | The key that signs transfer URLs. Set explicitly, the file must exist. See [Keys and tokens on disk](#keys-and-tokens-on-disk). |

### `[auth]`

Any `[auth]` section, even an empty one, turns dev mode off. It must then
configure at least one credential.

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `static_tokens` | none | an array of tables | Long-lived tokens, by hash. See [Static tokens](#static-tokens). |
| `jwt_public_keys` | none | an array of strings | Ed25519 public keys trusted to sign CAS tokens: a file path, or the PEM itself. See [CAS tokens](#cas-tokens). |
| `max_token_lifetime` | `24h` | `1m` to `30d` | A CAS token whose `exp` lies further ahead than this is refused. |

Every request needs a credential: there are no anonymous reads. The two
exceptions are the probes, `GET /healthz` and `GET /readyz`, which answer
a fixed body that says nothing about stored data.

## Dev mode

Without a configuration file, or with one that has no `[auth]` section,
`openxet serve` creates in `data_dir`, on first start:

- `index.sqlite` and `bucket/`, the data (see [Keys and tokens on disk](#keys-and-tokens-on-disk) for modes)
- `dev-token` (mode `0600`): one static write token for scope `default`
- `transfer.key` (mode `0600`)

It reuses all of them on every later start, so data, the token and transfer
URLs signed before a restart keep working. It prints the endpoint, the data
directory, the token and ready-to-paste upload commands on stdout. With
`--json` it prints one line instead:

```json
{"schema":"openxet.serve.v1","version":"0.0.0","endpoint":"http://127.0.0.1:8080","data_dir":"./openxet-data","scope":"default","mode":"write","token":"oxt_REDACTED","token_exp":4611686018427387904}
```

`token_exp` is `2**62`: a static token never expires, and `hf_xet` needs an
integer, so pass it as the token's `exp`. In configured mode the same line
has `null` for `scope`, `mode`, `token` and `token_exp`. The banner on
stdout is the only place openxet ever prints a credential; logs never hold
one.

## Keys and tokens on disk

The data directory holds every scope's data, readable there without a
credential, so on unix openxet keeps it private:

- It creates `data_dir` and every directory under it with mode `0700`, and
  `index.sqlite` (with its `-wal`, `-shm` and `-lock` files), the bucket's
  objects and the secret files with mode `0600`. The modes are set when
  each is created; the umask can only narrow them.
- If `data_dir` already exists and group or other users may enter it,
  `serve` starts but logs a warning naming the `chmod 700` that fixes it,
  and `config check` lists it under `warnings`.

openxet creates secret files whole (written to a temporary file, synced,
then linked into place) and never overwrites them. Of two servers starting
at once on one data directory, both use the first one's `dev-token` and
`transfer.key`. openxet refuses to start if a secret file can be read by
group or other users, and names the `chmod 600` that fixes it. A crash
between writing and linking can leave a temporary `.<name>.<16 hex>.tmp`
file holding the secret; `serve` removes those older than an hour at its
next start, and never a younger one another server may be writing.

Servers starting together on one data directory open the index one at a
time, under an advisory lock on `index.sqlite-lock`. A server that waits
for it logs so at info level, naming the lock file, and again when it gets
the lock; `/readyz` answers `503` meanwhile. It gives up after a minute.

`transfer.key` holds 32 random bytes as 64 hex characters, and signs
transfer URLs. Every node behind one `public_url` must share the same key;
copy the file to each node. Replacing it invalidates every transfer URL
already handed out.

- With the default `cas.transfer_key_file`, a missing key is generated. In
  dev mode this is silent. With `[auth]` configured, `serve` logs a warning
  naming the file and saying that every node serving the deployment must
  share it, since a node-local key makes other nodes refuse its URLs.
- An explicitly set `cas.transfer_key_file` must exist: `serve` refuses to
  start with error code `no_key`. Create the key once with
  `(umask 077; openssl rand -hex 32 > /etc/openxet/transfer.key)` and copy
  it to every node.
- Remote bucket backends (M3) must require the key: with a shared bucket
  there is no shared data directory to hold a generated one.

## Token format

Tokens openxet generates (the dev token and `openxet token generate`) are
`oxt_` followed by 43 base64url characters: 32 bytes from the operating
system's random number generator, unpadded. The fixed prefix lets secret
scanners find a token committed by accident; this repository's own scan
(`compat/tests/test_fixture_redaction.py`) matches `oxt_[A-Za-z0-9_-]{43}`.

## Static tokens

A static token is configured by the SHA-256 of its UTF-8 bytes, never by
the token itself:

```toml
[[auth.static_tokens]]
sha256 = "<64 lowercase hex characters>"
scope = "ci"
mode = "write"      # or "read"
subject = "ci-bot"  # who the token acts for
```

`openxet token generate --scope ci --mode write --subject ci-bot` prints a
new token and this entry. Give each token its own `subject`, so the audit
log tells them apart. A presented token is hashed and compared against
every configured hash in constant time. `scope` and `subject` must not be
empty or contain control characters, and `scope` is at most 1024 bytes.
`write` may upload; `read` may download and query.

## CAS tokens

CAS tokens are Ed25519-signed JWTs (ADR 0007), for platforms that issue
short-lived, scoped tokens. `openxet token keygen` writes a key pair (by default to
`$XDG_CONFIG_HOME/openxet/token.key`, or `~/.config/openxet/token.key`, and
`token.key.pub` beside it) and `openxet token mint` signs one; any JWT library that does `EdDSA` can too.
openxet accepts exactly this:

- **Keys:** `jwt_public_keys` lists one or more Ed25519 public keys in SPKI
  PEM (`-----BEGIN PUBLIC KEY-----`, RFC 8410). Signing keys are PKCS#8 PEM
  (`-----BEGIN PRIVATE KEY-----`). `token keygen` writes the v1 form
  (RFC 8410 §7), since Python's `cryptography` (checked with 50.0.2, under
  PyJWT) refuses v2 with the public key attached; `token mint` reads both.
  List several public keys to rotate: sign
  with the new key once every node trusts it, then remove the old one.
- **`kid`:** the first 16 lowercase hex characters of the SHA-256 of the raw
  32-byte public key. `openxet token keygen` and `config check` print it.
- **Header:** exactly `{"alg":"EdDSA","typ":"JWT","kid":"<kid>"}`. Any other
  algorithm or header parameter is refused.
- **Claims:** `sub`, `scope`, `mode` (`"read"` or `"write"`), `exp` and
  `iat` (Unix seconds), plus an optional `nbf`. Any other claim, such as
  `aud`, `iss` or `jti`, makes the token invalid, since openxet can't honour
  it. `sub` and `scope` follow the static-token rules above.
- **Time:** a token is valid while `now < exp` and invalid from the `exp`
  second on, with no leeway (stock clients refresh 30 s before `exp`).
  It is invalid before `nbf`. `exp` may lie at most `max_token_lifetime`
  ahead, and `iat` at most 5 minutes ahead (clock skew).
- **Size:** at most 8 KiB.

`openxet token mint` refuses a `--ttl` outside `1m` to `30d`; a server
refuses a token beyond its own `max_token_lifetime`.

## Transfer URLs

A reconstruction answers with signed URLs of the form
`{public_url}/v1/transfer/xorbs/{xorb}?range={start}-{end}&exp={unix}&sig=…`,
each valid for `url_lifetime` and only with exactly its `Range` header.
`url_lifetime` is at least 30 minutes, the longest `hf_xet` holds a URL
before its last use (ADR 0002), and at most 24 hours, so a leaked URL stops
working. A URL whose `exp` lies further ahead than this server's
`url_lifetime` (plus 5 minutes of clock skew) is refused, so shortening
`url_lifetime` on a live server invalidates outstanding URLs signed under
the longer lifetime. Clients recover by requesting a fresh reconstruction,
which `hf_xet` does on the `403` by itself.

## Dedup visibility

Files are always visible only in the scope they were uploaded to.
`dedup_visibility` decides xorbs, the deduplicated chunk containers:

- `global` (the default): every stored xorb is visible to every scope. An
  upload skips any chunk stored by anyone, which saves the most space and
  bandwidth.

  One dedup match offers the client every xorb of the latest upload
  (shard) that stored the matched chunk, up to 16 MiB of answer. So a
  client with an empty cache that re-uploads an edited file sends little
  more than the edit: about 1.2 MB for a 1 MiB edit of a 100 MiB file.
  The price is what one matching chunk reveals: anyone who holds one
  chunk of an upload learns that upload's xorb hashes, chunk counts and
  chunk lengths (never its data or raw chunk hashes); choose `per-scope`
  if tenants must not learn that about each other.
- `per-scope`: a scope sees only xorbs uploaded with its own credentials.
  To any other scope, another scope's xorb reads as absent: `HEAD` of it
  answers `404`, dedup queries find no match in it, and a shard that
  references it is refused with `unknown_xorb`. A dedup answer offers only
  xorbs of the scope's own uploads. So no scope can learn what
  another has stored, and each scope uploads its own copy of shared data
  (stored once in the bucket, all the same).

## Stale client caches

Clients cache dedup information for up to 21 days per endpoint. If a
server's data is reset (a new `data_dir`, or a wiped bucket), a client may
still reference xorbs the server no longer has, and its shard upload fails
with `unknown_xorb`, whose message says so. Clear the client's Xet cache for
that endpoint (`~/.cache/huggingface/xet`, or `$HF_XET_CACHE`) and upload
again.

## Lifecycle

- **Startup:** the server binds first; a taken port fails at once with exit
  code 1 and error code `address_in_use`, whose hint names `--port`. While
  the index and the bucket open, `GET /healthz` answers `200` and
  everything else, `GET /readyz` included, answers `503 not_ready` with
  `Retry-After: 1`. Then `GET /readyz` answers `200`, and the ready line or
  banner is printed.
- **Shutdown:** on `SIGTERM` or `SIGINT` the server stops accepting
  connections, lets in-flight requests finish within `shutdown_grace`, and
  exits 0. A second signal exits at once with code 1. Every acknowledged
  write is already durable, so neither loses data.
- The server speaks HTTP/1.1, which is what stock clients use. Put TLS in
  front of it with a proxy, and set `public_url` to the proxy's URL.

## Example

```toml
[server]
bind = "0.0.0.0:8080"
public_url = "https://cas.example.com"
data_dir = "/var/lib/openxet"
log_format = "json"

[cas]
dedup_visibility = "per-scope"
url_lifetime = "60m"

[auth]
jwt_public_keys = ["/etc/openxet/issuer.pub"]
max_token_lifetime = "1h"

[[auth.static_tokens]]
sha256 = "<from openxet token generate>"
scope = "ci"
mode = "write"
subject = "ci-bot"
```
