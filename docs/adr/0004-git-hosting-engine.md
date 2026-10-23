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
  the odb, alternates included. git 2.37–2.44 also advertises and serves
  the v2 `object-info` command unconditionally, which returns the size of
  any OID in the odb. Reachability is therefore openxet's job (item 2).
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
   - For each request, openxet builds a snapshot repo in a temp dir on
     the cache's filesystem, from **one database snapshot**:
     - `packed-refs`, with the repo's refs
     - `objects/pack`, holding hard links to exactly the cached pack
       files of that snapshot's `committed` pack set (item 4), and no
       others
   - The snapshot has no `objects/info/alternates`, so git can't see any
     pack outside that set. A generation's `multi-pack-index` and its
     bitmap are linked only when every pack they cover is in the set.
   - It then runs `git upload-pack` / `git receive-pack`: with
     `--stateless-rpc` over HTTP, without it over SSH.
   - openxet never runs `git http-backend`; it is the HTTP layer itself.
   - Pinned config in every snapshot:
     - `uploadpack.allowAnySHA1InWant=false`
     - `uploadpack.allowReachableSHA1InWant=true`
     - `uploadpack.allowRefInWant=false`, so every want is an OID that
       openxet checks
     - `uploadpack.allowFilter=false` until partial clone is designed
       and tested; turning it on needs its own ADR amendment
     - `core.alternateRefsCommand=true`, as defence in depth: snapshots
       have no alternates
     - `receive.procReceiveRefs=refs/`
     - `receive.fsckObjects=true`
     - `receive.unpackLimit=1`
     - `core.hooksPath` set to openxet's own hook directory
     - `transfer.advertiseObjectInfo=false`,
       `uploadpack.advertiseBundleURIs=false`, `promisor.advertise=false`
       and `transfer.advertiseSID=false`. Older git ignores some of these
       keys (item 2 lists the versions), so they are defence in depth.
       The command allowlist in item 2 is the control that holds from
       2.37 on.
2. **openxet answers the ref-independent parts in-process, and enforces
   reachability for every `fetch` want (R22).**
   - **In-process:** the protocol v2 capability advertisement and
     `ls-refs` (from the database). The advertisement is captured from the
     installed git at start-up, with the pinned config, and then filtered
     to the allowlist below.
   - **v2 command allowlist.** The interposer accepts exactly two
     commands: `ls-refs`, which openxet answers, and `fetch`, whose wants
     it checks before forwarding. Any other `command=`, including
     `object-info`, `bundle-uri` and unknown names, gets an `ERR` from
     openxet and never reaches git. The advertisement lists only `agent`,
     `ls-refs`, `fetch` (without `filter`), `server-option` and
     `object-format`. Adding a command needs an ADR amendment.
     - The same holds one level up. Over SSH, only `git-upload-pack` and
       `git-receive-pack` are accepted, and `git-upload-archive` or any
       other command is refused. Over HTTP, only the two smart services
       exist; the dumb protocol's `objects/` paths are not served.
     - **What git ≥ 2.37 serves by default** (source: git `serve.c`, the
       `capabilities[]` table, at tags v2.37.0 to v2.51.0):
       - `ls-refs` and `fetch`, always
       - `object-info` in 2.37–2.44, always (`.advertise =
         always_advertise`). It answers the size of any OID in the odb,
         with no reachability check: an existence and size oracle for
         unreachable objects. From 2.45 it is gated by
         `transfer.advertiseObjectInfo`, "Defaults to false"
         (`Documentation/config/transfer.txt`, v2.45.0).
       - `bundle-uri` from 2.40, only with
         `uploadpack.advertiseBundleURIs` (default false; `bundle-uri.c`,
         `bundle_uri_advertise`)
       - the non-command capabilities `session-id` (only with
         `transfer.advertiseSID`) and, from 2.49, `promisor-remote` (only
         with `promisor.advertise`)
     - git's `parse_command` dies with "invalid command" for a command
       whose advertise check is false, in every version from 2.37 to
       2.51. So on 2.45+ the pinned config also blocks `object-info`
       inside git, but on 2.37–2.44 only openxet's allowlist does.
   - **Why.** git's v2 `parse_want` only checks that the object exists in
     the odb. It does not check reachability, so
     `allowAnySHA1InWant=false` does not protect v2 (source: git v2.39.5
     `upload-pack.c`, `parse_want`, lines 1413–1442).
   - **Which requests.** openxet interposes on the request stream. It
     checks wants before forwarding them to git, so git never sees an
     unchecked want. This covers:
     - every v2 `command=fetch`, including several commands in one
       stateful SSH session
     - the v0/v1 want lines over HTTP and SSH

     Checking v0/v1 too means openxet does not depend on git's
     per-request `rev-list` for non-advertised wants, whose cost would
     be uneven across protocol versions.
   - **One walk per request.**
     - The walk starts from the same ref snapshot that is written to the
       request's `packed-refs`.
     - A want equal to a ref tip, or a tag's peeled target, passes
       without walking. This is every normal clone and fetch.
     - All remaining wants share one gix walk over commits
       (commit-graph assisted). The walk stops when every want is found
       or the budget is spent.
   - **Kinds of want:**
     - commit and tag wants are checked by the walk
     - tree and blob wants pass only when a ref or peeled tag points at
       them directly
     - no tree traversal is done, so any other tree or blob want is
       refused
   - **Budget, per request across all wants, with config defaults:**
     - `git.reachability.max_commits`: 200,000
     - `git.reachability.max_ms`: 2,000
     - `git.reachability.max_concurrent_per_repo`: 2
     - `git.reachability.max_concurrent_per_principal`: 4
     - A request over a concurrency limit gets a retryable `ERR` (HTTP
       429 over HTTP).
   - **Fail closed.** A want that is not found within the budget is
     refused with `ERR upload-pack: not our ref <oid>` and a hint. This
     covers unreachable, force-pushed-away, pending and lost-push objects.
   - **Cost.** Only non-tip wants walk. Stock clients send them rarely:
     `git fetch <sha>` and submodule pins.
   - **Known limitation.** In very large repos, fail-closed can refuse a
     legitimate deep `fetch <sha>` or submodule pin that the budget can't
     reach. The mitigation is bitmap-backed reachability: membership in a
     reachability bitmap built from the **current ref tips only**. The
     repack's own bitmaps include the keep refs (item 5) and must never
     be used for this. It is a follow-up, tracked as an issue labelled
     `git-reachability`, with no code until then.
   - **Known limitation: `have` acknowledgements.** git ACKs a `have`
     whose object is in the snapshot's packs, reachable or not. That
     confirms an object exists, but never sends it. Filtering haves
     through the same walk is part of the `git-reachability` follow-up.
