# 0004: Serve Git with the `git` binary over openxet-owned refs and bucket packs

- Status: accepted
- Date: 2026-10-23
- Scope: `git`, `ssh`, `storage`, `index`, `gc`, `server`, `hub`

## Context

Design §10.9 makes openxet a Git host: smart HTTP and SSH, `upload-pack`
(protocol v2 where clients support it) and `receive-pack`, and one history
shared by Git pushes and Hub API commits. §5.3 and AGENTS.md add three
constraints:
- Git objects live in the bucket, as packfiles plus index.
- Refs live in the database, under compare-and-swap.
- Nodes are stateless, apart from an optional local pack cache.

§10.9 left the engine to spike S9. The evidence is in
`docs/research/S9-git-engine.md`.

**What was verified, and how.** Throwaway prototypes of three engines ran
behind one storage layer: SQLite refs under CAS, MinIO packs, and
node-local caches. They used git 2.39.5 and `gix` 0.90.0 / `gix-pack`
0.77.0, on an Apple M3. The host was heavily shared, so CPU and RSS are
the stable measures and wall times are indicative. Unless marked, the
results below are **observed**.

- **gix has no server side (source).**
  - `gix-protocol` 0.68 and `gix-transport` 0.62 are client-only.
  - `gix-pack` 0.77 has pack indexing, which works for pushes (observed),
    but only `PackCopyAndBaseObjects` generation: no delta compression
    and no bitmaps.
  - The maintainer closed three server PRs (#2104, #2465, #2887) and asks
    for server work to live in forks.
- **Reads.** Serving a full clone of a 10,001-commit, 218,574-object repo:

  | Engine | Server CPU | Peak RSS | Server wall (median, indicative) |
  |---|---|---|---|
  | git | 0.18 s | 89 MB | 0.5 s |
  | in-process gix prototype | 2.86 s | 178 MB | 6.5 s |

  A 100-commit fetch cost git 45 ms CPU and 33 MB, and gix 284 ms and
  216 MB. git without bitmaps needed 1.2–1.6 s CPU for the full clone.
- **Pushes.**
  - A one-commit push cost git about 120 ms CPU, all of it spawned
    processes. gix cost 13 ms.
  - Both were measured without object validation:
    `receive.fsckObjects` was off for git, and gix has none. With fsck on,
    git's cost rises with the number of pushed objects.
  - A whole-history push cost about the same in both: git 6.3 s, gix
    7.4 s.
- **Correctness.** 60 rounds of 8 simultaneous pushes to one branch, across
  two nodes sharing one database and bucket: git+git, gix+gix and git+gix.
  - Every round had exactly one winner, and the database tip was the
    winner.
  - Losers got a clean `non-fast-forward` or `fetch first`.
  - A fresh node hydrated from the bucket served a clone that passed
    `git fsck --strict`.
  - CAS correctness comes from the database, not the engine.
  - The database was SQLite (`BEGIN IMMEDIATE`). PostgreSQL's
    conditional-update CAS is not yet tested.
- **Statelessness.**
  - git served through `objects/info/alternates` into the node cache,
    including that cache's single-pack bitmap: 178 ms CPU, the same as
    local. A MIDX bitmap through alternates was not tested.
  - A cold node hydrated 80 MB of packs from MinIO in 0.4–0.6 s.
  - A geometric repack with a MIDX bitmap took 0.7 s for 19 packs, and
    left the base pack untouched.
- **Reachability (source).** git's v2 `parse_want` accepts any object in
  the odb, alternates included. Reachability is therefore openxet's job
  (item 2).
- **Alternatives (source).**
  - JGit DFS fits the storage model best, but needs a JVM sidecar (start-up
    alone 1.6–2.0 s CPU, about 77 MB, observed), and the DFS API is
    `internal`.
  - go-git v6 serves upload-pack and receive-pack only in beta, with no
    bitmaps and no thin packs.
  - libgit2 has no server protocol.
  - Gitaly assumes local disks.
  - Mononoke's Git server is GPL-2.0-or-later and not meant for external
    use.
  - Gitea, Soft Serve and Gitaly all spawn the git binary.

## Decision

1. **The wire protocol is served by the `git` binary.**
   - For each request, openxet builds a snapshot repo in a node-local
     temp dir:
     - `packed-refs` from the database
     - `objects/info/alternates` pointing at that repo's own node pack
       cache, and nothing else
   - It then runs `git upload-pack` / `git receive-pack`: with
     `--stateless-rpc` over HTTP, without it over SSH.
   - openxet never runs `git http-backend`; it is the HTTP layer itself.
   - Pinned config in every snapshot:
     - `uploadpack.allowAnySHA1InWant=false`
     - `uploadpack.allowReachableSHA1InWant=true`
     - `uploadpack.allowFilter=false` until partial clone is designed
       and tested; turning it on needs its own ADR amendment
     - `core.alternateRefsCommand=true`
     - `receive.procReceiveRefs=refs/`
     - `receive.fsckObjects=true`
     - `receive.unpackLimit=1`
     - `core.hooksPath` set to openxet's own hook directory
2. **openxet answers the ref-independent parts in-process, and enforces
   reachability for v2 `fetch` (R22).**
   - In-process: the protocol v2 capability advertisement (captured from
     the installed git at start-up) and `ls-refs` (from the database).
   - git's v2 `parse_want` only checks that the object exists in the odb,
     alternates included. It does not check reachability, so
     `allowAnySHA1InWant=false` does not protect v2 (source: git v2.39.5
     `upload-pack.c`, `parse_want`, lines 1413–1442).
   - openxet already parses v2 requests, so it checks every `want` of a
     `fetch` before spawning git:
     - A want that is a current database ref tip (or a tag's peeled
       target) passes. This is the fast path for every normal clone and
       fetch.
     - Any other want passes only if a bounded gix reachability walk
       (commit-graph assisted, with a commit and time budget) from the
       current refs finds it.
     - Anything else is refused with `ERR upload-pack: not our ref <oid>`
       and an actionable hint. This covers unreachable, force-pushed-away
       and lost-push objects, and budget exhaustion.
   - Cost: only non-tip wants pay for the walk. Stock clients send them
     rarely, mainly for submodule pins and `git fetch <sha>`.
   - Protocol v0/v1 advertisements and receive-pack go to git. v0/v1
     upload-pack only accepts advertised or reachable wants under the
     pinned config.
3. **Ref updates go through a `proc-receive` hook.**
   - Before the hook runs, git has already done three things:
     - indexed the pack
     - checked connectivity
     - validated every object (`receive.fsckObjects`)
   - The hook hands the commands and the new pack path to the parent
     openxet process. openxet then:
     1. validates the request (item 7)
     2. re-checks fast-forward and branch policy itself
     3. uploads `.pack` and `.idx` to the bucket and inserts the pack row
        as `pending`
     4. runs the all-or-nothing ref CAS, which also sets the pack row to
        `committed`, in the same database transaction
     5. appends the ref log (§5.3)
   - If the CAS fails, the pack row is set to `lost` in its own
     transaction. Pack rows nobody resolves stay `pending`.
   - It answers `ok` or `ng <ref> <reason>`, and the reason reaches the
     client verbatim.
   - A pack is durable in the bucket before any ref points to it.
4. **Nodes read the ref snapshot and the pack list consistently.**
   - Both reads come from the primary, or from one consistent database
     snapshot. Never from two replicas with different lag (§5.8).
   - Refs are read first, then packs. A node downloads every `pending`
     or `committed` pack it lacks, and skips `lost` ones.
   - Pushed packs are never exploded (`receive.unpackLimit=1`).
   - **Rebuilding:** the pack list is rebuildable from the bucket (`reindex`,
     §5.3). Every pack under `repos/<id>/packs/` is listed. A pack is
     `committed` if the ref-log tips reach objects in it; otherwise it is
     `pending` and ages out under item 5.
5. **Repacking and orphan reclaim.** These run on one leaseholder per repo.
   - The working copy is built from `committed` packs only. Pending and
     lost packs are never inputs, so a repack can't drop a push that is
     between publish and CAS.
   - It runs `git repack --geometric=2 -d --write-midx
     --write-bitmap-index`.
   - It also runs a periodic full
     `git repack -adb --cruft --cruft-expiration=<grace>`.
     - Unreachable objects younger than the grace period stay in a cruft
       pack instead of being dropped.
     - Example: a commit on branch A force-pushed away while a pending push
       on branch B builds on it. That push's CAS can still succeed.
   - It publishes the new packs and swaps the pack list in one database
     transaction: new packs are `committed`, inputs are `superseded` with
     a timestamp.
   - GC (§5.6, default grace 7 days) deletes three kinds of pack:
     - `superseded`
     - `lost`
     - `pending` older than the grace period
   - Objects of lost pushes are reclaimed with their pack, since a lost
     pack's objects are never reachable from any ref. Unreachable objects
     inside committed packs are reclaimed by the cruft expiry of the next
     full repack.
6. **gitoxide is used in-process only off the wire path:**
   - reading objects for Hub API `tree`, `resolve` and commit listings
   - the reachability walk of item 2
   - writing Hub API commits as small packs that go through the same
     publish-then-CAS path as pushes
7. **Hardening of the hook route and the git process.**
   - **Hook channel.** It is an inherited `socketpair` fd with no
     filesystem path. Where that is impossible (SSH), it is a Unix socket
     with mode `0600` in an openxet-private directory (`0700`).
   - **Token.** Each request gets a single-use token. It is bound to repo
     ID, principal and request ID, and expires when the request ends.
   - **What openxet trusts.** It derives the repo and authorization only
     from the token's request context, never from data the hook supplies.
   - **Pack path.** The path the hook reports must resolve (after
     `realpath`) inside that request's snapshot directory, or its
     quarantine directory, and nowhere else.
   - **Sanitised environment for every git process.**
     - It starts empty. The only `GIT_PROTOCOL` passed through is a
       validated `version=0`, `version=1` or `version=2`.
     - It sets `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and
       a private `HOME`.
     - It runs openxet's hooks only, via `core.hooksPath`.
   - **Paths.** Snapshot and cache paths derive from internal repo IDs,
     never from user-supplied names, so there is no path traversal.
   - **No cross-repo sharing.** Objects are never shared across repos,
     such as fork object pools, without a layer that enforces
     reachability per repo.
8. **git is a runtime dependency.**
   - The minimum is 2.37, the highest of these floors:
     - `proc-receive`: 2.29
     - MIDX bitmaps: 2.34
     - cruft packs: 2.37
   - `openxet` checks the version at start-up and fails with an
     actionable error.
   - **Licence.** git (GPL-2.0-only) runs as a separate program over
     pipes, never linked. That is aggregation, so openxet stays
     Apache-2.0. This is inferred from GPL-2.0 §2, not legal advice.
   - **Images.** Container images ship an unmodified distro git package
     and list it in their licence notice.
     - Each image release publishes the exact git source package next to
       the image, or includes a written offer for it (GPL-2.0 §3).
9. **Revisit** when gitoxide upstream (not a fork) ships server-side
   upload-pack and receive-pack, delta compression and bitmaps. Then rerun
   the S9 server-only benchmark.

## Consequences

- **Easier.**
  - Protocol completeness, from day one and maintained upstream: v0/v1/v2,
    shallow, partial clone, bitmaps, `fsckObjects` and quarantine.
  - Clone and fetch cost is the lowest measured.
  - CAS, durability and statelessness are openxet code that is
    independent of the engine, so they are testable without git.
- **Harder.**
  - Spawn-per-request cost:
    - about 100 ms CPU per small push, measured without
      `receive.fsckObjects`, so it rises with fsck on, in proportion to
      the pushed objects
    - a few ms per fetch
    - The in-process `ls-refs` keeps polling cheap.
  - A second binary in every deployment, with GPL-2.0 source obligations
    for images.
  - openxet must parse v2 `fetch` wants and keep the reachability check
    in step with git's protocol.
  - Hook plumbing: a socketpair or socket, and tokens.
  - Large repos need repo-affinity routing to keep caches warm.
- **Must stay true, with the tests that enforce it:**
  - `publish_then_cas_survives_faults`, a fault-injection test. It kills
    the node at three points:
    - between publish and CAS
    - between CAS and ack
    - during a full repack that runs while a push sits between publish
      and CAS

    It asserts that no ref points at a missing object, that no
    acknowledged push is lost, and that the pending pack is neither a
    repack input nor deleted before its grace period.
  - `concurrent_push_one_winner`, from the S9 concurrency test (8 clients,
    2 nodes, 20 rounds): one winner, database tip equals winner,
    fresh-node `fsck --strict` clean.
    - S9's evidence comes from SQLite. The test runs on SQLite and on
      PostgreSQL, where CAS is a conditional `UPDATE … WHERE oid = $old`
      in the same transaction.
  - `cold_node_serves_from_bucket`, a compatibility suite leg that clones
    from a node with an empty cache.
  - `v2_want_unreachable_refused`. A v2 `want` is refused in four cases:
    - an OID that is in the odb but unreachable
    - a force-pushed-away commit
    - an object in a lost push's orphan pack
    - an object only in another repo's cache

    A non-tip reachable want still succeeds.
  - `upload_pack_config_pinned`: the snapshot config has the item 1
    values.
  - `hook_channel_private`:
    - the hook channel has no network listener
    - a path socket is `0600` in a `0700` directory
    - a missing, wrong, reused or expired token is rejected
  - `hook_cannot_choose_repo`: hook-supplied repo names and principals are
    ignored, and the token's context wins.
  - `hook_pack_path_confined`: pack paths outside the request's snapshot
    or quarantine directory are rejected, including symlink and `..`
    escapes.
  - `hook_policy_rechecked`: a non-fast-forward or a protected-branch
    update is refused by openxet even if the hook reports it as allowed.
  - `git_env_sanitised`:
    - an injected `GIT_PROTOCOL` value other than `version=N` is
      dropped
    - global, system and `HOME` config cannot change git's behaviour
  - `repo_paths_from_ids`: repo names with `..`, `/` or absolute paths
    never reach a filesystem path.
  - `lost_packs_not_hydrated` and `pending_packs_aged_out_by_gc`.
  - `git_version_floor`: a start-up test with a fake `git --version` below
    2.37.
  - Performance: the S9 server-only fetch benchmark runs in CI as a
    regression gate (AGENTS.md: performance claims come with a benchmark).

## Alternatives considered

- **gitoxide in-process for everything.** Lost on reads: 16x the CPU and
  2x the memory of git for a clone, and 6x the CPU for a fetch (observed).
  It also needs a server protocol stack, delta compression, bitmaps and
  object validation that upstream keeps out of its crates. openxet would
  maintain a fork-sized server alone.
- **git for reads, gix for receive-pack (H2).** It saves about 100 ms CPU
  per push. In exchange, openxet would own receive-pack's security
  surface (object validation, report-status, push options, atomic
  pushes) that git already provides. Not worth it at v1 push volumes.
- **gix for reads, git for pushes.** Worst of both: it keeps gix's clone
  cost.
- **JGit with DFS as a sidecar.** Closest to the storage model, with
  bucket-backed packs, a block cache and reftable. It loses on "one
  binary" (design §5.1, §9): a JVM runtime with about 77 MB and 1.6–2.0 s
  CPU just to start, and an `internal` storage API with no stability
  promise.
- **go-git v6 as a sidecar.** Server support is beta (2026-10-04), with no
  bitmaps and no thin-pack support in upload-pack (source). It would add a
  Go runtime and process.
- **libgit2 (`git2`).** No server protocol, so it is the gix trade-off plus
  C.
- **Gitaly.** A Git RPC service over local disks with its own replication
  (Praefect). It conflicts with "bucket is truth". Kept as a reference for
  the pack-objects cache.
- **Mononoke.** GPL-2.0-or-later, and its OSS build is not meant for
  external use. Kept as a reference: a gix-primitives server over object
  storage is possible at scale with a large team.
