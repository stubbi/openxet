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
| `io` | 1 | The result couldn't be written to stdout, e.g. a closed pipe, or a file couldn't be read or written. |
| `config` | 1 | The configuration has problems. `openxet config check` lists each one ([config](config.md)). |
| `address_in_use` | 1 | `serve` couldn't listen: another process has the port. The hint names `--port`. |
| `bind` | 1 | `serve` couldn't listen for another reason, such as a privileged port. |
| `data_dir` | 1 | `serve` couldn't create the data directory. |
| `index` | 1 | `serve` couldn't open the index in the data directory. |
| `storage` | 1 | `serve` couldn't open the bucket in the data directory. |
| `insecure_permissions` | 1 | A secret file (`dev-token`, `transfer.key`, a signing key) can be read by other users. The hint names the `chmod`. |
| `invalid_key` | 1 | A key or token file doesn't hold what it should. |
| `exists` | 1 | `token keygen` would overwrite a file. Nothing was written. |
| `no_key` | 1 | `token mint` found no signing key at `--key`. |
| `invalid_ttl` | 1 | `token mint --ttl` is outside `1m` to `30d`. |
| `invalid_claims` | 1 | `token mint` or `token generate` was given an empty or invalid `--scope` or `--subject`. |
| `key`, `random` | 1 | The operating system's random number generator failed. |
| `signal`, `runtime` | 1 | `serve` couldn't install its signal handlers or start its runtime. |
| `internal` | 1 | A bug. Report it with the log. |

Without `--json`, a usage error is clap's message and usage line, and any
other error is two lines, `error: <message>` and `hint: <hint>`.

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