3. **Ref updates go through a `proc-receive` hook, and every ref writer
   commits in one fenced transaction.**
   - Before the hook runs, git has already done three things:
     - indexed the pack into the request's quarantine
     - checked connectivity against the snapshot's pack set only
       (`committed` packs, item 4)
     - validated every object (`receive.fsckObjects`)
   - A push's own new pack is visible only to its own receive-pack,
     through quarantine. Other requests can't see it until it is
     `committed`.
   - The hook hands the commands and the new pack path to the parent
     openxet process. openxet then:
     1. validates the request (item 7)
     2. re-checks fast-forward and branch policy itself
     3. applies the writer reachability rule below
     4. inserts the pack row as `pending`, then uploads `.pack` and `.idx`
        to the bucket. The row comes first, so the bucket never holds a
        pack that no row describes, except after database loss
        (item 4, `reindex`).
     5. runs the commit transaction below
     6. answers the client only after that commit
   - **The commit transaction.** Every ref writer uses the same
     transaction: pushes, Hub API ref creates, updates and deletes, Hub
     commits and Hub-created tags (item 6), and the operator's `reindex`
     restore (item 4). It commits only if all of these hold:
     - the ref CAS for every command matches
     - for a writer with a pack, `packs.state = 'committed' WHERE id =
       $pack AND state = 'pending'` matches one row
     - `repos.full_repack_gen` still equals the value read in the
       writer's snapshot (item 5's fence). The snapshot is the one item 4
       describes: refs, `committed` pack set and `full_repack_gen` from
       one database read.
     - one `ref_log` row per command is inserted, with repo, ref, old,
       new, actor, request and time
   - **Writer reachability rule (R22 for writers).** For each new ref
     target N, every object listed by `git rev-list --objects N --not
     <snapshot ref tips>` must be in the writer's own new pack.
     - A writer with no pack (a Hub ref create or update) must get an
       empty list, so N must already be reachable from the snapshot's
       refs. N is checked exactly like an R22 want (item 2): a ref tip or
       peeled tag target passes, a commit or tag needs the walk within
       its budget, a tree or blob passes only as a direct ref target, and
       everything else is refused (fail closed).
     - **Why.** receive-pack's connectivity check, and gix, accept any
       object present in the snapshot's committed packs. Those include
       objects no ref reaches any more: force-pushed away, not yet
       expired. Without this rule a writer could point a ref at one, or
       name it as a parent without sending it, and then fetch it. That
       would bypass R22.
     - Honest clients are unaffected: a push sends every object that is
       not reachable from the refs the server advertised.
     - Cost: the walk is proportional to the new objects, the same order
       as receive-pack's own connectivity check.
     - The operator's `reindex` restore is the one exception, since
       making an unplaced commit reachable is its purpose (item 4).
   - **Invariant I1: every database ref's closure lies in `committed`
     packs.**
     - A writer's snapshot satisfies I1. With the reachability rule, the
       new ref's closure lies in the snapshot's committed set plus the
       writer's own pack.
     - Between snapshot and commit, the committed object set only grows:
       geometric repacks drop nothing, GC never deletes a `committed`
       pack, and `recovered` packs are outside the set. A full repack is
       the only step that removes objects. Its publish bumps
       `full_repack_gen`, which fails the writer's transaction.
     - So every committed transaction preserves I1, and no ref can
       dangle.
   - **If the transaction fails,** for any reason (a CAS mismatch, a
     fence mismatch, or a pack that is no longer `pending`), it rolls
     back as a whole.
     - The pack then becomes `lost` through `UPDATE … SET state = 'lost'
       WHERE id = $pack AND state = 'pending'`, in its own transaction.
       A fence failure therefore ends in `lost`, exactly like a CAS
       failure.
     - If the node dies before that update, the row stays `pending`, and
       GC expires it (item 5).
     - A pack that GC already expired matches neither update and stays
       `expired`. It can never be committed.
   - **Retries.**
     - A push that fails the fence gets a retryable `ng`, and the client
       pushes again with a new pack.
     - Hub writes retry inside openxet, up to 3 times. Each attempt takes
       a fresh snapshot and, for commits, builds a new pack with a new
       row. A pack row is never reused across attempts. After that, the
       Hub API returns a retryable error.
   - It answers `ok` or `ng <ref> <reason>`, and the reason reaches the
     client verbatim.
   - A pack is durable in the bucket before any ref points to it.
   - **Known limitation: thin-pack bases.**
     - A thin push may send a delta against a base it doesn't send.
       `index-pack --fix-thin` then copies that base from the snapshot
       into the pushed pack.
     - The base itself never becomes reachable, but the new object's
       content is derived from it. A writer who knows an unreachable
       object's OID and its exact size could use this to reconstruct its
       content.
     - It needs write access to the same repo. Sizes are not served,
       because `object-info` is refused (item 2).
     - Checking that every appended base is reachable is part of the
       `git-reachability` follow-up, since bitmaps can answer it for
       trees and blobs.
   - **The database ref log** is the `ref_log` table, and this item
     defines it.
     - Every ref writer inserts its rows in the commit transaction above.
     - Rows are retained for at least the grace period plus the maximum
       pending-pack age.
     - The bucket ref log of §5.3 is derived from these rows within the
       documented RPO (60 s). It is never written independently.
