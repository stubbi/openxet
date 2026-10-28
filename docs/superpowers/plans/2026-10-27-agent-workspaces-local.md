# Agent workspaces, steps 1 and 2: measure, local store and provider

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

<!-- description: Plan for agent workspaces steps 1 and 2: measure parallel agent builds, then build the OpenXet workspace command with copy-on-write build outputs, clone dedup, a free-space floor and idle eviction, so the disk stays almost flat. -->

**Goal:** N coding agents work in parallel worktrees of one Git repository
while the disk stays almost flat during their builds and nothing needs
manual cleanup.

**Architecture:** A new client crate, `openxet-workspace`, behind a new
`openxet workspace` command. Workspaces are Git worktrees registered in a
per-user SQLite registry. Reconstructible directories (`target/`,
`node_modules/`, `.venv/`) are seeded at creation as copy-on-write clones
of the nearest sibling's, and a per-user daemon deduplicates settled files
in them by cloning (APFS `clonefile`, Linux `FICLONE`), keeps a free-space
floor by evicting idle workspaces' reconstructible directories, and evicts
them after an idle TTL. Step 3 (the mount, chunk-level write-time dedup)
gets its own plan once Task 2's measurements pick its shape.

**Tech Stack:** Rust 2024 (workspace toolchain), clap 4, sqlx with bundled
SQLite (already in the tree), `reflink-copy` 0.1 (clone files), `blake3`
(whole-file dedup key), `notify` (filesystem events), `fs4` or `std::fs`
file locks; Python compat suite (`uv`, pytest).

**Spec:** `docs/superpowers/specs/2026-10-27-agent-workspaces-design.md`

## Global Constraints

- Success test (spec §2): at the peak of six concurrent builds, each
  workspace beyond the first adds at most 10% of one build's footprint, and
  use never exceeds the human's budget. Builds at most 10% slower.
- Nothing is treated as reconstructible by guess: only preset paths
  (`target/`, `node_modules/`, `.venv/`) and paths the project config adds.
  Every other ignored path, every tracked and every untracked unignored
  file is durable (spec §6.2a).
- Eviction never touches durable files. Removal never loses unpushed or
  uncommitted work: it refuses and says what would be lost.
