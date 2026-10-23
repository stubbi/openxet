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

| `code`  | Exit | When                                                              |
| ------- | ---- | ----------------------------------------------------------------- |
| `usage` | 2    | The arguments don't parse, including an argument that isn't UTF-8. |
| `io`    | 1    | The result couldn't be written to stdout, e.g. a closed pipe.      |

Without `--json`, a usage error is clap's message and usage line, and an
`io` error is one line starting with `error: `.

`--json` counts only as an option: after `--`, or as the value of an option
that takes one, it is an ordinary argument. `openxet --help` and
`openxet --version` print to stdout and exit 0. `openxet` with no command
prints the help to stderr and exits 2, since nothing was run.

Successful output goes to stdout and carries a `schema` field of the form
`openxet.<command>.v1`. For example, `openxet version --json`:

```json
{"schema":"openxet.version.v1","version":"0.0.0","git_commit":"abc1234","protocol_spec":"1.1.0"}
```