4. **Pack sets, hydration and eviction.**
   - **Visible packs.** A new request's pack set is exactly the
     `committed` packs, read in the same database snapshot as its refs
     and `full_repack_gen`.
     - The read is from the primary or a single consistent snapshot,
       never from two replicas with different lag (§5.8).
     - `pending`, `lost`, `expired`, `superseded` and `recovered` packs
       are never exposed to new snapshots, receive-pack's included.
     - Every request builds such a snapshot, Hub API reads and writes
       included (item 6).
     - A request already in flight keeps the hard links it took.
   - **Why exclude superseded packs.** A full repack drops expired
     objects, but they would still sit in the superseded inputs. A push
     could then pass connectivity against them, and its ref would dangle
     once GC deleted those inputs. The same reasoning excludes pending
     and lost packs: a push must never lean on another in-flight or lost
     pack.
   - **Hydration.** A node downloads the committed files it lacks:
     `.pack`, `.idx`, `.rev`, `.bitmap`, `.mtimes`, and the generation's
     `multi-pack-index` with its `.bitmap` and `.rev`.
     - It pins each pack's mtime to the row's `created_at`.
   - **Snapshot filesystem.** Snapshot directories live on the cache's
     filesystem, because hard links can't cross filesystems (`EXDEV`).
     - At start-up, openxet checks this with `st_dev`, and with a probe
       link, and fails fast with an actionable error.
     - Also at start-up it sweeps leaked snapshot directories.
   - **Eviction.**
     - It counts a cached file's space as freed only when the link
       count shows no snapshot holds it, and prefers files with
       `st_nlink == 1`.
     - Superseded packs leave the cache once no snapshot links them.
       Before that they cost no correctness, only disk.
     - Evicting `lost` packs is defensive only, since they are never
       hydrated.
   - **Stale bitmaps.** A superseded pack's `.bitmap` can make git slower
     to serve, but new snapshots never contain superseded packs, so only
     requests already in flight see it.
   - Pushed packs are never exploded (`receive.unpackLimit=1`).
   - **Rebuilding (`reindex`, §5.3).** It runs with the repo's writes
     stopped. It rebuilds refs, `ref_log` rows and the pack list from the
     bucket, and starts a fresh `full_repack_gen`.
     - Refs and `ref_log` rows come from the bucket ref log. If a ref's
       closure is incomplete in the listed packs, the ref falls back to
       its newest earlier ref-log value whose closure is complete, and
       reindex reports it.
     - Every pack under `repos/<id>/packs/` is listed. A pack that holds
       any object reachable from a rebuilt ref is `committed`. Objects
       duplicated across committed packs are harmless, and the next full
       repack folds them.
     - Every other pack becomes **`recovered`**, never `pending`. It may
       hold commits pushed after the last ref-log entry that reached the
       bucket, and GC must never take those.
     - **`created_at`** of a reindexed pack is the bucket's
       `Last-Modified` time of its `.pack` object. Packs are written
       once, so that is the publish time. If the store does not report
       it, reindex uses its own start time, which only makes cruft
       expiry later.
     - **Unplaced commits** are commits that no rebuilt ref reaches, in a
       `recovered` pack or in a committed pack that is not a cruft pack.
       `openxet reindex` lists them with their packs. `openxet verify`
       keeps listing them as recoverable until an operator resolves
       them.
     - Objects that sit only in cruft packs were already unreachable from
       every ref and keep ref at the last full repack. They keep their
       normal cruft lifecycle and are listed for information only.
     - `multi-pack-index` files are not reindexed. The next geometric
       repack writes a new one.
   - **The `recovered` state.**
     - A `recovered` pack is never exposed to a snapshot, never a repack
       input, and never collected by GC.
     - While a repo has any `recovered` pack or unplaced commit, its full
       repack is held, because it is the only step that drops objects.
       Geometric repacks still run over committed packs.
     - Only an explicit operator action, after review, ends the state:
       - **Restore** an unplaced commit as a ref. openxet takes a
         snapshot of the committed packs plus the recovered packs that
         hold the commit's closure, and checks connectivity against it.
         The commit transaction (item 3) then also moves those packs
         `recovered` → `committed`, with `WHERE state = 'recovered'`
         matching every one.
       - **Discard.** The remaining recovered packs go `recovered` →
         `lost` with `WHERE state = 'recovered'`, and GC deletes them
         after the grace period. Unplaced commits in committed packs
         return to the normal cruft lifecycle.
     - Once nothing is left to resolve, the hold lifts.
