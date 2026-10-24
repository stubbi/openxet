# Exit codes

`openxet` exits with one of these codes. They are stable: scripts and agents
can rely on them.

| Code | Name      | Meaning                                                              |
| ---- | --------- | -------------------------------------------------------------------- |
| 0    | `SUCCESS` | The command did what was asked.                                      |
| 1    | `FAILURE` | The command was valid but failed. The message on stderr says why.    |
| 2    | `USAGE`   | Unknown command, missing argument or invalid option. Nothing was run. |

Every failure is reported on stderr. With `--json`, it is a single JSON
object with a stable `code` and a `hint` naming the next action:

```json
{"schema":"openxet.error.v1","code":"usage","message":"unrecognized subcommand 'nope'","hint":"Run `openxet --help` to list commands and options."}
```

| `code` | Exit | When |
| ------ | ---- | ---- |
| `usage` | 2 | The arguments don't parse, including an argument that isn't UTF-8. |
| `io` | 1 | The result couldn't be written to stdout, e.g. a closed pipe, or a file couldn't be read or written (including a downloaded file). |
| `config` | 1 | The configuration has problems. `openxet config check` lists each one ([config](config.md)). |
| `address_in_use` | 1 | `serve` couldn't listen: another process has the port. The hint names `--port`. |
| `bind` | 1 | `serve` couldn't listen for another reason, such as a privileged port. |
| `data_dir` | 1 | `serve` couldn't create the data directory. |
| `index` | 1 | `serve` couldn't open the index in the data directory. |
| `storage` | 1 | `serve` couldn't open the bucket in the data directory. |
| `insecure_permissions` | 1 | A secret file (`dev-token`, `transfer.key`, a signing key) can be read by other users. The hint names the `chmod`. |
| `invalid_key` | 1 | A key or token file doesn't hold what it should. |
| `exists` | 1 | `token keygen` would overwrite a file. Nothing was written. |
| `no_key` | 1 | `token mint` found no signing key at `--key`, or `serve` found no transfer key at an explicitly set `cas.transfer_key_file`. |
| `invalid_ttl` | 1 | `token mint --ttl` is outside `1m` to `30d`. |
| `invalid_claims` | 1 | `token mint` or `token generate` was given an empty or invalid `--scope` or `--subject`. |
| `key`, `random` | 1 | The operating system's random number generator failed. |
| `signal`, `runtime` | 1 | `serve` couldn't install its signal handlers, or a command couldn't start its runtime. |
| `missing_endpoint` | 2 | `upload` or `download` has no endpoint. The hint names `OPENXET_ENDPOINT`. |
| `missing_token` | 2 | `upload` or `download` has no token. The hint names `OPENXET_TOKEN`. |
| `invalid_endpoint` | 2 | The endpoint is not an `http` or `https` URL, or holds credentials, a query or a fragment. |
| `invalid_input` | 2 | `upload` was given paths that are missing, not regular files, or unreadable. `files` lists each with its `problem`. Nothing was uploaded. |
| `invalid_hash` | 2 | `download` was given a hash that isn't 64 lowercase hex characters. |
| `output_exists` | 2 | `download --output` names an existing file and `--force` wasn't given. Nothing was downloaded. |
| `token_expired` | 1 | The token expires within 30 seconds, or expired during the transfer. Nothing was sent if it was found before. |
| `unreachable_endpoint` | 1 | Nothing answered at the endpoint. |
| `unauthorized` | 1 | The server refused the token (`401`): unknown, revoked or expired. |
| `forbidden` | 1 | The token doesn't allow this (`403`), e.g. a read token uploading. |
| `not_found` | 1 | `download`: no file with this hash in the token's scope. |
| `upload_rejected` | 1 | The server refused the upload. The message is the server's reason, e.g. a stale client cache after a server reset. |
| `server_error` | 1 | The server kept answering with an error status after the client's retries. |
| `transfer_failed` | 1 | The transfer failed for another reason; the message names its kind. |
| `internal` | 1 | A bug. Report it with the log. |

Without `--json`, a usage error is clap's message and usage line, and any
other error is two lines, `error: <message>` and `hint: <hint>`, with one
`  <path>: <problem>` line between them per entry of `files`.

An error object may carry `files`, a list of `{"path", "problem"}` entries,
when a command given several paths finds problems with some of them; it is
absent otherwise.

`openxet serve` exits 0 after a graceful stop on `SIGTERM` or `SIGINT`, and
1 after a second signal, which stops it without waiting for in-flight
requests. `openxet config check` exits 1 when it finds problems; its report
is on stdout and the `config` error on stderr.

`--json` counts only as an option: after `--`, or as the value of an option
that accepts values starting with `-`, it is an ordinary argument. Right
after any other option that takes a value, it is still `--json`, and the
missing value is reported as a JSON usage error. `openxet --help` and
`openxet --version` print to stdout and exit 0. `openxet` with no command
prints the help to stderr and exits 2, since nothing was run.

Successful output goes to stdout and carries a `schema` field of the form
`openxet.<command>.v1`. For example, `openxet version --json`:

```json
{"schema":"openxet.version.v1","version":"0.0.0","git_commit":"abc1234","protocol_spec":"1.1.0"}
```
