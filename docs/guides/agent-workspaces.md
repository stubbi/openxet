# Agent workspaces

<!-- description: Give each coding agent its own Git worktree whose build outputs start as copy-on-write clones of a sibling's: install the Claude Code hooks, set a free-space floor, and know what is evicted and what never is. -->

Several agents working in one repository each need their own checkout,
and each checkout builds its own `target/` or `node_modules`. On one
measured Cargo project that is 2.7 GiB per checkout before anyone edits a
line. `openxet workspace` makes each agent's checkout a Git worktree on
its own branch whose build outputs start as copy-on-write clones of a
sibling's, so a new workspace shares its disk blocks until it changes
them. A per-user daemon keeps a free-space floor by evicting idle
workspaces' build outputs, and never touches your work.

It runs on your machine only and needs no OpenXet server. Every command
takes `--json` and `--dry-run` where it changes something; the
[command line reference](../reference/cli.md#openxet-workspace) has each
option and output field, and [exit codes](../reference/exit-codes.md)
each error code.

The commands below run in CI, in order, in a scratch repository with a
pushed commit and a built `target/`. Run them from the top of a
repository of yours to follow along; the last ones change
`~/.config/openxet/workspace.toml`. Until there is a release, install
`openxet` from source (Rust 1.97 or newer):

```bash install
cargo install --locked --git https://github.com/stubbi/openxet openxet
```

## For agents: install once, then nothing

Claude Code runs a `WorktreeCreate` hook in place of its own `git worktree
add` when one is configured, and a `WorktreeRemove` hook when it removes a
worktree. Install OpenXet's two hooks for every repository:

```bash
openxet workspace install claude --user
```

That merges them into `~/.claude/settings.json`, keeping every other key
and hook. From then on, every worktree Claude Code makes (`claude
--worktree`, the `EnterWorktree` tool, subagents with `isolation:
"worktree"`) is an OpenXet workspace, and the agent does nothing
different. Use `--project` instead to put the hooks in the repository's
`.claude/settings.json` for everyone who clones it, and `--dry-run` to
see the merged file first. If another tool already has a worktree hook
there, the install stops with `hook_conflict` and writes nothing.

This is what Claude Code runs, by hand. `claude-create` reads the hook's
JSON on stdin and prints only the new worktree's path; `claude-remove`
removes it:

```bash
path=$(printf '{"cwd": "%s", "name": "Try Hooks"}' "$PWD" | openxet workspace hook claude-create)
echo "$path"
printf '{"worktree_path": "%s"}' "$path" | openxet workspace hook claude-remove
```

The name became the slug `try-hooks`, which is also the branch.
`claude-remove` refuses, exits 1 and leaves the directory when the
workspace holds unsaved work (see [Removing a
workspace](#removing-a-workspace)), so Claude Code keeps it.

Claude Code does not run the remove hook when a subagent finishes, so a
subagent's workspace stays until it is removed. Its build outputs don't:
the daemon evicts them once it is idle (see [What is evicted](#what-is-evicted-and-what-never-is)).

Other agents, and scripts, use the same commands directly. MCP tools for
them come with the MCP server, `openxet mcp`
([#129](https://github.com/stubbi/openxet/issues/129)).

## Creating and listing workspaces

```bash
ws=$(openxet workspace create --name fix-login --json | python3 -c 'import json, sys; print(json.load(sys.stdin)["path"])')
openxet workspace list
```

`create` adds a worktree on a new branch `fix-login` from `HEAD` (or
`--base`). Without `--path` it goes to `.claude/worktrees/fix-login` when
the repository has a `.claude` directory, else beside the repository in
`<repo>-worktrees/fix-login`. Its `target/`, `node_modules` and the
directories your `.openxet/workspace.toml` lists start as clones of the
nearest sibling's: the registered workspace or main worktree closest in
history to the base. Files keep their mtimes, and tracked files the
sibling has unchanged get the sibling's mtimes, so Cargo finds the cloned
outputs fresh instead of rebuilding them. `.venv` is never cloned, since
a virtual environment holds its own absolute path.

`create` starts the daemon in the background when none runs. Set
`OPENXET_WORKSPACE_DAEMON=off` where something else manages disk space.

## Removing a workspace

`remove` deletes the worktree, its branch and its registration, unless
that would lose work that exists nowhere else: changed tracked files,
untracked files, ignored files outside the build output directories
(results, a local `.env`), and commits no remote branch, tag or other
local branch has. Then it refuses, exits 1 with `unsaved_work`, and lists
each item:

```bash
echo "draft" > "$ws/notes.md"
```

```bash fails
openxet workspace remove "$ws"
```

Commit and push the work, or pass `--discard` to delete it with the
workspace:

```bash
openxet workspace remove "$ws" --discard
```

`remove --dry-run` reports what it would remove and free, and refuses
just as `remove` would.

## Worktrees made without OpenXet

A worktree made by `git worktree add` in a shell, by another tool, or by
Claude Code before the hooks were installed is not registered, so nothing
evicts its build outputs. `adopt --scan` registers every linked worktree
of the repository; `adopt PATH` registers one:

```bash
git worktree add -b by-hand ../by-hand
openxet workspace adopt --scan
openxet workspace remove ../by-hand
```

Both are safe to run again: a registered worktree is reported as
`already`. The main worktree, your own checkout, is never adopted. An
adopted workspace's branch may be yours, so `remove` keeps it.

**Cursor** runs the commands in `.cursor/worktrees.json` inside each
worktree it creates ([Cursor
docs](https://cursor.com/docs/configuration/worktrees)). Add the adoption
as a setup step:

```json
{
  "setup-worktree-unix": ["openxet workspace adopt ."]
}
```

**Codex** runs a local environment's setup script when it creates a
worktree for a new chat ([Codex
docs](https://developers.openai.com/codex/app/local-environments)). Its
docs don't say which directory the script runs in, so add `openxet
workspace adopt --scan`, which works from any worktree of the repository,
to the setup script in the app's local environment settings.

## For humans: set the limits, read the status

The daemon keeps free space on each volume with a workspace above a
floor, by default the larger of 20 GiB and a tenth of the volume, and
evicts the build outputs of workspaces idle for `idle_ttl`, by default 24
hours. Set your own in `~/.config/openxet/workspace.toml` (or
`$XDG_CONFIG_HOME/openxet/workspace.toml`):

```bash
mkdir -p ~/.config/openxet
cat > ~/.config/openxet/workspace.toml <<'EOF'
floor = "50 GiB"
idle_ttl = "12h"
EOF
openxet workspace status
```

`floor` is a number of bytes or a whole number with `B`, `KiB`, `MiB`,
`GiB` or `TiB`; `idle_ttl` a whole number with `s`, `m`, `h` or `d`. Any
other key or value fails `create`, `status` and the daemon with `config`,
naming the file. A running daemon keeps the limits it started with; stop
it (`pkill -f 'openxet workspace daemon'`) and the next `create` starts it
with the new ones.

`status` reports, for the volume the current repository is on, the free
space and the floor, whether files can be cloned there, whether the
daemon runs, how many workspaces are registered, and how many bytes the
daemon has replaced by clones of identical files. When free space is
under the floor even after evicting, `create` fails with `disk_budget`
and creates nothing, so agents back off instead of filling the disk.

## What is evicted and what never is

Every path in a workspace is durable or reconstructible.

- **Reconstructible:** what Git ignores under `target`, `node_modules`
  and `.venv` at the workspace root, and under the directories a
  repository lists in `.openxet/workspace.toml`
  (`reconstructible = ["build", "web/dist"]`). Losing it costs a rebuild
  or a download.
- **Durable, never evicted:** tracked files, untracked files Git doesn't
  ignore, every ignored path outside those directories, and anything
  unclear. Nothing is reconstructible by guess.

The daemon evicts only reconstructible directories, and only of
workspaces that no process has its current directory in. Under the floor,
it takes workspaces idle for 10 minutes, least recently active first, and
deletes Cargo's `incremental/` directories in all of them before any
whole directory, until free space is 5% over the floor. `incremental/` is
the part least shared between workspaces and the cheapest to lose. After
`idle_ttl`, a workspace loses all its reconstructible directories. The
next build makes them again.

The main worktree is never registered, so the daemon never evicts its
build outputs and `remove` never deletes it. A workspace's files under 64
KiB, and all files outside its reconstructible directories, are never
replaced by clones either.

## Filesystems

Clones need a filesystem that shares blocks between files:

| Platform | Filesystem | Clones |
| -------- | ---------- | ------ |
| macOS | APFS | yes |
| Linux | Btrfs | yes |
| Linux | XFS made with `reflink=1` | yes |
| Linux | ext4, tmpfs, network volumes | no |

Without clones, workspaces still work as plain worktrees: nothing is
seeded and nothing is deduplicated, and OpenXet never copies build
outputs in full instead. `create` reports `seed_skipped:
"reflink_unsupported"` and `status` reports clones as not supported on
that volume. Eviction and the floor work on every filesystem.

## Measured

On an Apple M3 with APFS, debug builds of OpenXet's own workspace (373
units, 2.68 to 2.78 GiB of `target/` each), with a script that seeds
as `create` does ([S16](../research/S16-workspace-disk.md)):

- Six plain checkouts write about 16.7 GiB.
- A checkout seeded by clone, with mtimes aligned as `create` aligns
  them, compiled 0 units, wrote 0 bytes and finished a no-op build in 2
  s. Without the alignment, each compiled 12 units and wrote 1.31 GiB,
  49% of a build.
- Once workspaces diverge, rebuilding the 12 workspace crates wrote
  1.31 GiB. Of a second complete build, whole-file dedup (what the daemon
  does) leaves 45% new, and chunk-level dedup would leave 14%;
  `incremental/` is 65% of that 14%, which is why it is evicted first.

Linux, npm and Python projects and test targets are not measured yet.