5. **Repacking, the repack fence and orphan reclaim.** These run on one
   leaseholder per repo.
   - **Lease fencing.** The lease row carries an `epoch` that increases
     with every acquisition. Every repack publish transaction, geometric
     or full, requires `epoch = $epoch AND holder = $me AND expires_at >
     now()`, on the database clock. A leaseholder that has lost its
     lease, even one paused mid-publish, therefore can't publish.
   - **Inputs.** The working copy is built from `committed` packs only.
     Pending, lost and recovered packs are never inputs.
   - **Outputs.** Repack outputs follow the pushed-pack pattern. Each row
     is inserted as `pending` before its files are uploaded, and the
     publish moves it to `committed` with `WHERE state = 'pending'`. A
     failed publish marks the outputs `lost`, and a crashed one leaves
     them `pending` for GC to expire.
   - **Snapshot.** The leaseholder records the `ref_log` high-water mark
     and `full_repack_gen` when it takes the working-copy snapshot.
   - **mtimes.** Right before each repack, the leaseholder re-pins every
     input pack's mtime from the database `created_at`.
     - Why: hard links share an inode, and git may freshen a pack's mtime
       while serving. Re-pinning makes the leaseholder's view
       deterministic.
     - It costs one `utime` per pack, and needs neither copies of
       multi-GB packs nor a separate uid that would complicate
       deployment.
   - **Extra roots.** Before every repack, the leaseholder writes
     temporary refs `refs/openxet/keep/<n>` into the working copy. They
     hold every old and new OID in the database `ref_log` within the
     grace window.
     - Objects that became unreachable less than a grace period ago
       (force-pushed or deleted refs) therefore stay reachable for the
       repack, whatever their mtime.
     - Example: a commit on branch A is force-pushed away while a pending
       push on branch B builds on it. The commit survives.
     - These keep refs exist only in the working copy. They never
       become database refs.
     - This avoids `pack.recentObjectsHook`, which would raise the floor
       to git 2.42.
   - **Geometric repack:** `git repack --geometric=2 -d --write-midx
     --write-bitmap-index`.
     - It rolls packs up with `pack-objects --stdin-packs`, which keeps
       every object of every input pack (source: git v2.39.5
       `builtin/repack.c` line 937; `git-pack-objects(1)`
       `--stdin-packs`). It drops nothing, so it needs no fence.
     - It leaves cruft packs alone (source: `builtin/repack.c` line 355,
       `if (p->is_cruft) continue`).
   - **Full repack:** a periodic
     `git repack -adb --cruft --cruft-expiration=<grace>`. It is held
     while the repo has anything left to resolve after `reindex`
     (item 4).
     - Objects that are unreachable even from the keep refs go into a
       cruft pack.
     - git expires cruft objects by **object mtime**, the pinned pack
       creation time recorded per object in the cruft pack's `.mtimes`.
       That is not the time an object became unreachable; the keep refs
       cover that gap.
     - Each cruft pack's `.mtimes` is a pack artifact, uploaded and
       hydrated with the pack.
   - **The repack fence, in both directions.** A full repack can drop a
     long-unreachable object that an in-flight push builds on. Two
     checks prevent that:
     - **Push after repack.** A full-repack publish increments
       `repos.full_repack_gen`. The push CAS requires the generation that
       was read in the push's snapshot (item 3). So a push whose
       connectivity check used the old generation's packs can't commit.
     - **Repack after push.** The full-repack publish transaction aborts,
       and retries later, if `ref_log` has advanced past the high-water
       mark recorded with the working copy. So a repack never publishes
       a pack set computed without a committed push's refs.
     - Geometric publishes take neither fence, since they drop nothing.
       They still take the lease check.
   - **Publishing.** The leaseholder publishes the new packs and swaps the
     pack list in one database transaction. It commits only if the lease
     check holds, every output matches `WHERE state = 'pending'`, and
     every input matches `committed` → `superseded` (with a timestamp)
     `WHERE state = 'committed'`. Otherwise it aborts as a whole.
     - Superseded bucket objects are kept for the full grace period, so
       in-flight requests and `reindex` can still read them.
   - **GC** (§5.6, default grace 7 days) deletes:
     - `superseded` packs past their grace
     - `lost` packs past their grace
     - `pending` packs older than the grace period, first marked
       `expired` with `WHERE state = 'pending'` so no CAS can commit them
     - never a `committed` or `recovered` pack, and never a bucket object
       that no row describes
   - **Orphan reclaim.**
     - Lost pushes' objects are reclaimed with their pack. They were
       never exposed, and no ref reaches them.
     - Unreachable objects in committed packs leave through cruft expiry,
       once they are older than the grace period by mtime and outside
       the keep-ref window.
