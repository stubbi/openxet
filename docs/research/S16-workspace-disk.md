# S16: Workspace disk and build measurements

<!-- description: Disk use and build work of six sibling Cargo worktrees on macOS APFS: plain, seeded by clone, seeded with mtime alignment, and path remapping, plus a chunk-level dedup breakdown of two complete target/ directories. -->

- Date: 2026-10-28
- Scope: step 1 ("Measure") of the [agent workspaces
  design](../superpowers/specs/2026-10-27-agent-workspaces-design.md) (§2,
  §6.3, §6.4), Task 2 of the plan
  `docs/superpowers/plans/2026-10-27-agent-workspaces-local.md`.
- Repository measured: OpenXet at `1c49398`, `cargo build --workspace
  --locked` (debug, every workspace member, no test targets; 373 units).
- Tool: `scripts/bench-workspaces.sh` in this repository.

## Verdict

- **Seeding `target/` by APFS clone meets the 10% bar only with mtime
  alignment.** With every tracked file given its sibling's mtime, five
  extra workspaces compiled 0 units, wrote 0 bytes and finished a no-op
  build in 2 s each. Without alignment, Cargo recompiles all 12 workspace
  crates in every extra workspace: 1.31 GiB each, 49% of one build.
- **Per-workspace `--remap-path-prefix` is harmful.** Each workspace gets
  different `RUSTFLAGS`, so Cargo rebuilds all 373 units in every one of
  them: 114% of one build per extra workspace, worse than no seeding. One
  identical flag string naming every workspace path keeps seeding intact
  (0 units, 0 bytes), but adding a workspace changes the string.
- **Chunk dedup of two complete sibling builds leaves 14% of the second
  build new** (whole-file dedup leaves 45%). Of that 14%, 65% is
  `incremental/`, which stays half unique even at chunk level; `deps/` is
  97% shared at chunk level and `build/` almost entirely.
- **The mount was not measured.** The Network Volumes permission was not
  granted on the reference Mac, so the mount spike (the plan's Task 2 step 3)
  did not run. The choice between a full mount and the hybrid stays with step 3's
  plan.

## Machine and versions

| | |
|---|---|
| Machine | Apple M3, 8 cores, 16 GiB memory |
| OS | macOS 15.6.1 (24G90), APFS data volume, about 70 GiB free at start |
| Toolchain | rustc 1.97.1, cargo 1.97.1 (pinned in `rust-toolchain.toml`) |
| Chunker | Xet's: gearhash, 64 KiB target, 8 KiB minimum, 128 KiB maximum, as in `xet-data` 1.7.0 |

The machine was not quiet. Other sessions built Rust code during every run:
`rustc` processes outside the benchmark appeared in 7% to 67% of the
one-second samples per run, and the load averages ranged from 16 to over
200. So:
- **Bytes written and units compiled are the primary measures.** Bytes
  written is the allocated size of the files each workspace's build left in
  its `target/` (all of it for a build from empty, only files newer than
  the seed for a seeded one); other processes do not move it. Units
  compiled is the count of Cargo's `Compiling` lines.
- **The `df` peak is reported but noisy.** It is the volume's used bytes
  over the baseline, sampled every second, so other sessions' writes and
  deletions land in it. The standalone first build measured 2.45 to 4.05
  GiB by `df` across runs while its `target/` was 2.68 to 2.77 GiB.
- **Wall times are indicative only.** They are an order of magnitude, not
  a speed claim.

Each experiment ran twice, once in a first pass and once gated on 30 s
without any other `rustc` on the machine at its start. The second pass's
numbers are below; the first pass agreed on units compiled and bytes
written (seeded without alignment: 12 units and 1.31 GiB per extra
workspace, 48.7%; with alignment: 0 and 0).

## Method

`scripts/bench-workspaces.sh MODE <repo> <n>` clones the repository with
`git clone --local` into a scratch directory on the same APFS volume, adds
`n` detached worktrees of `HEAD` to that clone, samples `df -k` every
second, and builds:

| Mode | What it does |
|---|---|
| `--plain` | `n` builds at once, each from an empty `target/` |
| `--seeded` | build the first; then create the others, `cp -c -pR` (clonefile, mtimes kept) its `target/` into each, and build them at once |
| `--seeded --align-mtimes` | as `--seeded`, then `touch -r` every tracked file to the first worktree's mtime (same commit, so the blobs are identical) |
| `--remap --align-mtimes` | as the line above, with `RUSTFLAGS="--remap-path-prefix=<own path>=/ws"` in each worktree, the first included |
| `--remap-shared --align-mtimes` | as the line above, with one identical `RUSTFLAGS` in every worktree that remaps each of the `n` paths to `/ws` |

The script stops every build the moment free space falls under 20 GiB
(`BENCH_FLOOR_GIB`), and always removes the worktrees and the scratch
directory, also on failure. No run came near the floor. Plain ran with
`n=2` and is extrapolated to six; the others ran with `n=6`. In seeded
modes the extra worktrees are created after the first build, as when an
agent asks for a workspace next to a built one; their checkout is then
newer than the cloned outputs, which is what dirties Cargo's fingerprints.

The chunk analysis is a throwaway program (not committed) that splits every
file of two complete sibling `target/` directories from the plain run with
Xet's chunker and counts bytes not seen before, by whole file (BLAKE3 of
the file) and by chunk (BLAKE3 of the chunk), per top-level directory
under `target/debug/`. Hard links count once, as on disk. Its chunk
boundaries match the 796 chunk lengths of the Xet reference file
`Electric_Vehicle_Population_Data_20250917.csv.chunks`
(`xet-team/xet-spec-reference-files` at `c4aa3a3`) exactly.

