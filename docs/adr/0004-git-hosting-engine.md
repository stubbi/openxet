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

  | Engine | Server CPU | Peak RSS | Server wall (median) |
  |---|---|---|---|
  | git | 0.18 s | 89 MB | 0.5 s |
  | in-process gix prototype | 2.86 s | 178 MB | 6.5 s |

  A 100-commit fetch cost git 45 ms CPU and 33 MB, and gix 284 ms and
  216 MB. git without bitmaps needed 1.2–1.6 s CPU for the full clone.
- **Pushes.**
  - A one-commit push cost git about 120 ms CPU, all of it spawned
    processes. gix cost 13 ms, without object validation.
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
- **Statelessness.**
  - git served through `objects/info/alternates` into the node cache,
    including that cache's bitmap: 178 ms CPU, the same as local.
  - A cold node hydrated 80 MB of packs from MinIO in 0.4–0.6 s.
  - A geometric repack with a MIDX bitmap took 0.7 s for 19 packs, and
    left the base pack untouched.
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
     temp dir: `packed-refs` from the database, and
     `objects/info/alternates` pointing at the node's pack cache. It then
     runs `git upload-pack` / `git receive-pack`: with `--stateless-rpc`
     over HTTP, without it over SSH.
   - `GIT_PROTOCOL` is passed through.
   - openxet never runs `git http-backend`; it is the HTTP layer itself.
2. **openxet answers the ref-independent parts in-process.**
   - These are the protocol v2 capability advertisement (captured from the
     installed git at start-up) and `ls-refs` (from the database).
   - `fetch`, protocol v0/v1 advertisements and receive-pack go to git.
3. **Ref updates go through a `proc-receive` hook** (`receive.procReceiveRefs
   = refs/`).
   - git has already indexed the pack, checked connectivity, and, with
     `receive.fsckObjects=true`, validated every object.
   - The hook hands the commands and the new pack path to the parent
     openxet process over a Unix socket with a per-request token.
   - openxet then does four things, in this order:
     1. checks fast-forward and branch policy
     2. uploads `.pack` and `.idx` to the bucket and registers the pack in
        the database
     3. runs the all-or-nothing ref CAS in the database
     4. appends the ref log (§5.3)
   - It answers `ok` or `ng <ref> <reason>`, and the reason reaches the
     client verbatim.
   - A pack is durable in the bucket before any ref points to it.
4. **Nodes read the ref snapshot before syncing the pack list,** and
   download every registered pack they lack before serving. Pushed packs
   are never exploded (`receive.unpackLimit=1`).
5. **Repacking runs on one leaseholder per repo.**
   - It runs `git repack --geometric=2 -d --write-midx
     --write-bitmap-index`, with a periodic full `repack -adb`.
   - It publishes the result and swaps the pack list in one database
     transaction, and tombstones superseded packs.
   - GC (§5.6) deletes tombstoned packs, and packs no ref can reach (such
     as those from lost CAS races), after a grace period longer than the
     longest request.
6. **gitoxide is used in-process only off the wire path:**
   - reading objects for Hub API `tree`, `resolve` and commit listings
   - writing Hub API commits as small packs that go through the same
     publish-then-CAS path as pushes
7. **git is a runtime dependency.**
   - The minimum is 2.34: `proc-receive` needs 2.29, MIDX bitmaps need
     2.34.
   - `openxet` checks the version at start-up and fails with an actionable
     error.
   - Container images ship an unmodified distro git package and list it in
     their licence notice.
8. **Revisit** when gitoxide upstream (not a fork) ships server-side
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
  - A spawn-per-request cost of about 100 ms CPU per small push, and a few
    ms per fetch. The in-process `ls-refs` keeps polling cheap.
  - A second binary in every deployment, with GPL-2.0 obligations for
    images.
  - Hook plumbing: a Unix socket and a token.
  - Large repos need repo-affinity routing to keep caches warm.
- **Must stay true, with the tests that enforce it:**
  - Publish-then-CAS ordering, and refs-then-packs on read. A fault
    injection test kills the node between publish and CAS, and between
    CAS and ack, and asserts that no ref points at a missing object and
    no acknowledged push is lost.
  - Exactly one winner under concurrent pushes across nodes. The S9
    concurrency test (8 clients, 2 nodes, 20 rounds) becomes an
    integration test: one winner, database tip equals winner, fresh-node
    `fsck --strict` clean.
  - Any node serves any repo from the bucket alone: a compatibility suite
    leg clones from a node with an empty cache.
  - The `proc-receive` socket is never reachable from the network. A test
    asserts it is a Unix socket and that it rejects a missing or wrong
    token.
  - git version floor: a start-up test with a fake `git --version`.
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