6. **gitoxide is used in-process only off the wire path, and only on a
   request snapshot.** The Hub API obeys the same fence, committed set
   and R22 rules as Git.
   - **Uses:**
     - reading objects for Hub API reads
     - the reachability walk of item 2
     - writing Hub API commits as small packs that go through the same
       publish-then-commit path as pushes
   - **Snapshot odb.** Every gix read or write opens the request's
     snapshot repo (items 1 and 4: refs, committed pack set and
     `full_repack_gen` from one database read). It never opens the node
     cache directory.
     - Why: the cache can still hold superseded packs whose expired
       objects a full repack has dropped. gix reading them could serve
       an object, or build a commit on one, that is in no committed pack.
   - **Hub reads by revision:** `tree`, `resolve`, `paths-info`,
     `commits`, repo info, and the source revision of a commit's copy
     operation.
     - The revision is either a ref name, resolved in the snapshot, or a
       commit or tag OID that passes R22 from the snapshot's refs: a ref
       tip or peeled tag target, otherwise the item 2 walk within its
       budget, failing closed.
     - A failure gets the same "revision not found" answer as a missing
       object.
     - Tree and blob OIDs are not accepted as revisions. Paths are read
       inside the commit's tree, which is reachable by construction.
     - So the Hub API serves exactly what a git fetch would; there is no
       difference from R22.
     - If the compatibility suite shows clients sending abbreviated OIDs,
       they expand only to commits that pass the same check.
     - A response cache keyed by OID still runs the check on each
       request.
   - **Hub commits**, and annotated tags created through the Hub API,
     take these steps:
     1. Take a snapshot. The parent is the branch tip in it, and must
        equal `parent_commit` when the client sends one.
     2. Build the new objects with gix, reading from the snapshot odb and
        writing one pack into the request's quarantine directory.
     3. Index that pack with `git index-pack --strict`, the counterpart
        of `receive.fsckObjects`.
     4. Check connectivity against the snapshot. This is the check
        receive-pack runs: `git rev-list --objects <new commit> --not
        <snapshot ref tips>`, with the quarantine as the object directory
        and the snapshot as its only alternate. Every object must be
        found.
     5. Apply the writer reachability rule (item 3).
     6. Insert the pack row as `pending`, upload, and run the commit
        transaction (item 3), fenced by `full_repack_gen`.
   - **Hub ref creates and updates** to a revision (`create_branch`,
     `create_tag` with `revision=<sha>`):
     - take a snapshot and resolve the revision in its odb, so the
       target must be an object in the snapshot's committed pack set
     - apply the writer reachability rule with no pack, so the target
       must also be reachable from the snapshot's refs under R22's
       rules, which by I1 puts its whole closure in the committed set
     - run the commit transaction, fenced by `full_repack_gen`
   - **Hub ref deletes** use the same fenced transaction. They can't make
     a ref dangle, but they write `ref_log`, and one transaction shape
     for every writer is simpler to test.