## Results

### Six workspaces

Per extra workspace, as a share of one build (`target/` of the first
workspace, 2.68 to 2.78 GiB by bytes written):

| Mode | n | Units compiled | Bytes written | Share by bytes written | `df` peak over baseline | Share at the `df` peak | Wall time |
|---|---|---|---|---|---|---|---|
| plain | 2 | 373 | 2.79 GiB | 100.1% | 3.28 GiB | 92.4% | 424 s (both at once) |
| seeded | 6 | 12 | 1.31 GiB | 49.0% | 6.53 GiB | 32.1% | 164 s first, 86 s each other |
| seeded, mtimes aligned | 6 | 0 | 0 | 0.0% | 4.10 GiB | 0.2% | 215 s first, 2 s each other |
| remap, mtimes aligned | 6 | 373 | 3.06 GiB | 114.0% | 15.31 GiB | 104.9% | 192 s first, about 1,360 s each other |
| shared remap, mtimes aligned | 6 | 0 | 0 | 0.0% | 3.80 GiB | 0.0% | 827 s first (load over 150), 2 s each other |

Six plain builds, extrapolated: about 16.7 GiB by bytes written (six times
2.78 GiB). The design's earlier estimate of about 90 GB came from the
2026-10-26 directories (14 GiB each), whose build command was not
recorded; they were most likely built with test targets, so `cargo test`
or `cargo nextest` workloads should be measured before the gate's
absolute budgets are set.

The `df` share for plain is computed against the final use over `n`
(1.70 GiB), since plain has no standalone build. `df` reports a plain
build at about 60% of its `du` size; the cause was not isolated here.

### Chunk dedup of two complete sibling builds

Two `target/` directories built at once from empty, same commit, different
paths. Second directory's bytes that are new given the first:

| Directory | Raw (second) | New after whole-file dedup | New after chunk dedup |
|---|---|---|---|
| `deps/` | 1,901 MB | 584 MB (30.7%) | 54.5 MB (2.9%) |
| `incremental/` | 567 MB | 567 MB (99.9%) | 283.9 MB (50.1%) |
| `build/` | 90.9 MB | 0.09 MB (0.1%) | 0.09 MB (0.1%) |
| `.fingerprint/` | 0.39 MB | 0.006 MB (1.6%) | 0.006 MB (1.6%) |
| other (`target/debug/` itself: uplifted binaries) | 284 MB | 127 MB (44.7%) | 68.8 MB (24.2%) |
| **Total** | **2,843 MB** | **1,278 MB (44.9%)** | **407 MB (14.3%)** |

Both directories together: 5.30 GiB raw, 3.55 GiB after whole-file dedup
(minus 33%), 2.04 GiB after chunk dedup (minus 62%). Within one directory,
chunk dedup alone already removes 37% (2.84 GB to 1.78 GB). The
2026-10-26 figure (minus 40% for both directories) was taken with the
second build still running; on complete builds chunk dedup does much
better, and beats whole-file dedup by 29 points, not ten.

## Conclusions

1. **Seed with clones and align mtimes.** Step 2's seed must clone
   `target/` with mtimes kept and give each tracked file whose blob matches
   the sibling's the sibling's mtime; files that differ keep their
   checkout mtime, so Cargo rebuilds exactly what changed (that case was
   not measured here). With that, a new workspace costs nothing on disk and
   builds as a no-op, so the 10% bar holds at creation on APFS.
2. **Never vary compiler flags per workspace.** Path stability must come
   from a stable build path (the mount point is one) or one flag set shared
   by every workspace of a project, not from per-workspace `RUSTFLAGS`.
   The shared flag set works (0 units, 0 bytes) but changes whenever a
   workspace is added, which rebuilds every workspace; it is not a
   shippable lever as measured. Cargo's `trim-paths` was not measured.
3. **Divergence is the remaining cost.** Once agents edit and rebuild, a
   workspace's own crates diverge: a rebuild of the 12 workspace crates
   wrote 1.31 GiB (49% of one build). Chunk dedup would keep `deps/`
   nearly flat (2.9% new), but `incremental/` stays half unique.
4. **Levers step 3 must add** to keep the bar after divergence:
   settled-file dedup or write-time chunk dedup of `deps/` and uplifted
   binaries (plan Task 5); budget eviction that targets idle workspaces'
   `incremental/` first, since it is the least shareable and is rebuilt
   on demand; and a measurement of `CARGO_INCREMENTAL=0` in agent
   workspaces (bytes against build time) before choosing it.
5. **The mount decision is open.** The disk bar at creation does not need
   the mount; its case rests on path-stable builds after divergence and on
   the 10% speed bar, which step 3 measures once the Network Volumes
   permission is granted.

Not covered: Linux (reflink on Btrfs or XFS), npm and Python projects,
test targets, and edits after seeding. Each is a run of the same script.

## Reproduce

```bash
scripts/bench-workspaces.sh --plain --inspect <cdc-tool> . 2
scripts/bench-workspaces.sh --seeded . 6
scripts/bench-workspaces.sh --seeded --align-mtimes . 6
scripts/bench-workspaces.sh --remap --align-mtimes . 6
scripts/bench-workspaces.sh --remap-shared --align-mtimes . 6
```

Results (per-second `df` samples, events, per-build logs and a summary
line per run) land in `$TMPDIR/s16-bench-results` unless `BENCH_RESULTS`
says otherwise.