- Default idle TTL: 24 hours. Default free-space floor: the larger of
  20 GiB and 10% of the volume (Claude's call; configurable).
- No server needed; no data leaves the machine in this plan.
- ADR 0006: `openxet-workspace` must not depend on `xet-data`,
  `xet-client` or `xet-runtime`; `scripts/check-client-core-ban.sh` stays
  green.
- Every command: `--json` with an `openxet.workspace.*.v1` schema, errors
  as `openxet.error.v1` with a stable `code` and a `hint`, exit codes 0, 1
  or 2 as `docs/reference/exit-codes.md` defines, `--dry-run` where it
  changes state, and an entry in `compat/tests/test_agent_cli.py`.
- No `unsafe` (workspace lint `unsafe_code = "forbid"`); platform calls go
  through `reflink-copy`, `notify` and `std`.
- Public copy: "OpenXet" in prose, no em or en dashes,
  `scripts/check-brand.py` and `scripts/check-copy.py` green.
- Commits: Conventional Commits, scope `client` for the crate and command,
  `docs`/`adr` for docs; every commit signed off (`git commit -s`).
- An MCP server does not exist yet; each command's MCP tool is tracked in
  one issue opened by Task 9, not built here.

## Review Focus

1. **A build writing into `target/` while the daemon dedups it.** A file
   still being written, or rewritten between hash and clone, must never be
   replaced: the clone swap re-checks size and mtime under the settle rule
   and aborts on any change (Task 5 test
   `file_changed_after_hashing_is_left_alone`).
2. **Cargo freshness after a dedup swap or a seed.** Replacing a file with
   a clone must keep its mtime and permissions, or Cargo rebuilds
   everything (Task 5 test `swap_preserves_mtime_and_mode`; Task 4 test
   `seeded_target_keeps_mtimes`).
3. **A filesystem without clones** (ext4, a network volume). Seeding and
   dedup must fall back to doing nothing, never to a full copy, and say so
   once in `workspace status` (Task 4 test
   `seed_without_reflink_support_skips`, Task 5 test
   `dedup_without_reflink_support_reports_unsupported`).
4. **Worktrees an agent made without OpenXet** (`git worktree add` through
   a shell, subagent worktrees whose remove hook never runs). `adopt
   --scan` must register them so TTL and the floor cover them (Task 7 test
   `scan_registers_unknown_worktrees_once`).
5. **Removing a workspace with work in it.** Uncommitted changes, untracked
   unignored files and commits not on any remote block removal unless
   `--discard` (Task 4 test `remove_refuses_unpushed_commits`).

---

## File Structure

```
crates/workspace/
  Cargo.toml
  migrations/0001_registry.sql   registry and dedup index schema
  src/lib.rs                     public API re-exports
  src/error.rs                   WorkspaceError with stable codes
  src/paths.rs                   per-user state dir, project id
  src/registry.rs                Registry: workspaces, activity, dedup index
  src/classify.rs                Presets, Class, classify()
  src/git.rs                     git worktree add/remove, durable-work check
  src/seed.rs                    clone reconstructible dirs from a sibling
  src/dedup.rs                   settled-file dedup by clone swap
  src/space.rs                   free-space floor, eviction choice
  src/daemon.rs                  single-instance daemon loop
  src/hooks.rs                   Claude Code hook stdin/stdout contract
  tests/*.rs                     integration tests per module
crates/openxet/src/workspace_cmd.rs   clap subcommands, output, errors
compat/tests/test_workspaces.py       flat-disk gate (marker `workspaces`)
docs/guides/agent-workspaces.md       guide
docs/reference/cli.md, exit-codes.md, benchmarks.md (modified)
docs/adr/0013-*.md, 0014-*.md
docs/research/S16-workspace-disk.md
scripts/bench-workspaces.sh
```

---

### Task 1: ADRs 0013 and 0014, and Codex's draft folded in

**Files:**
- Create: `docs/adr/0013-agent-workspaces-come-next.md`
- Create: `docs/adr/0014-agents-are-the-primary-daily-user.md`
- Create: `docs/research/S15-agent-storage-feasibility.md`,
  `docs/research/S15-agent-storage-probe-results.json`,
  `scripts/probe-agent-storage.py` (from `origin/codex/agent-storage-vision`)
- Modify: `docs/SUMMARY.md`, `docs/superpowers/specs/2026-10-23-openxet-design.md` (§7.1 and §12 point at the ADRs)

- [ ] **Step 1:** `git checkout origin/codex/agent-storage-vision -- docs/research/S15-agent-storage-feasibility.md docs/research/S15-agent-storage-probe-results.json scripts/probe-agent-storage.py`; read them; fix brand and dash findings.
- [ ] **Step 2:** Write ADR 0013 from `docs/adr/0000-template.md`: decision "agent workspaces (spec 2026-10-27) come after M2 and before the rest of design §12; M3's backends, OIDC and issued tokens interleave as spec §9 says". It supersedes Codex's draft ADR 0013 of PR #103 (taking its number, since that draft never merged) and cites S15.
- [ ] **Step 3:** Write ADR 0014: agents are the primary daily user; the human sets agents loose and sets limits; every human guarantee of design §7.1 stays (readable output, safe defaults, limits, audit, approvals). Amends design §7.1.
- [ ] **Step 4:** Add a one-line pointer to each ADR in design §7.1 and §12; add the ADRs and S15 to `docs/SUMMARY.md`.
- [ ] **Step 5:** Run `python3 scripts/check-brand.py && python3 scripts/check-copy.py && scripts/build-site.sh`. Expected: all pass.
- [ ] **Step 6:** Commit `docs(adr): 0013 agent workspaces come next, 0014 agents are the primary daily user`; after merge, comment on PR #103 that it is folded in and close it.

### Task 2: Measure (spec roadmap step 1)

**Files:**
- Create: `scripts/bench-workspaces.sh`
- Create: `docs/research/S16-workspace-disk.md`
- Modify: `docs/superpowers/specs/2026-10-27-agent-workspaces-design.md` §6.3 and §6.4 (results and the decision)

**Interfaces:**
- Produces: the decision for step 3 (full mount or hybrid) and whether
  seeding plus clone dedup alone meets the 10% per-workspace bar on macOS.

- [ ] **Step 1:** Write `scripts/bench-workspaces.sh <repo> <n>`: creates `n` worktrees of `<repo>` at HEAD under a scratch dir on the same volume, samples volume used bytes (`df -k`) every second into a TSV, and runs one experiment per flag:
  - `--plain`: `cargo build` in each, concurrently.
  - `--seeded`: build the first, then `cp -c -R` (macOS) or `cp --reflink=always -R` (Linux) its `target/` into the others, then build them concurrently.
  - `--remap`: as `--seeded`, with `RUSTFLAGS="--remap-path-prefix=$PWD=/ws"` in every worktree.
  It prints peak used bytes over baseline, per-workspace increment as a percentage of one build, and wall time per build.
- [ ] **Step 2:** Run it on this repository on the Mac, one experiment at a time, quiet machine: `--plain` with `n=2` (six plain builds need about 90 GB, more than the disk has free; extrapolate), `--seeded` and `--remap` with `n=6`, aborting any run when free space falls under 20 GiB. Also run the scratchpad CDC tool on two complete sibling `target/` dirs, broken down by `deps/`, `incremental/`, `build/` (raw, whole-file, chunk).
- [ ] **Step 3:** Run the mount spike (`bench.sh` from the 2026-10-26 spike, full mount and hybrid) after the owner grants Network Volumes to the terminal. If not granted, record that and leave the mount decision to step 3's plan.
- [ ] **Step 4:** Write S16 with the machine, versions, raw numbers and the conclusions; update spec §6.3 and §6.4. If `--seeded` or `--remap` misses the 10% bar, the spec names which levers step 3 must add.
- [ ] **Step 5:** Commit `docs(research): S16 workspace disk and build measurements`.

### Task 3: Crate, registry and classification

**Files:**
- Create: `crates/workspace/Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/paths.rs`, `src/registry.rs`, `src/classify.rs`, `migrations/0001_registry.sql`
- Test: `crates/workspace/tests/registry.rs`, `crates/workspace/tests/classify.rs`
- Modify: `deny.toml` if the new dependencies need it; `fuzz/Cargo.lock` regeneration is not needed (no fuzz target uses the crate)

**Interfaces:**
- Produces:
  - `pub struct WorkspaceId(pub uuid::Uuid)` (v7)
  - `pub struct ProjectId(String)`: the canonical path of the repository's Git common dir (`git rev-parse --git-common-dir`, canonicalized).
  - `pub struct Workspace { id: WorkspaceId, project: ProjectId, path: PathBuf, branch: Option<String>, created_at: SystemTime, last_active: SystemTime, adopted: bool }`
  - `pub struct Registry` with `async fn open(state_dir: &Path) -> Result<Registry, WorkspaceError>`, `insert(&Workspace)`, `get_by_path(&Path) -> Option<Workspace>`, `list(Option<&ProjectId>) -> Vec<Workspace>`, `touch(&WorkspaceId, SystemTime)`, `remove(&WorkspaceId)`, and the dedup index `canonical(size: u64, hash: [u8; 32]) -> Option<PathBuf>`, `set_canonical(size, hash, &Path)`, `forget_canonical(&Path)`.
  - `pub fn state_dir() -> PathBuf`: `$OPENXET_WORKSPACE_STATE` if set, else `~/Library/Application Support/openxet/workspaces` on macOS, `$XDG_STATE_HOME/openxet/workspaces` (default `~/.local/state/...`) on Linux.
  - `pub enum Class { Durable, Reconstructible, Scratch }`, `pub struct Presets { dirs: Vec<PathBuf> }` with `Presets::default()` = `target`, `node_modules`, `.venv` at the workspace root, extended by `.openxet/workspace.toml` key `reconstructible = ["..."]`, and `pub fn reconstructible_dirs(root: &Path, presets: &Presets) -> Vec<PathBuf>` (existing directories only).
  - `pub enum WorkspaceError` with `fn code(&self) -> &'static str` returning stable codes used by the CLI: `not_a_repository`, `not_a_workspace`, `registry`, `git`, `unsaved_work`, `disk_budget`, `io`.

- [ ] **Step 1:** Write failing tests: `registry_round_trips_a_workspace`, `touch_updates_last_active`, `canonical_index_returns_the_last_set_path`, `forget_canonical_drops_entries_under_a_path`, `registry_survives_reopen`, `presets_are_target_node_modules_venv`, `project_config_adds_reconstructible_dirs`, `ignored_path_outside_presets_is_durable`.
- [ ] **Step 2:** Run `cargo nextest run -p openxet-workspace`. Expected: FAIL (crate missing).
- [ ] **Step 3:** Implement. SQLite in WAL mode, one file `registry.sqlite3` in the state dir, migrations via `sqlx::migrate!`, matching `crates/index`'s use of sqlx.
- [ ] **Step 4:** Run the tests, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check`, `scripts/check-client-core-ban.sh`. Expected: PASS.
- [ ] **Step 5:** Commit `feat(client): workspace registry and reconstructible-path classification`.

### Task 4: `openxet workspace create`, `list`, `status`, `remove`

**Files:**
- Create: `crates/workspace/src/git.rs`, `crates/workspace/src/seed.rs`
- Create: `crates/openxet/src/workspace_cmd.rs`
- Modify: `crates/openxet/src/cli.rs` (add `Workspace { command }`, update `agent_leg_lists_new_commands`), `compat/tests/test_agent_cli.py` (`JSON_COMMANDS`)
- Test: `crates/workspace/tests/git.rs`, `crates/workspace/tests/seed.rs`, unit tests in `workspace_cmd.rs`

**Interfaces:**
- Consumes: Task 3's `Registry`, `Presets`, `reconstructible_dirs`, `WorkspaceError`.
- Produces:
  - `pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<(), WorkspaceError>` (runs `git worktree add -b <branch> <path> <base>`).
  - `pub fn unsaved_work(path: &Path) -> Result<Vec<UnsavedWork>, WorkspaceError>` with `enum UnsavedWork { Uncommitted(PathBuf), Untracked(PathBuf), Unpushed { branch: String, commits: u32 } }`. A commit counts as pushed when it is reachable from any remote-tracking ref.
  - `pub fn seed(target: &Path, from: &Path, presets: &Presets) -> SeedReport` with `struct SeedReport { cloned_bytes: u64, skipped: Option<SeedSkip> }` and `enum SeedSkip { NoSibling, ReflinkUnsupported }`. Sibling choice: registered workspace of the same project whose HEAD has the smallest `git rev-list --count A...B` distance; ties by most recent `last_active`.
  - CLI: `openxet workspace create [--repo <dir>] [--name <slug>] [--base <rev>] [--path <dir>] [--dry-run]`, default path `<repo>/.claude/worktrees/<name>` if `<repo>/.claude` exists, else `<repo>/../<repo-name>-worktrees/<name>`; default name a random `adjective-noun-xxxx` slug. Output schema `openxet.workspace.created.v1`: `{path, name, branch, base, seeded_bytes, seed_skipped}`.
  - `openxet workspace list [--repo <dir>]` → `openxet.workspace.list.v1` `{workspaces: [{path, name, branch, last_active, reconstructible_bytes, idle}]}`.
  - `openxet workspace status` → `openxet.workspace.status.v1` `{volume_free_bytes, floor_bytes, reflink_supported, daemon_running, workspaces, deduplicated_bytes}`.
  - `openxet workspace remove <path> [--discard] [--dry-run]` → `openxet.workspace.removed.v1` `{path, freed_bytes}`; refuses with `unsaved_work` (exit 1), listing each item in `files`, hint naming `--discard`.

- [ ] **Step 1:** Write failing tests against a temp repo with a bare remote: `create_makes_a_worktree_on_a_new_branch`, `seeded_target_keeps_mtimes`, `seed_picks_the_nearest_sibling`, `seed_without_reflink_support_skips` (use `check_reflink_support` on the temp dir; on a filesystem with support, inject `ReflinkUnsupported` through a `seed_with(cloner)` seam used only by tests), `remove_refuses_uncommitted_changes`, `remove_refuses_unpushed_commits`, `remove_with_discard_deletes_worktree_and_branch`, `remove_dry_run_changes_nothing`, `create_outside_a_repository_is_not_a_repository`.
- [ ] **Step 2:** Run `cargo nextest run -p openxet-workspace -p openxet`. Expected: FAIL.
- [ ] **Step 3:** Implement; seed clones directory trees file by file with `reflink_copy::reflink`, then sets mtimes from the source (`File::set_modified`) and copies permissions. Never falls back to a byte copy.
- [ ] **Step 4:** Add the four invocations to `JSON_COMMANDS` in `compat/tests/test_agent_cli.py`, run `OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m agent`. Expected: PASS.
- [ ] **Step 5:** Run unit tests, clippy, fmt. Commit `feat(client): openxet workspace create, list, status and remove`.

### Task 5: Clone dedup of settled files

**Files:**
- Create: `crates/workspace/src/dedup.rs`
- Test: `crates/workspace/tests/dedup.rs`

**Interfaces:**
- Consumes: `Registry::{canonical, set_canonical, forget_canonical}`.
- Produces: `pub async fn dedup_file(reg: &Registry, path: &Path, settle: Duration) -> Result<DedupOutcome, WorkspaceError>` with `enum DedupOutcome { Cloned { bytes: u64 }, Canonical, Unsettled, Changed, TooSmall, Unsupported }`, and `pub async fn dedup_tree(reg: &Registry, root: &Path, settle: Duration) -> DedupReport { cloned_bytes, files_seen, unsupported: bool }`. Files under 64 KiB are `TooSmall` (Claude's call: clone metadata outweighs the saving).

Algorithm for `dedup_file` (fixed here because the tests don't determine it):

```
meta0 = lstat(path); skip unless regular file, size >= 64 KiB,
        now - mtime >= settle            -> Unsettled
hash  = blake3(file contents)
canon = reg.canonical(size, hash)
if canon is None or canon == path or canon no longer exists
        or lstat(canon) size differs or blake3(canon) != hash:
    reg.set_canonical(size, hash, path); -> Canonical
tmp = sibling temp name in path's directory
reflink(canon, tmp)                       Err(unsupported) -> Unsupported
set tmp's mtime/atime and mode from meta0
meta1 = lstat(path); if (size, mtime, inode) != meta0's: remove tmp -> Changed
rename(tmp, path)                         -> Cloned { bytes: size }
```

- [ ] **Step 1:** Write failing tests: `identical_files_share_blocks_after_dedup` (assert `Cloned` and, on APFS/btrfs/xfs, that volume free space does not drop by the file size when a third copy is deduped), `swap_preserves_mtime_and_mode`, `file_changed_after_hashing_is_left_alone` (seam: a hook between hash and swap that rewrites the file), `unsettled_files_are_skipped`, `small_files_are_skipped`, `stale_canonical_is_replaced`, `dedup_without_reflink_support_reports_unsupported`.
- [ ] **Step 2:** Run; expected FAIL. **Step 3:** Implement. **Step 4:** Run; expected PASS; clippy.
- [ ] **Step 5:** Commit `feat(client): deduplicate settled workspace files by copy-on-write clones`.

### Task 6: The daemon: watch, dedup, floor, TTL

**Files:**
- Create: `crates/workspace/src/space.rs`, `crates/workspace/src/daemon.rs`
- Modify: `crates/openxet/src/workspace_cmd.rs` (`daemon` subcommand, hidden from `--help`; `create` starts it if not running)
- Test: `crates/workspace/tests/space.rs`, `crates/workspace/tests/daemon.rs`

**Interfaces:**
- Consumes: Tasks 3 to 5.
- Produces:
  - `pub struct Budget { floor_bytes: u64, idle_ttl: Duration, settle: Duration }`, `Budget::default_for(volume_total: u64)` = floor `max(20 GiB, 10%)`, TTL 24 h, settle 2 s; overridden by `~/.config/openxet/workspace.toml` keys `floor`, `idle_ttl`.
  - `pub fn eviction_order(workspaces: &[Workspace], now: SystemTime, active: &HashSet<WorkspaceId>) -> Vec<WorkspaceId>`: idle ones only (no process has its cwd under the path, per the active set, and `last_active` older than 10 minutes), least recently active first.
  - `pub async fn run_daemon(state_dir: &Path, budget: Budget, shutdown: impl Future) -> Result<(), WorkspaceError>`: one instance per state dir via an exclusive lock on `daemon.lock`; watches every registered workspace's reconstructible dirs with `notify`, queues changed files, calls `dedup_file` after `settle`; every 5 s checks volume free space and evicts reconstructible dirs in `eviction_order` until free space is back over the floor plus 5%; every 10 minutes evicts reconstructible dirs of workspaces idle beyond `idle_ttl`; touches `last_active` on events; exits after 10 minutes with no registered workspaces.
  - Backpressure in `create`: if free space is under the floor after one eviction pass, fail with `disk_budget` (exit 1), message giving free and floor bytes, hint "Retry in a minute, or remove idle workspaces with `openxet workspace list` and `remove`."
  - Activity: a workspace is active when any process's cwd is under its path (`lsof -d cwd -Fn` on macOS, `/proc/*/cwd` on Linux), sampled each eviction pass.

- [ ] **Step 1:** Failing tests: `floor_defaults_to_twenty_gib_or_ten_percent`, `eviction_skips_active_workspaces`, `eviction_orders_least_recently_active_first`, `eviction_removes_only_reconstructible_dirs` (durable files and an ignored non-preset dir survive), `ttl_evicts_idle_reconstructible_dirs`, `second_daemon_exits_when_lock_is_held`, `daemon_dedups_a_file_written_after_start`, `create_fails_with_disk_budget_under_the_floor` (fake free-space source seam).
- [ ] **Step 2:** Run; FAIL. **Step 3:** Implement. **Step 4:** Run; PASS; clippy.
- [ ] **Step 5:** Commit `feat(client): workspace daemon keeps a free-space floor and evicts idle build outputs`.

### Task 7: Hooks and adoption

**Files:**
- Create: `crates/workspace/src/hooks.rs`
- Modify: `crates/openxet/src/workspace_cmd.rs`, `compat/tests/test_agent_cli.py`
- Test: `crates/workspace/tests/hooks.rs`

**Interfaces:**
- Produces:
  - `openxet workspace hook claude-create`: reads Claude Code's `WorktreeCreate` stdin JSON (`{"cwd", "name", ...}`; contract verified 2026-10-27 at code.claude.com/docs/en/hooks), creates the workspace for the repository at `cwd` with that name, prints only the absolute normalized path as the last stdout line, all else on stderr; any failure exits non-zero.
  - `openxet workspace hook claude-remove`: reads `{"worktree_path", ...}`, runs `remove` without `--discard`; on `unsaved_work` it exits non-zero and leaves the directory, so Claude Code keeps it.
  - `openxet workspace adopt <path>` and `adopt --scan [--repo <dir>]`: register existing worktrees (`git worktree list --porcelain`), `adopted = true`; idempotent. Output `openxet.workspace.adopted.v1` `{adopted: [path], already: [path]}`.
  - Other agents, documented in Task 9 and not built as commands: Cursor's `.cursor/worktrees.json` setup step and Codex environment scripts run `openxet workspace adopt .` inside the worktree they made.
  - `openxet workspace install claude [--project | --user] [--dry-run]`: merges the two hooks into `.claude/settings.json` (project) or `~/.claude/settings.json` (user) without dropping existing keys.

- [ ] **Step 1:** Failing tests: `claude_create_prints_only_the_path_on_stdout`, `claude_create_fails_without_a_path_on_error`, `claude_remove_keeps_a_workspace_with_unsaved_work`, `scan_registers_unknown_worktrees_once`, `install_merges_hooks_into_existing_settings`, `install_dry_run_prints_the_merged_settings`.
- [ ] **Step 2:** Run; FAIL. **Step 3:** Implement. **Step 4:** Run; PASS; agent leg passes with the new invocations.
- [ ] **Step 5:** Commit `feat(client): Claude Code worktree hooks and adoption of existing worktrees`.

### Task 8: The flat-disk gate

**Files:**
- Create: `compat/tests/test_workspaces.py` (marker `workspaces`, registered in `compat/pyproject.toml`), `compat/fixtures/workspace-project/` (a small Cargo workspace with a handful of crates.io dependencies, vendored via `cargo vendor` into the fixture so CI is offline)
- Modify: `.github/workflows/ci.yml` (job `workspaces-gate` on `ubuntu-latest`: create a 20 GiB XFS loop volume with `mkfs.xfs -m reflink=1`, mount it, run the marker there), `AGENTS.md` Commands, `docs/reference/benchmarks.md`

- [ ] **Step 1:** Write `test_six_concurrent_builds_stay_almost_flat`: `openxet workspace create` six times on the fixture, start the daemon, run `cargo build` in all six concurrently while sampling used bytes every 200 ms, assert peak minus baseline is at most `1 + 5 * 0.10` times one solo build's footprint (measured first in the same test), and that after `remove` of all six used bytes return within 1% of baseline.
- [ ] **Step 2:** Write `test_build_in_a_workspace_is_at_most_ten_percent_slower`: no-op and incremental (touch one source file) build times in a workspace versus a plain clone, median of 5, assert ratio at most 1.10.
- [ ] **Step 3:** Run locally on the Mac (APFS); record numbers. If the first test fails, the result goes into Task 2's decision and step 3's plan; do not weaken the assertion.
- [ ] **Step 4:** Add the CI job and the command line to AGENTS.md; record the Mac and CI numbers in `docs/reference/benchmarks.md` with the machine.
- [ ] **Step 5:** Commit `test(compat): flat-disk and build-speed gate for agent workspaces`.

### Task 9: Docs

**Files:**
- Create: `docs/guides/agent-workspaces.md`
- Modify: `docs/reference/cli.md`, `docs/reference/exit-codes.md` (codes `not_a_repository`, `not_a_workspace`, `registry`, `git`, `unsaved_work`, `disk_budget`), `docs/SUMMARY.md`, `site/llms-template.txt`, AGENTS.md "Current state"

- [ ] **Step 1:** Write the guide for both readers: the agent's path (`openxet workspace install claude --user`, then nothing), the human's (set `floor` and `idle_ttl`, read `status`), what is evicted and what never is, and macOS versus Linux filesystem support. Every command block runs in the `docs` compat leg (add a `test_docs_workspaces` case like `test_docs_quickstart.py`).
- [ ] **Step 2:** Run `uv run --frozen --directory compat pytest -m docs`, brand and copy checks, `scripts/build-site.sh`. Expected: PASS.
- [ ] **Step 3:** Open one issue "MCP tools for `openxet workspace`" linking the MCP server milestone. Commit `docs: agent workspaces guide and references`.

---

Step 3 (the mount) and steps 4 to 6 get their own plans after Task 2's
decision and this plan's gate are green.
