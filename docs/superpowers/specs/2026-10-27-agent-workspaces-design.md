# Agent workspaces: OpenXet as the drive agents work in

<!-- description: Design for OpenXet's next direction after M2: storage built first for AI agents, where many agents get full writable workspaces at once, the disk stays almost flat, nothing needs manual cleanup, and the human sets the limits. -->

- Status: proposed, for the owner's approval
- Date: 2026-10-27
- Builds on: the approved design (`2026-10-23-openxet-design.md`), Codex's
  draft vision (PR #103: `2026-10-25-openxet-agent-storage-vision.md`,
  ADR 0013 proposed, research S15), and the agent storage survey of
  2026-10-26 (summarised in [Prior art](#prior-art))

## 1. Intent

More and more of the work done on computers and the internet is done by
AI agents, not people. Storage is still built for people: folders to
browse, sync clients to install, sign-up flows, cleanup someone remembers
to do. The owner wants OpenXet to be storage built first for agents: "a
Dropbox or Google Drive for agents", "addressing a real need today that
grows in the future".

The need today is concrete and measured on the owner's own machine. Coding
agents run in parallel, and each one spins off a full Git worktree with its
own build outputs. On 2026-10-25 six parallel builds filled a 460 GB disk
and stalled every agent. On 2026-10-26 one research repository held 418
agent worktrees in 87 GB, and single OpenXet worktrees held `target/`
directories of 12 to 21 GB. Nobody knows which copies are safe to delete,
so they pile up until something breaks.

**The roles** (the owner's words): the human "sets agents loose and sets
limits". Day to day, the agent is the user. The human's surface is control:
budgets, permissions, approvals, audit and revocation. The human does not
browse files.

**Ambition** (the owner's words): "reach for the stars, and be happy when we
land on the moon". This document aims high and orders the work so each step
ships something useful on its own.

## 2. The success test

Decided by the owner:

1. **The disk stays almost flat** when N agents work in parallel on the same
   project.
2. **Nothing needs manual cleanup.**
3. **A build inside an agent workspace is at most 10% slower** than the same
   build on the plain local disk.

Each of these becomes a measured gate in CI, like the reconstruction and Hub
latency gates, with its benchmark recorded in `docs/reference/benchmarks.md`.

## 3. Sequence

Decided by the owner: **finish M2 first, then agent-first.** M2 completed on
2026-10-27. This work comes next, ahead of the remaining milestones' order
in design §12. How it interleaves with M3 (production backends, OIDC,
issued tokens) is in [§9](#9-roadmap).

## 4. Goals

- **Workspaces in seconds.** An agent asks for a full, writable workspace
  of a project (a Git repository at a revision, plus its data) and gets it
  in seconds, without copying the bytes it shares with other workspaces.
- **Shared bytes stored once.** Workspaces of the same project share source,
  dependencies and build outputs on the local disk wherever the bytes are
  the same. Xet's content-defined chunking makes "the same" finer than
  whole files.
- **Disposable local state.** Anything that can be rebuilt (build outputs,
  dependency caches, virtual environments) can be evicted under a disk
  budget and restored from the shared store on demand, without the agent
  noticing beyond latency.
- **Durable work.** Work worth keeping (commits, unpushed changes, results)
  becomes a durable, verifiable snapshot in OpenXet before any local copy
  is released, and is addressable as `xet://`, so another agent on another
  machine can pick it up.
- **Snapshots and forks.** A workspace or a bucket can be snapshotted
  (immutable, cheap) and forked (writable, copy on write), portable between
  machines and vendors because the format is open.
- **Limits the human sets.** Per agent: storage quota, new unique bytes,
  repositories it may touch, read or write, expiry, and approval for
  destructive actions. Enforced by the server at upload registration, not
  by the agent's good behaviour.
- **One provider for every agent.** One agent-neutral `openxet workspace`
  that plugs into the workspace hooks agents already expose (Claude Code
  `WorktreeCreate`/`WorktreeRemove`, Cursor `.cursor/worktrees.json`, Codex
  environment scripts), and works the same in cloud sandboxes.
- **Mac first, Linux close behind.** The owner's pain is on a Mac, and most
  people running parallel coding agents run them on Macs. Linux is where
  cloud agents and sandboxes run, and carries the long-term growth.

## 5. Non-goals and anti-goals

**Non-goals** (not now):
- A general-purpose network filesystem for humans, or a desktop sync client.
- An agent runtime, sandbox or messaging protocol. OpenXet is the storage
  under them; A2A and MCP carry references to it.
- Capturing process memory or running environments. A workspace snapshot is
  file state, as Codex's draft says.
- Replacing build tools' own caches (Cargo, sccache, Bazel, Turborepo). The
  workspace layer works with them and may serve as their remote tier.

**Anti-goals** (failure even if it works):
- **Slower builds than the bar.** If the mount costs more than 10%, the
  design falls back (see [§6.3](#63-the-mount)) rather than shipping a slow
  default.
- **Lost work.** Evicting or releasing a workspace must never lose
  unpushed changes. Durability comes before any automatic release.
- **Lock-in.** Snapshots, forks and manifests stay readable by stock Xet
  and Git tooling.
- **A degraded human.** The human's surface is smaller, but what they get
  (limits, audit, approvals) must be excellent and safe.
- **Breaking the founding rules.** Every non-negotiable and security
  requirement in AGENTS.md holds: protocol exactness, the bucket as source
  of truth, nothing deleted outside GC, no owned users, stateless nodes, no
  telemetry by default.

## 6. Shape

### 6.1 Three classes of workspace data

Codex's draft names the classes; this design keeps them and makes the
lifecycle follow from them:

| Class | Examples | If lost | Lifecycle |
|---|---|---|---|
| Durable work | commits, unpushed edits, results | work is lost | snapshot to OpenXet before release; GC only |
| Reconstructible | `target/`, `node_modules`, `.venv`, model weights | costs a rebuild or download | dedup locally; evict under budget; restore from store |
| Scratch | temp files, logs | nothing | dropped with the workspace |

The research (S15 and the survey) agrees that reconstructible data needs no
remote durability guarantee. So dedup and eviction of build outputs can ship
before durable snapshots, and durable roots only gate the automatic release
of unpushed source work.

### 6.2 The local store and workspace provider

*Builder's question settled by Claude:* a "project" is **any local Git
repository**, wherever its canonical remote lives (GitHub included, as for
OpenXet itself). Steps 2 and 3 need **no OpenXet server**: the local store is
embedded in the client. A local or remote `openxet serve` is optional and
adds durability and sharing across machines (steps 4 to 6). Evicted
reconstructible data is not uploaded anywhere by default; it is rebuilt, or
refilled from a remote store when one is configured.

Each machine runs one local OpenXet store: a content-addressed chunk cache
on local disk, shared by every workspace on that machine, bounded by a disk
budget the human sets. `openxet workspace create` materialises a workspace
from the store; `openxet workspace remove` snapshots durable work, then
releases the rest. Eviction picks cold reconstructible data first and never
touches durable work that is not yet snapshotted.

### 6.2a Release without a remove hook, and classification

*Settled by Claude, conservative by default:*
- **Classification.** Tracked files and untracked files that Git doesn't
  ignore are durable. Ignored paths are reconstructible only when a preset
  names them (`target/` for Cargo, `node_modules/` for npm and pnpm, `.venv/`
  for Python, extendable in project config); every other ignored path is
  durable. Nothing is ever treated as reconstructible by guess.
- **Secrets never leave the machine by default.** Snapshots skip files that
  match a secret filter (`.env*`, key and credential file patterns, and
  anything `git check-ignore` reports from a `.gitignore` line that names
  secrets), unless the human opts a path in. The filter is tested like the
  redaction guard.
- **Release without a hook.** Agents often never call remove (one machine
  held 418 stale worktrees). A workspace idle beyond a TTL the human sets
  (default 24 hours) gets its reconstructible data evicted. Its durable work
  stays on disk until step 4 can snapshot it; until then, "no manual
  cleanup" covers build outputs and caches, not unpushed source.

### 6.3 The mount

The owner's preferred shape is to **mount** the workspace: the agent sees a
normal directory, reads come from the local chunk cache, writes land on
local disk first, and chunking, dedup and upload happen in the background,
so the build never waits on OpenXet.

The open question is speed on macOS. The 2026-10-26 spike found:
- A kext-free user-space NFS loopback mount (the approach XetHub's
  `xet-mount` used, via the `nfsserve` crate) mounts without admin rights.
- macOS gates every network volume behind the **Network Volumes** privacy
  permission, granted per app (terminal, IDE, agent app). Denied, reads fail
  with a bare `EPERM`. The mount command must detect this and name the
  setting.
- Apple's FSKit offers path-based file systems only from macOS 26, behind a
  signed app extension the user enables. Not an option on macOS 15.
- No build timing through the mount yet: blocked on the permission.

So the design has a measured fork:
- **If a full-workspace mount stays within 10%** on a `cargo build` of this
  repository (cold, no-op and incremental), the whole workspace is mounted.
- **Otherwise, the hybrid:** source and read-mostly data come through the
  mount; each workspace's hot write directories (`target/`, `node_modules`,
  `.venv`) stay on native disk as disposable, budget-evicted directories,
  refilled from the shared store or a build cache.

On Linux, FUSE with passthrough or overlayfs over the chunk store are mature
and are expected to meet the bar; the same gate measures them.

*Settled by Claude:* the 10% bar is a CI gate on Linux. macOS hosted runners
can't be granted the Network Volumes permission, so the macOS leg is a
recorded benchmark on a reference Mac, run before each release, like the
latency gates' quiet-machine runs. The bar is measured on `cargo build` of
this repository (cold, no-op, incremental), plus one npm and one Python
project, so `node_modules` and `.venv` are covered.

### 6.4 Dedup expectations, measured

A first measurement on 2026-10-26 (Xet chunking parameters, two sibling
`target/` directories of this repository, the second build still in
progress, no compression):

| | Raw | Whole-file dedup | Chunk dedup |
|---|---|---|---|
| Both `target/` directories | 28.4 GiB | 20.1 GiB (−29%) | 17.1 GiB (−40%) |

Chunk dedup beats whole-file dedup (the approach of Cargo's draft blob
store, cargo#17512) by about ten points. A second sibling build still costs
about half its size, because Rust embeds per-build hashes and paths. So
**dedup alone does not flatten the disk; eviction under a budget does**, and
dedup multiplies what the budget holds. The measurement is repeated on
complete builds and broken down by `deps/`, `incremental/` and `build/`
before the gate's targets are set.

### 6.5 Snapshots, forks and handoffs

A workspace snapshot is a manifest: a Git commit for tracked files, plus an
ordinary Xet-backed record of untracked durable files, stored in a
repository or bucket and named by `xet://`. A fork is a new writable
workspace from a snapshot. Bucket snapshots (immutable) and forks (copy on
write over the path-to-file-hash map) are new bucket operations, added as a
superset that leaves stock Hugging Face bucket behaviour unchanged. A
handoff is a snapshot reference passed to another agent, with a token
narrow enough for that agent to read it.

### 6.6 Limits, audit and approvals

The design's §7.1 already promises delegated tokens narrower than the
human's own, attribution of every action, and approval gates. This work
adds what agents at scale need, enforced server side:
- **Storage quotas** per agent token and per namespace: stored bytes and new
  unique bytes, checked at upload registration.
- **Workspace budgets** per machine: the local disk budget, set by the human.
- **Expiry** on every agent token, measured in hours by default.

Egress caps stay out of scope until the mediated download path exists, as
Codex's draft says, because signed URLs can be replayed.

## 7. Constraints

- AGENTS.md non-negotiables and security requirements hold unchanged.
- The Xet wire format and `xet://` contract do not change.
- The two storage primitives (ADR 0010) stay: workspaces, snapshots and
  forks are built from repositories and buckets, not a third primitive.
- Nothing is deleted outside GC; the 28-day minimum grace stays. Local
  eviction of durable work happens only after its snapshot is in OpenXet;
  local eviction of reconstructible data is not data loss, because it can
  be rebuilt.
- The client-core ban (ADR 0006) keeps `xet-data` and friends out of server
  crates; the workspace provider and mount are client code.
- Every new command has `--json`, stable exit codes, structured errors with
  a hint, `--dry-run` where it changes state, and an MCP tool.

## 8. Prior art

The 2026-10-26 survey (primary sources, re-checked where marked in the
report) found nobody covering the full picture:
- **Coding agents** cap and sweep worktrees; none shares bytes between them.
  Claude Code issue #99672 reports about 200 GB across 22 worktrees.
- **Cloud sandboxes** (E2B, Modal, Vercel, Morph, Runloop, Fly Sprites,
  Cloudflare) offer fork, snapshot and pause, with no cross-sandbox dedup and
  platform-locked snapshots. Daytona's open-source repository was archived
  on 2026-10-03, leaving the open-source slot empty.
- **Closest competitors:** Vercel Drives (snapshots and writable forks),
  Tigris bucket forks, Mesa, Cloudflare Artifacts, lakeFS's agent features
  (Enterprise only). All closed or hosted.
- **Content-defined chunk dedup** exists only in Hugging Face Storage
  Buckets and Xet, Attic and casync. Hugging Face calls Buckets "agentic
  storage" and ships `hf-mount`, hosted only, without fork or snapshot.
- **Cargo's draft blob store** (cargo#17512) reports 47 to 55% savings
  across worktrees, and is the main risk to the local win and a candidate
  partner.

Integrate rather than build: sandbox volumes (as the backing store), build
caches (sccache, REAPI, Turborepo, Attic), mounts (`hf-mount`, Mountpoint,
JuiceFS), package stores (pnpm, uv), A2A and MCP for references.

## 9. Roadmap

Each step ships something useful alone and ends with its gate green.

1. **Measure** (days): finish the mount spike once the Network Volumes
   permission is granted; repeat the dedup measurement on complete builds.
   Decides full mount versus hybrid.
2. **Local store and workspace provider** (Mac first, Linux next): shared
   chunk cache, `openxet workspace create/remove`, disk budget, eviction of
   reconstructible data, the agent hooks. Gate: N parallel workspaces of
   this repository with a flat disk and no manual cleanup.
3. **The mount**, per step 1's result. Gate: the 10% build bar.
4. **Durable snapshots and handoffs**: workspace snapshots and restore,
   `xet://` references, safe automatic release of unpushed work.
5. **Bucket snapshots and forks** (ADR first).
6. **Limits at scale**: per-agent quotas and budgets, on top of M3's issued
   tokens and audit.

M3's production backends, OIDC and issued tokens are prerequisites for the
hosted and multi-tenant promises of steps 4 to 6, not for steps 1 to 3,
which run against a local or self-hosted server. The plan for this work
interleaves them; it does not drop any M3 to M7 scope.

## 10. Decisions this design needs

- **An ADR that amends design §7.1.** It currently says humans and agents
  are equal, "neither is a degraded mode of the other". The owner's
  direction makes agents the primary daily user and the human the one who
  sets limits. The ADR keeps every human guarantee (readable output, safe
  defaults, limits, audit, approvals) and states the new priority.
- **An ADR for the reorder** of design §12, superseding or folding in
  Codex's proposed ADR 0013.
- **An ADR for bucket snapshots and forks** before step 5.

## 11. Left to the builder

- The local store's on-disk layout, eviction policy details and IPC between
  workspaces and the store.
- Which build cache to integrate first. Step 2 may evict and let the build
  tool rebuild; refilling from a store is an optimisation, not a step 2
  requirement (settled by Claude).
- The mount server's implementation, once step 1 picks the shape.
- How Codex's draft (PR #103) is merged or folded in: its research (S15) and
  probes stay valid; its vision document is superseded by this one where
  they differ.
