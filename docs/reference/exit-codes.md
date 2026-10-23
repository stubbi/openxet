# Exit codes

`openxet` exits with one of these codes. They are stable: scripts and agents
can rely on them.

| Code | Name      | Meaning                                                              |
| ---- | --------- | -------------------------------------------------------------------- |
| 0    | `SUCCESS` | The command did what was asked.                                      |
| 1    | `FAILURE` | The command was valid but failed. The message on stderr says why.    |
| 2    | `USAGE`   | Unknown command, missing argument or invalid option. Nothing was run. |

With `--json`, a usage error is printed to stderr as a single JSON object:

```json
{"schema":"openxet.error.v1","code":"usage","message":"unrecognized subcommand 'nope'","hint":"Run `openxet --help` to list commands and options."}
```

Successful output goes to stdout and carries a `schema` field of the form
`openxet.<command>.v1`. For example, `openxet version --json`:

```json
{"schema":"openxet.version.v1","version":"0.0.0","git_commit":"abc1234","protocol_spec":"1.1.0"}
```
