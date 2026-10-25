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
| File | `[section]` and `key = value` | `[cas]` `url_lifetime = "45m"` |
| Environment | `OPENXET_<SECTION>__<KEY>`, case-insensitive, `__` between section and key and between a subsection and its key | `OPENXET_CAS__URL_LIFETIME=45m`, `OPENXET_GIT__REACHABILITY__MAX_MS=3000` |
| Flags | `--port N` replaces the port of `server.bind`; `--data-dir DIR` and `--log-format text\|json` replace their keys | `--port 0` |

An environment value is taken as written for string keys. For the other
keys it is a TOML value, as in the file: `OPENXET_SERVER__BODY_MIN_RATE=8192`,
`OPENXET_AUTH__JWT_PUBLIC_KEYS='["/etc/openxet/issuer.pub"]'`, or
`OPENXET_AUTH__STATIC_TOKENS='[{sha256 = "…", scope = "ci", mode = "write", subject = "ci"}]'`.
Only variables with `__` are configuration: `OPENXET_ENDPOINT` and
`OPENXET_TOKEN`, which the client commands read, are not. An unknown key in
the file or the environment is a problem, not ignored.

Durations are a whole number and one unit: `s`, `m`, `h` or `d`, such as
`30s`, `60m`, `24h` or `30d`. Sizes are a whole number and one binary
unit: `KiB`, `MiB`, `GiB` or `TiB`, such as `512MiB` or `2GiB`.

## Keys

### `[server]`

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `bind` | `127.0.0.1:8080` | an IP address and port | Where to listen. `0.0.0.0:8080` or `[::]:8080` listens on every interface and then needs `public_url`. `hf_xet`'s default CAS endpoint is `http://localhost:8080`. |
| `public_url` | `http://` and the bound address | absolute `http` or `https` URL of an origin root | The URL clients reach the server at. Every transfer URL is built from it. |
| `data_dir` | `./openxet-data` | a directory | Holds `index.sqlite`, `bucket/`, `transfer.key`, `cas-token.key`, by default the Git cache `git-cache/` and, in dev mode, `dev-token`. Its filesystem must support hard links (ext4, XFS, Btrfs, ZFS, APFS, NTFS; not FAT or exFAT): the bucket writes its records with them, and `serve` refuses to start without them. |
| `log_format` | `text` | `text` or `json` | Log lines on stderr. |
| `log_level` | `info` | `error`, `warn`, `info`, `debug` or `trace` | openxet's own log level. Other libraries log warnings and errors only. `debug` logs one line per request. |
| `header_read_timeout` | `30s` | `1s` to `5m` | How long a connection may take to send a request line and headers. It also closes a keep-alive connection that sits idle this long; clients open a new one. |
| `body_read_timeout` | `300s` | `1s` to `1h` | The longest gap allowed between two parts of a request body. It matches `hf_xet`'s own 300 s read timeout. |
| `body_min_rate` | `4096` | `1` to `1048576` | The least average rate of a request body, in bytes per second, after a first `body_read_timeout`: a body of `n` bytes may take `body_read_timeout + n / body_min_rate`. The default lets a 64 MiB xorb take up to 4.6 hours over a very slow link. |
| `memory_budget` | a quarter of the memory | `256MiB` to `1TiB` | The most memory request data may hold at once: upload bodies being buffered and reconstruction answers waiting to be sent. The default is a quarter of the host's physical memory, or on Linux of the smallest cgroup memory limit over the process if lower, rounded down to a MiB and at least `256MiB`; `1GiB` where the memory can't be read. `config check` shows the value in effect. See [Memory](#memory). |
| `max_connections` | `4096` | `1` to `1000000` | The most connections open at once. Past it, a new connection waits unaccepted in the listen backlog until one closes, then is served. Keep it below the process's open-file limit (`ulimit -n`). |
| `shutdown_grace` | `30s` | `1s` to `10m` | On `SIGTERM` or `SIGINT`, how long in-flight requests get to finish. |

A body that breaks `body_read_timeout` or `body_min_rate` is refused with
`408 body_timeout`, and the connection is closed. This also bounds how long
a refused upload's body is read and discarded before the refusal is sent.

### Memory

The server bounds the memory that requests hold, however many clients
connect, upload slowly or never read their answers (ruling M1-R40).
Before it buffers anything, a request reserves its bytes from
`memory_budget`, and holds them until the work they serve has ended,
even if its client has left:

- An upload (`POST /v1/xorbs/…`, `/v1/shards`, `/v2/shards`) reserves its
  `Content-Length`, or the route's limit (64.5 MiB for a xorb, 64 MiB for
  a shard) if it declares none, until it is stored and registered. A body
  longer than it declared is refused with `400`.
