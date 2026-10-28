# Exit codes

The `openxet` command exits with one of these codes. They are stable: scripts and agents
can rely on them.

<!-- table-caption: Process exit codes -->

| Code | Name      | Meaning                                                              |
| ---- | --------- | -------------------------------------------------------------------- |
| 0    | `SUCCESS` | The command did what was asked.                                      |
| 1    | `FAILURE` | The command was valid but failed. The message on stderr says why.    |
| 2    | `USAGE`   | Unknown command, missing argument or invalid option, or a refusal to overwrite a file. Nothing was run or written. |
| 130  | `INTERRUPTED` | `upload` or `download` was stopped by `SIGINT` (Ctrl-C) or `SIGTERM`. A partial download was removed. |

Every failure is reported on stderr. With `--json`, it is a single JSON
object, on the last line of stderr, with a stable `code` and a `hint`
naming the next action. Only `serve` writes anything before it: its log
lines.

```json
{"schema":"openxet.error.v1","code":"usage","message":"unrecognized subcommand 'nope'","hint":"Run `openxet --help` to list commands and options."}
```

<!-- table-caption: JSON error codes and exit statuses -->

| `code` | Exit | When |
| ------ | ---- | ---- |
| `usage` | 2 | The arguments don't parse, including an argument that isn't UTF-8. |
| `io` | 1 | The result couldn't be written to stdout, e.g. a closed pipe, or a file couldn't be read or written (including a downloaded file). |
| `config` | 1 | The configuration has problems. `openxet config check` lists each one ([config](config.md)). From a `workspace` command: the repository's `.openxet/workspace.toml` or your `~/.config/openxet/workspace.toml` is invalid. |
| `address_in_use` | 1 | `serve` couldn't listen: another process has the port. The hint names `--port`. |
| `bind` | 1 | `serve` couldn't listen for another reason, such as a privileged port. |
| `data_dir` | 1 | `serve` couldn't create the data directory. |
| `index` | 1 | `serve` or `purge` couldn't open or write the index in the data directory, or `purge` found none there (the hint names `--config` and `--data-dir`). |
| `storage` | 1 | `serve` or `purge` couldn't open the bucket in the data directory, or `purge` couldn't write its purge record there. Nothing was purged. |
| `insecure_permissions` | 1 | A secret file (`dev-token`, `transfer.key`, a signing key) can be read by other users. The hint names the `chmod`. |
| `invalid_key` | 1 | A key or token file doesn't hold what it should. |
| `exists` | 2 | `token keygen` would overwrite a file, or `workspace create` names a path or branch that exists. Nothing was written. |
| `invalid_name` | 2 | `workspace create --name` isn't a slug of lowercase letters, digits and single hyphens of at most 64 bytes. |
| `unknown_revision` | 2 | `workspace create --base` names no commit of the repository. |
| `not_a_repository` | 1 | `workspace create` or `list --repo` was run outside a Git repository. The hint names `--repo`. |
| `not_a_workspace` | 1 | `workspace remove` was given a path no workspace is registered at. The hint names `workspace list`. |
| `unsaved_work` | 1 | `workspace remove` would lose work that exists nowhere else. `files` lists each item with its `problem`: `uncommitted`, `untracked`, `ignored` or `unpushed` (its `path` is the branch). Nothing was removed; the hint names `--discard`. |
| `disk_budget` | 1 | `workspace create` found free space under the floor even after evicting idle workspaces' build outputs. The message gives both in bytes; nothing was created. The hint says to retry in a minute or remove idle workspaces. |
| `registry` | 1 | The workspace registry couldn't be opened, migrated or read. |
| `git` | 1 | A Git command a `workspace` command ran failed; the message is Git's. |
| `no_key` | 1 | `token mint` found no signing key at `--key`, or `serve` found no transfer key at an explicitly set `cas.transfer_key_file`. |
| `invalid_ttl` | 1 | `token mint --ttl` is outside `1m` to `30d`. |
| `invalid_claims` | 1 | `token mint` or `token generate` was given an empty or invalid `--scope` or `--subject`. |
| `key`, `random` | 1 | The operating system's random number generator failed. |
| `signal`, `runtime` | 1 | `serve` couldn't install its signal handlers, or a command couldn't start its runtime. |
| `missing_endpoint` | 2 | `upload`, `download` or `label` has no endpoint. The hint names `OPENXET_ENDPOINT`. |
| `missing_token` | 2 | `upload`, `download` or `label` has no token. The hint names `OPENXET_TOKEN`. |
| `invalid_endpoint` | 2 | The endpoint is not an `http` or `https` URL, or holds credentials, a query or a fragment. |
| `invalid_input` | 2 | `upload` was given paths that are missing, not regular files, or unreadable. `files` lists each with its `problem`. Nothing was uploaded. |
| `invalid_hash` | 2 | `download` or `purge` was given a hash that isn't 64 lowercase hex characters. |
| `output_exists` | 2 | `download --output` names an existing file and `--force` wasn't given. Nothing was downloaded. |
| `token_expired` | 1 | The token expires within 30 seconds, or expired during the transfer. Nothing was sent if it was found before. |
| `unreachable_endpoint` | 1 | Nothing answered at the endpoint, or at the proxy in front of it (named in the message). |
| `confirmation_required` | 2 | `purge` without `--yes`, with `--json` or without a terminal to ask on. Nothing was purged. |
| `declined` | 1 | `purge` asked on the terminal and the answer wasn't `yes`. Nothing was purged. |
| `no_actor`, `invalid_actor` | 2 | `purge` couldn't tell who is running it (none of `USER`, `LOGNAME`, `USERNAME` is set), or `--actor` is empty, longer than 256 bytes, or holds a control character. The hint names `--actor`. |
| `invalid_repo` | 2 (1 from the server) | `label` was given a repository that isn't `namespace/name` with a valid namespace and name. |
| `invalid_label` | 2 (1 from the server) | `label` was given a label that isn't a lowercase slug of 1 to 64 bytes, or `add` would give a repository more than 32 labels. |
| `repo_not_found` | 1 | `label`: no such repository, or the token may not read it; the two answer alike. |
| `labels_changed` | 1 | `label add` or `remove` lost to concurrent label changes three times in a row, 50 to 200 ms apart. Nothing was changed; run it again. |
| `unauthorized` | 1 | The server refused the token (`401`): unknown, revoked or expired. |
| `forbidden` | 1 | The token doesn't allow this (`403`), e.g. a read token uploading, a token without `write` changing labels, or a CAS token used with `label`. |
| `not_found` | 1 | `download`: no file with this hash in the token's scope. `purge`: no scope holds the hash and no purge recorded it; nothing was written. |
| `no_clobber_unsupported` | 1 | `download` without `--force` on a filesystem that can neither rename without replacing nor hard-link. Nothing was written. The hint names `--force`. |
| `size_mismatch` | 1 | `download --size` doesn't match the file's size: the size or the hash is wrong. Nothing was written. |
| `interrupted` | 130 | `upload` or `download` was stopped by a signal. The message says what is known: a download wrote nothing to its output; an upload may or may not have been registered, and re-running it is safe. |
| `upload_rejected` | 1 | The server refused the upload's shard. The message is the server's reason, without control characters, e.g. a stale client cache after a server reset. |
| `server_error` | 1 | The server kept answering with an error status after the client's retries, or `label` got an answer it doesn't recognise; the message and hint are the server's where it sent them. |
| `transfer_failed` | 1 | The transfer failed for another reason; the message names its kind. |
| `internal` | 1 | A bug, such as a panic. Its details are never printed, since they may hold request data. Report it with the command you ran or the log. |

Without `--json`, a usage error is clap's message and usage line, and any
other error is two lines, `error: <message>` and `hint: <hint>`, with one
`  <path>: <problem>` line between them per entry of `files`.

An error object may carry `files`, a list of `{"path", "problem"}` entries,
when a command given several paths finds problems with some of them; it is
absent otherwise.

The `openxet serve` command exits 0 after a graceful stop on `SIGTERM` or `SIGINT`, and
1 after a second signal, which stops it without waiting for in-flight
requests. The `openxet config check` command exits 1 when it finds problems; its report
is on stdout and the `config` error on stderr.

`--json` counts only as an option: after `--`, or as the value of an option
that accepts values starting with `-`, it is an ordinary argument. Right
after any other option that takes a value, it is still `--json`, and the
missing value is reported as a JSON usage error. Both `openxet --help` and
`openxet --version` print to stdout and exit 0. Run with no command,
`openxet` prints the help to stderr and exits 2, since nothing was run.

Successful output goes to stdout and carries a `schema` field of the form
`openxet.<command>.v1`. For example, `openxet version --json`:

```json
{"schema":"openxet.version.v1","version":"0.0.0","git_commit":"abc1234","protocol_spec":"1.1.0"}
```