7. **Hardening of the hook route and the git process.**
   - **Hook channel.** It is an inherited `socketpair` fd with no
     filesystem path, over HTTP and SSH alike.
     - openxet clears `FD_CLOEXEC` on that one fd for the git child,
       which passes it on to the hook.
     - The hook finds it through `OPENXET_HOOK_FD`.
   - **Token.** Each request gets a single-use token, delivered as
     `OPENXET_HOOK_TOKEN`. It is bound to repo ID, principal and request
     ID, and expires when the request ends.
   - **What openxet trusts.** It derives the repo and authorization only
     from the token's request context, never from data the hook supplies.
   - **Pack path.** The path the hook reports must resolve (after
     `realpath`) inside that request's snapshot directory, or its
     quarantine directory, and nowhere else.
   - **Sanitised environment for every git process.** It starts empty and
     contains only these variables:
     - `PATH`: a fixed list, the git exec dir and openxet's hook dir
     - `HOME`: a private, empty directory
     - `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL=/dev/null`
     - `GIT_PROTOCOL`: only a validated `version=0`, `version=1` or
       `version=2`
     - `OPENXET_HOOK_FD` and `OPENXET_HOOK_TOKEN`, for receive-pack only
   - Hooks run from openxet's directory only, via `core.hooksPath`.
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
    shallow, bitmaps, `fsckObjects` and quarantine. Partial clone is
    available once `allowFilter` is enabled through an ADR amendment.
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
  - openxet must parse v0/v1/v2 upload-pack wants and keep the
    reachability check in step with git's protocol. Fail-closed budgets
    can refuse rare deep wants in huge repos until the bitmap follow-up
    (`git-reachability`) lands.
  - Snapshots link a committed pack set instead of pointing at the whole
    cache, and they must share the cache's filesystem. Repack writes
    temporary keep refs and re-pins mtimes, and `.mtimes` files must be
    carried through the bucket.
  - The repack fence can refuse a push or Hub write that races a full
    repack. The refusal is retryable, Hub writes retry internally, and
    full repacks are rare.
  - Hub API reads and writes build the same per-request snapshot as Git
    requests. Hub reads by commit OID pay the R22 walk when the OID is
    not a ref tip.
  - After database loss, a repo with unplaced commits keeps its
    `recovered` packs, and skips full repacks, until an operator reviews
    them.
  - Hook plumbing: an inherited socketpair and tokens.
  - Large repos need repo-affinity routing to keep caches warm.