- A reconstruction (`/v1` and `/v2/reconstructions`, and the batch)
  reserves its file's decoded record (52 bytes a term, from the stored
  count) before loading it, then about 600 bytes a term of the answer once
  the range is resolved, then keeps the answer's size (42 MB for 100,000
  terms) until the answer has been sent. An answer not sent within 30 s
  plus 1 s per MiB is cut off, and its share freed.

Requests that don't fit wait for bytes to come free:

- Uploads queue in arrival order, each up to 120 s, their bodies unread.
  `hf_xet` sends up to 64 xorbs at once; past the budget they queue here
  and are read as others finish, well inside the client's 300 s timeout.
  A waiting upload holds only its connection.
- A reconstruction that fits in the free bytes takes them at once, past
  any queued upload, so uploads never hold up downloads. One that doesn't
  fit waits up to 5 s. A batch's own body waits the same way.

Answers that fit go ahead of queued uploads, so under read load that
holds nearly the whole budget, uploads can wait their full 120 s and get
`503`.

After its wait a request is refused with `503 memory_busy` and
`Retry-After: 1`; stock clients retry it. An upload refused so is not
read, and its connection is closed.

Two refusals are final, since no retry could succeed:

- One answer may reserve at most an eighth of the budget. A larger one
  is refused at once with `413 answer_too_large`: ask for the file by
  range, as `hf_xet` always does (at most 8 GB at a time), or put fewer
  files in a batch. A batch holds at most 100,000 terms (about 65 MB).
- An upload larger than the whole budget gets `413 exceeds_memory_budget`.
  Stock clients' uploads (at most 64.5 MiB) never do, since the budget is
  at least 256 MiB.

The budget is per server, not per client: one credential can fill it, up
to an answer's eighth at a time, and so can delay others' requests until
its answers are sent or cut off. Per-credential shares of the budget come
with rate limiting in M3.

Size the budget to leave room for the rest of the process: the index's
cache, the CPU-bound work each permit runs (about 150 MB per dedup answer
being built) and the operating system. The default, a quarter of the
memory, does; a budget over half of it is a warning at startup and in
`config check`. Every refusal is logged at warn as `request refused` with
its code; frequent `memory_busy` means the budget, or the server, is too
small for its load.