- **Not yet proven: the pack lifecycle must be prototyped first, in M2.**
  - The pack lifecycle above is intricate, and it came out of review, not
    the spike. None of it is prototyped.
  - Before any production use, M2 builds it as a prototype with
    property-based tests (random interleavings of push, Hub ref create,
    update and delete, Hub commit, geometric repack, full repack, lease
    takeover, GC, node hydration across nodes, and reindex with operator
    restore and discard) and the fault-injection tests above. Each
    interleaving asserts I1 (item 3) and that nothing R22 refuses is
    served.
  - Each rule below must be covered by at least one test:
    - [ ] pack state machine: `pending` → `committed` | `lost` | `expired`;
      `committed` → `superseded`; reindex → `committed` | `recovered`;
      `recovered` → `committed` | `lost`, by operator action only. Every
      transition is guarded by its `WHERE state =` clause.
    - [ ] the commit transaction holds ref CAS, pack commit, the
      `full_repack_gen` check and the `ref_log` insert, atomically, for
      every ref writer. Any failure, the fence included, ends in `lost`
      or, after a crash, in `pending` and then `expired`.
    - [ ] Hub ref writes and Hub commits obey the fence and the committed
      set: gix reads and writes only on the request snapshot, Hub commits
      pass a connectivity check against it before the commit
      transaction, and Hub ref creates and updates target only objects
      R22 accepts
    - [ ] the writer reachability rule, for pushes and Hub writes alike
    - [ ] Hub reads by OID pass R22
    - [ ] the v2 command allowlist: only `ls-refs` and `fetch`
    - [ ] the lease epoch check at every repack publish
    - [ ] `recovered` packs are never exposed, never repack inputs and
      never collected, and the full repack is held while any remain
    - [ ] `ref_log` has the retention of item 3, and the bucket ref log is
      derived from it, for every ref writer
    - [ ] new snapshots expose exactly the `committed` packs of one
      database snapshot, refs included. In-flight requests keep their
      links.
    - [ ] a push's own pack is visible only through its quarantine
    - [ ] hydration pins mtimes, and the leaseholder re-pins before a
      repack
    - [ ] keep refs cover the `ref_log` grace window during every repack
    - [ ] geometric repacks drop nothing and take no fence
    - [ ] full-repack fence in both directions: the `full_repack_gen` bump,
      and the publish abort on `ref_log` advance
    - [ ] `.mtimes`, `.rev` and MIDX files travel through the bucket
    - [ ] GC deletes only superseded, lost and expired packs past the
      grace period. Superseded bucket objects survive their grace period.
    - [ ] snapshot root on the cache filesystem, a sweep at start-up, and
      link-count-aware eviction
    - [ ] R22 reachability within its budget
    - [ ] `reindex` rebuilds refs, `ref_log` and the pack list from the
      bucket, and lists unplaced commits as recoverable