**Behind a proxy**, set its timeouts to at least 130 s: a queued upload
waits up to 120 s before its body is read, and a large answer may take as
long to send. With common defaults (nginx's `proxy_read_timeout` and
`proxy_send_timeout`, an AWS load balancer's idle timeout, all 60 s), a
queued upload becomes a `504` at 60 s. Clients retry it, but lose the
queue's place.

`public_url` must be the origin root, with no path, query, fragment or user
info. Clients append `/v1/...` to it and resolve `/v1/telemetry` against
the origin (`docs/research/S10-cas-wire-contract.md`), so the CAS can't
live under a path prefix. Behind a proxy that adds one, give openxet its own
host name instead. A trailing `/` is allowed and ignored.

### `[cas]`

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `dedup_visibility` | `global` | `global` or `per-tenant` | Whether one tenant's xorbs are visible to another. See [Dedup visibility](#dedup-visibility). |
| `url_lifetime` | `60m` | `30m` to `60m` | How long a signed transfer URL is valid. See [Transfer URLs](#transfer-urls). |
| `heartbeat_interval` | `5s` | `1s` to `2m` | How often a `/v2/shards` upload stream repeats its progress while the upload is verified and committed. `hf_xet` fails a stream that is silent for 300 s. |
| `transfer_key_file` | `<data_dir>/transfer.key` | a file path | The key that signs transfer URLs. Set explicitly, the file must exist. See [Keys and tokens on disk](#keys-and-tokens-on-disk). |

### `[auth]`

Any `[auth]` section, even an empty one, turns dev mode off. It must then
configure at least one credential.

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `static_tokens` | none | an array of tables | Long-lived tokens, by hash. See [Static tokens](#static-tokens). |
| `rules` | none | an array of tables | What user credentials may do per repository. See [Rules](#rules). |
| `jwt_public_keys` | none | an array of strings | Ed25519 public keys trusted to sign CAS tokens: a file path, or the PEM itself. See [CAS tokens](#cas-tokens). |
| `max_token_lifetime` | `24h` | `1m` to `30d` | A CAS token whose `exp` lies further ahead than this is refused. |

Every request needs a credential: there are no anonymous reads unless a
[rule](#rules) for `anonymous` grants them on a repository. The other
exceptions are the probes, `GET /healthz` and `GET /readyz`, which answer
a fixed body that says nothing about stored data.

### `[hub]`

The Hub API, which `huggingface_hub` and the `hf` CLI call, is served at
the root of the same origin as the CAS.

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `public_url` | `server.public_url` | absolute `http` or `https` URL of an origin root | The URL clients reach the Hub API at: their `HF_ENDPOINT`. Every repository URL the Hub answers is built from it, and `huggingface_hub` parses those against its endpoint, so it must be the one clients use. |
| `cas_url` | `server.public_url` | absolute `http` or `https` URL of an origin root | The CAS URL the Hub's tokens name. Set it only when clients reach the CAS at another URL than the server's. |
| `lfs_threshold` | `1MiB` | `1KiB` to `10MiB` | Files at least this large are uploaded through Xet; smaller ones travel inside the commit as Git blobs. |
| `max_commit_bytes` | `64MiB` | `1MiB` to `1GiB`, and at least twice `lfs_threshold` | The largest commit body. A regular file travels base64-encoded inside it. |
| `token_ttl` | `15m` | `1m` to `24h`, and at most `auth.max_token_lifetime` | How long the CAS tokens the Hub hands clients live. Clients refresh them before they expire. |
| `page_size` | `1000` | `1` to `1000` | The most entries one listing page holds. A listing with more answers `Link: <…>; rel="next"`, which `huggingface_hub` follows. |
| `commit_wait` | `30s` | `1s` to `10m` | How long a Hub commit waits for the commits to the same branch already running on this node. One that waits longer is refused as retryable (`503` with `Retry-After`), and `huggingface_hub` retries it. |

A repository the caller may not read answers exactly as one that doesn't
exist: `404` with `X-Error-Code: RepoNotFound`. A request without a
credential gets `401` instead, unless a [rule](#rules) for `anonymous`
grants the read.

### `[git]`

The Git engine behind the Hub's commits and reads and Git smart HTTP. It
keeps a node-local cache of packs hydrated from the bucket, and runs
`git` 2.37 or newer from `PATH`.

Git is served over smart HTTP at the Hub's URL: `{hub.public_url}/{namespace}/{name}`,
with or without `.git`, and under `datasets/`, `spaces/` or `kernels/` for
a repository with that label. git sends the token as the password of HTTP
Basic (any user name), or as a `Bearer` header with
`http.extraHeader`. The first push to a missing repository creates it,
with the `model` label or the prefix's, when the caller may create in the
namespace and read and write the repository: its advertisement shows an
empty repository and creates nothing, and its upload creates it. Errors
come as plain text, which git shows as `remote:` lines. The two `POST`
routes take only git's request types (`application/x-git-upload-pack-request`,
`application/x-git-receive-pack-request`) and answer anything else `415`,
as `git http-backend` does, so a web page can't use credentials a browser
cached for the server. The dumb HTTP protocol is not served.

Every 10 seconds `serve` copies new ref changes to the bucket's ref log,
which keeps the bucket's copy within a minute of the index (design §5.3),
and repacks a repository holding more than 50 packs.

The cache is as private as the data directory: its directories are mode
`0700` (a root you create wider is narrowed at start), its files `0600`,
and cached pack files `0400`.

git runs as a child of `serve`, so a descriptor `serve` itself inherited
from whatever started it without `FD_CLOEXEC` (an open log file, a
socket) reaches every git child too. Start `serve` from a supervisor,
such as systemd, that passes it only standard input, output and error.
This is about the parent's descriptors; how openxet's own per-push hook
channel is kept to its git child, and its one platform limitation, is in
[ADR 0004 item 7](../adr/0004-git-hosting-engine.md).

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `cache_dir` | `<data_dir>/git-cache` | a directory path | The node's Git cache: hydrated packs, per-request snapshots and the `proc-receive` hook (a link to the openxet binary). It must be on a filesystem with hard links. One process holds it at a time: a second `serve` given the same directory refuses to start, naming the key, so servers that share a data directory each need their own `cache_dir`. |
| `cache_max_bytes` | `20GiB` | `1GiB` to `1PiB` | The cache is evicted down to this size; an evicted pack is hydrated from the bucket again when a request needs it. |
| `max_push_bytes` | `10GiB` | `1MiB` to `1TiB` | The largest pack one `git push` may send, counted after HTTP compression is undone. A push whose `Content-Length` is larger is refused with `413` before it is read; a longer streamed push is refused by git ("pack exceeds maximum allowed size"). Store large files with Git LFS instead. |
| `max_plain_blob_bytes` | unset | `1KiB` to `1TiB` | A pushed blob larger than this that is not a Git LFS pointer is refused, and the message names `git lfs track`. Unset, any size is accepted. |

#### `[git.reachability]`

Limits on the reachability check that a fetch asking for an object by its
id runs, so one request can't walk a whole history. A request over a limit
is refused as retryable.

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `max_commits` | `200000` | `1` or more | The most commits one check may walk. |
| `max_ms` | `2000` | `1` or more | The most milliseconds one check may take. |
| `max_concurrent_per_repo` | `2` | `1` or more | The most checks running at once in one repository. |
| `max_concurrent_per_principal` | `4` | `1` or more | The most checks one caller may run at once. |

#### `[git.thin_bases]`

Limits on the check a push's thin pack goes through: every base object it
leaves out must be reachable from a ref the pusher may read. A push over a
limit is refused as retryable. The check shares
`[git.reachability]`'s concurrency limits.

| Key | Default | Bounds | Meaning |
| --- | ------- | ------ | ------- |
| `max_objects` | `2000000` | `1` or more | The most objects one check may visit. |
| `max_ms` | `2000` | `1` or more | The most milliseconds one check may take. |

## Dev mode

Without a configuration file, or with one that has no `[auth]` section,
`openxet serve` creates in `data_dir`, on first start:

- `index.sqlite` and `bucket/`, the data (see [Keys and tokens on disk](#keys-and-tokens-on-disk) for modes)
- `dev-token` (mode `0600`): one static write token for scope `default`,
  which is also the user `dev`; dev mode's one [rule](#rules) lets `dev`
  read, write and create every repository
- `transfer.key` and `cas-token.key` (mode `0600`)
- `git-cache/`, the Git cache, unless `git.cache_dir` names another place

It reuses all of them on every later start, so data, the token and transfer
URLs signed before a restart keep working. It prints the endpoint, the data
directory, the token, ready-to-paste upload commands and the `HF_ENDPOINT`
and `hf auth login --token …` lines for the `hf` CLI, and `git push` and
`git clone` lines, on stdout. With
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

`cas-token.key` is an Ed25519 private key in PKCS#8 PEM, the format of
`openxet token keygen`. The Hub signs the [CAS tokens](#cas-tokens) it
hands clients (`xet-read-token` and `xet-write-token`) with it, and the
CAS trusts its public key without it being listed in
`auth.jwt_public_keys`. Like `transfer.key`, every node behind one
endpoint must share it, or a token one node's Hub hands out is refused by
another node's CAS. A missing key is generated, silently in dev mode and
with a warning naming the file when `[auth]` is configured. Replacing it
invalidates the tokens already handed out; clients fetch new ones.

## Token format

Tokens openxet generates (the dev token and `openxet token generate`) are
`oxt_` followed by 43 base64url characters: 32 bytes from the operating
system's random number generator, unpadded. The fixed prefix lets secret
scanners find a token committed by accident; this repository's own scan
(`compat/tests/test_fixture_redaction.py`) matches `oxt_[A-Za-z0-9_-]{43}`.

## Static tokens

A static token is configured by the SHA-256 of its UTF-8 bytes, never by
the token itself. It is a CAS credential, a user credential, or both
(ADR 0012):

```toml
[[auth.static_tokens]]
sha256 = "<64 lowercase hex characters>"
subject = "ci-bot"  # who the token acts for
scope = "ci"        # a CAS credential for this scope
mode = "write"      # or "read"; required with scope
tenant = "ci"       # optional; `scope:` and the scope when left out

[[auth.static_tokens]]
sha256 = "<64 lowercase hex characters>"
subject = "alice"                      # no scope: a user credential
name = "Alice Martin"                  # optional; whoami's name, the subject when left out
email = "alice@example.com"            # optional; the author of her Hub commits
claims = { orgs = ["acme"], groups = ["ml-team"] }  # optional; string lists that rules match
```

- **CAS credential:** a token with `scope` may use the CAS routes within
  that scope, as `mode` allows: `write` may upload; `read` may download
  and query. `tenant` names the [tenant](#tenants) the scope belongs to.
- **User credential:** a token without `scope` acts for its subject on
  the Hub API, Git and LFS routes, where [rules](#rules) decide what it
  may do per repository. It gets CAS access per repository from the Hub's
  token routes, as `huggingface_hub` and `git-xet` do; presented to a CAS
  route itself it answers `403 insufficient_scope`. `mode` and `tenant`
  are then refused, since there is no scope for them to apply to.
- **Both:** a token with `scope` and any of `name`, `email` or `claims` is
  both credentials, for the same subject. The dev token is one.

`openxet token generate --scope ci --mode write --subject ci-bot` prints a
new CAS token and its entry; for a user token, leave out `scope` and `mode`
from that entry. Give each token its own `subject`, so the audit
log tells them apart. A presented token is hashed and compared against
every configured hash in constant time. `scope` and `subject` must not be
empty or contain control characters, and `scope` is at most 1024 bytes.
A scope may not start with `repo:`, a prefix reserved for repositories;
pick another scope. The subject of a user credential may not be
`anonymous`, which names requests without a credential. `name`, `email`,
claim names and claim values may not contain control characters.
`tenant` follows the same rules as `scope` and may not start with
`scope:`; without it the tenant is `scope:` and the scope.

## Rules

Rules decide what a user credential may do on the Hub API, Git and LFS
routes: `read` a repository, `write` it (commit, push, change refs and
labels; `write` includes `read`), or `create` repositories in a namespace.
CAS routes don't consult them: a CAS credential's scope and mode decide
there.

```toml
[[auth.rules]]
subjects = ["alice"]            # these subjects
claims = { groups = ["ml-team"] }  # or anyone holding one of these values under that claim
repos = ["acme/*"]              # these repositories
actions = ["read", "write", "create"]

[[auth.rules]]
subjects = ["anonymous"]        # everyone, with or without a credential
repos = ["acme/public-model"]
actions = ["read"]
```

- A request is allowed when any rule applies to the caller, covers the
  repository and lists the action; with no such rule it is denied. Rules
  only grant: their order doesn't matter and there is no deny rule.
- A rule applies to a caller whose subject is in `subjects`, or who holds,
  under any claim name in `claims`, any of the values listed for it. It
  needs at least one subject or claim value. Subjects match exactly; `*`
  is not a wildcard there.
- `repos` are patterns: `*` alone covers every repository, and
  `{namespace}/{name}` with `*` inside a part matches any characters
  except `/`, so `acme/*` covers every repository in `acme`, `acme/llama-*`
  the ones whose names start with `llama-`, and `a*/x` the repository `x`
  in every namespace starting with `a`. `**` is refused. Matching ignores
  ASCII case, as repository names do.
- `create` on `acme/*` (or `*`) allows creating repositories in the
  namespace `acme`. Creation is decided per namespace, before the name is
  looked at, so a pattern such as `acme/llama-*` never grants it.
- `anonymous` is the subject of a request without a credential. A rule
  naming it applies to everyone, so a repository it makes readable is
  readable with any credential too. No rule names it unless you add one.
- A repository the caller may not read answers exactly as one that
  doesn't exist.

Without `[auth]`, in [dev mode](#dev-mode), the one rule is
`subjects = ["dev"], repos = ["*"], actions = ["read", "write", "create"]`.
With `[auth]` and no rules, user credentials may do nothing.

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
  `iat` (Unix seconds), plus an optional `nbf` and an optional `tenant`
  (a string naming the [tenant](#tenants) the scope belongs to; without it
  the tenant is `scope:` and the scope). Any other claim, such as `aud`,
  `iss` or `jti`, makes the token invalid, since openxet can't honour it. `sub`,
  `scope` and `tenant` follow the static-token rules above.
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
before its last use (ADR 0002), and at most 60 minutes (ruling M1-R42), so
a leaked URL, or one issued before data is purged, stops working within the
hour. A URL whose `exp` lies further ahead than this server's
`url_lifetime` (plus 5 minutes of clock skew) is refused, so shortening
`url_lifetime` on a live server invalidates outstanding URLs signed under
the longer lifetime. Clients recover by requesting a fresh reconstruction,
which `hf_xet` does on the `403` by itself.

## Tenants

Every credential names a **scope**, the unit of access: files are read
and registered per scope. It also names a **tenant**, the unit of
isolation: under [`per-tenant` visibility](#dedup-visibility) the scopes
of one tenant share xorbs and dedup, and different tenants don't. A CAS
token names its tenant in the optional `tenant` claim and a static token
in its optional `tenant` field. Without one, the tenant is `scope:` followed
by the scope, so each such scope is a tenant of its own. No explicit tenant
may start with `scope:` (ruling M1-R43): a scope named like a tenant, say
`org`, never lands in tenant `org`, whether or not its tokens name one. A
platform whose customers each own several
scopes (repos, projects) gives all of a customer's tokens that customer's
tenant, so a fine-tune uploaded to one project dedups against the base
model uploaded to another.

## Dedup visibility

Files are always visible only in the scope they were uploaded to.
`dedup_visibility` decides xorbs, the deduplicated chunk containers,
between [tenants](#tenants):

- `global` (the default): every stored xorb is visible to every tenant. An
  upload skips any chunk stored by anyone, which saves the most space and
  bandwidth.

  One dedup match offers the client every xorb of the latest upload
  (shard) that stored the matched chunk, up to 16 MiB of answer. So a
  client with an empty cache that re-uploads an edited file sends little
  more than the edit: about 1.1 MB for a 1 MiB edit of a 100 MiB file.
  The price is what one matching chunk reveals: anyone who holds one
  chunk of an upload learns that upload's xorb hashes, chunk counts and
  chunk lengths (never its data or raw chunk hashes); choose `per-tenant`
  if tenants must not learn that about each other.
- `per-tenant`: a tenant sees only the xorbs it owns: those uploaded with
  its credentials, in any of its scopes, and those its registered files
  reference. To any other tenant such a xorb reads as absent: `HEAD` of it
  answers `404`, dedup queries find no match in it, and a shard that
  references it is refused with `unknown_xorb`. A dedup answer offers only
  xorbs of the tenant's own uploads. So no tenant can learn what another
  has stored, and each tenant uploads its own copy of shared data (stored
  once in the bucket, all the same), while the scopes of one tenant
  deduplicate against each other.

Switching a running deployment from `global` to `per-tenant` keeps
everything working: every file stays readable in its scope, and a tenant
keeps owning the xorbs its files reference even if another tenant uploaded
them, so its uploads keep deduplicating against them. Switching back loses
nothing either.

What a tenant keeps covers whole xorbs, not just the chunks it used. A
file that referenced even one chunk of another tenant's xorb makes the
whole xorb its tenant's: after the switch, that tenant still gets `HEAD`
`200` and dedup matches for its dedup-eligible chunks. It never gets another
tenant's raw chunk hashes, and under `global` it could see all of this
anyway. An operator who needs tenants isolated from their first upload
starts on `per-tenant`.

Dedup answers get their own share of the server, a quarter of its CPUs (at
least one answer at a time). A query that waits 1 s for its turn answers
`429 dedup_busy`, which `hf_xet` reads as no match: the upload goes on and
sends those chunks. An answer not sent within its deadline (30 s plus 1 s
per MiB) is cut off and its turn freed. At `log_level = "debug"` both are
logged, the first as `request refused` with code `dedup_busy`, the second
as `an answer missed its send deadline`. Frequent `dedup_busy`
means the server needs more CPUs.

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
- **Readiness:** from then on `GET /readyz` answers `200` only while the
  index answers a read and the bucket takes a write, and `503 unavailable`
  naming `index` or `storage` otherwise; the cause is logged at error
  level ([routes](routes.md#get-healthz-get-readyz)). Point the load
  balancer's or orchestrator's readiness check at it, and the liveness
  check at `GET /healthz`, so a node with a full disk leaves the rotation
  without being restarted.
- **Shutdown:** on `SIGTERM` or `SIGINT` the server stops accepting
  connections, lets in-flight requests finish within `shutdown_grace`, and
  exits 0. A second signal exits at once with code 1. Every acknowledged
  write is already durable, so neither loses data.
- **Connections:** at most `max_connections` are open at once. The next
  one is not accepted until one closes: it waits in the kernel's listen
  backlog with its request unread, and is served then. Refusing it
  instead would cost the file descriptor the cap protects. A client whose
  connect attempt times out meanwhile retries it. The cap is for the
  whole server, not per client: one client can hold many slots, for as
  long as its bodies keep `body_min_rate`. Per-client and per-credential
  limits come with rate limiting in M3.
- The server speaks HTTP/1.1, which is what stock clients use. Put TLS in
  front of it with a proxy, and set `public_url` to the proxy's URL.
  Choose a proxy that negotiates the hybrid post-quantum key exchange
  `X25519MLKEM768` ([ADR 0011](../adr/0011-post-quantum-readiness.md)):
  one built on OpenSSL 3.5 or later, or on Go 1.24 or later, does by
  default unless its config overrides the key exchange groups (for
  nginx, `ssl_ecdh_curve`).

## Example

```toml
[server]
bind = "0.0.0.0:8080"
public_url = "https://cas.example.com"
data_dir = "/var/lib/openxet"
log_format = "json"

[cas]
dedup_visibility = "per-tenant"
url_lifetime = "60m"

[auth]
jwt_public_keys = ["/etc/openxet/issuer.pub"]
max_token_lifetime = "1h"

[[auth.static_tokens]]
sha256 = "<from openxet token generate>"
scope = "ci"
mode = "write"
subject = "ci-bot"

[[auth.static_tokens]]
sha256 = "<from openxet token generate>"
subject = "alice"
claims = { groups = ["ml-team"] }

[[auth.rules]]
claims = { groups = ["ml-team"] }
repos = ["acme/*"]
actions = ["read", "write", "create"]

[hub]
lfs_threshold = "4MiB"
```