- **Must stay true, with the tests that enforce it:**
  - `publish_then_cas_survives_faults`, a fault-injection test. It kills
    the node, or interleaves operations, at these points:
    - between publish and CAS
    - between CAS and ack
    - during a full repack that runs while a push sits between publish
      and CAS
    - during force-push-then-dependent-push: branch A is force-pushed
      away, a push to branch B building on A's old commit is held between
      publish and CAS, and a full cruft repack and GC run with a short
      grace
    - a full repack that publishes while a push, whose snapshot read the
      previous `full_repack_gen` and builds on a long-unreachable object,
      sits between publish and CAS. The push's CAS must fail on the
      fence.
    - a push that commits while a full repack computes from an older
      `ref_log` high-water mark. The repack's publish must abort and
      retry.
    - a Hub `create_branch` or `create_tag` with `revision=<sha>`, held
      between its snapshot (generation g) and its commit transaction,
      while a full repack publishes g+1 and then GC deletes the
      superseded inputs. The Hub transaction must fail on the fence and
      retry from a fresh snapshot. No ref may point at a dropped object.
      The test runs it twice: once with an object that is reachable, and
      once with an object that has been unreachable for longer than the
      grace period, which the writer reachability rule must refuse
      before any transaction.

    It asserts that no ref points at a missing object, that no
    acknowledged push is lost, and that the pending pack is neither a
    repack input nor deleted before its grace period. Once GC has expired
    a pending pack, its CAS fails cleanly.
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
    - the hook channel is an inherited fd, with no path and no listener
    - only that fd has `FD_CLOEXEC` cleared
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
  - `lost_packs_not_hydrated` and `pending_packs_aged_out_by_gc`. The
    lost-pack update and the expiry update both match only
    `state = 'pending'`.
  - `push_cannot_use_foreign_pending_objects`. A push whose commits
    reference an object that exists only in another `pending` or `lost`
    pack is rejected by receive-pack's connectivity check.
  - `snapshot_exposes_only_its_pack_set`. A new snapshot's
    `objects/pack` holds exactly the `committed` pack set read with its
    refs. The test checks that:
    - there are no alternates
    - no `superseded`, `pending` or `lost` pack is linked, for upload-pack
      and receive-pack alike
    - an in-flight request keeps its links across a repack publish
  - `push_cannot_use_superseded_objects`: after a full repack drops an
    expired object, a push that references it is rejected by the
    connectivity check.
  - `ref_log_in_cas_transaction`:
    - pushes, Hub API ref creates, updates and deletes, and Hub commits
      write their `ref_log` rows in the commit transaction, which also
      checks `full_repack_gen`
    - the bucket ref log is derived from those rows
    - rows are kept for at least the grace period plus the maximum
      pending age
  - `snapshot_dir_same_filesystem` and `stale_snapshots_swept`: start-up
    fails fast when the snapshot root and the cache are on different
    filesystems, and leaked snapshot directories are removed.
  - `cas_requires_pending_pack`: a pack marked `expired` by GC can't be
    committed by a late CAS.
  - `failed_commit_marks_pack_lost`: a commit transaction that fails on
    the CAS, on the fence or on a non-pending pack rolls back as a whole
    and leaves the pack `lost`. With a crash injected before that update,
    the pack stays `pending` and GC expires it.
  - `hub_ref_write_fenced`:
    - a Hub ref create, update or delete whose snapshot read an older
      `full_repack_gen` fails and retries
    - a Hub ref create or update to an OID that is not reachable from
      the snapshot's refs is refused, a reachable non-tip commit is
      accepted, and a target in no committed pack is refused
  - `hub_commit_on_snapshot`:
    - with a superseded pack still in the node cache that holds an
      object no committed pack has, a Hub commit (including a copy
      operation whose source names that object) can neither read nor
      reference it
    - a Hub commit whose objects are not connected in the snapshot fails
      before the commit transaction, and nothing it wrote is visible
  - `writer_cannot_make_unreachable_reachable`: a push, or a Hub write,
    that points a ref at, or names as a parent, an object that is in a
    committed pack but unreachable is refused. The same push succeeds
    when it sends that object itself.
  - `hub_read_by_sha_obeys_r22`: `tree`, `resolve`, `paths-info` and
    `commits` at a commit OID succeed for a reachable non-tip commit, and
    answer "revision not found" for a force-pushed-away commit, a tree
    or blob OID, and a commit beyond the walk budget.
  - `v2_command_allowlist`:
    - the advertisement lists only `agent`, `ls-refs`, `fetch` without
      `filter`, `server-option` and `object-format`
    - `command=object-info`, `command=bundle-uri` and an unknown command
      are refused by openxet and never reach git
    - it runs against git 2.37 (where git itself serves `object-info`)
      and against the newest supported git
  - `repack_publish_lease_fenced`: a leaseholder whose lease expired or
    was taken over can't publish, geometric or full, and its pending
    outputs end `lost` or `expired`. A publish whose inputs are no longer
    all `committed` aborts.
  - `reindex_keeps_unplaced_commits`: after deleting the database,
    reindex from a bucket whose ref log lags the last push:
    - the rebuilt refs have complete closures
    - unreached packs are `recovered`, get `created_at` from the bucket,
      and survive GC
    - unplaced commits are listed by `reindex` and `verify`
    - the full repack is held until an operator restores or discards
      them, and a restore makes the commit fetchable
  - `hydrated_pack_mtime_pinned`:
    - hydrated pack files carry the row's `created_at`
    - the leaseholder re-pins mtimes right before each repack
    - a cruft pack's `.mtimes` is uploaded and hydrated, and so are the
      `.rev` and MIDX `.rev` files
  - `reachability_budget_enforced`:
    - one walk per request across all wants
    - the commit and time limits and the per-repo and per-principal
      concurrency limits apply
    - tree and blob wants not directly referenced are refused
    - v0/v1 wants are checked too
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
