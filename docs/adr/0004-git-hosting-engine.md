# 0004: Serve Git with the `git` binary over OpenXet-owned refs and bucket packs

<!-- description: Git is served by the git binary over refs in OpenXet's database and packs in the bucket. -->

- Status: accepted
- Date: 2026-10-23
- Scope: `git`, `ssh`, `storage`, `index`, `gc`, `server`, `hub`
- Amended: 2026-10-23, in the M0 design reconciliation (rulings on the
  ADR's review): item 3's thin-base predicate and its known limitation,
  item 4's `tag` entries and discard semantics, item 5's GC of side files
- Amended: 2026-10-25, in M2 Task 8 (found while building receive-pack):
  item 7's hook channel is a pair of pipes, not a socketpair; item 3's
  thin-pack hint is `git -c pack.window=0 push`, because git ignores
  `--no-thin` over smart HTTP; item 1's `core.alternateRefsCommand=true`
  is written so that it runs without `PATH`; item 3's policy re-check
  fences stale updates by the old value and accepts non-fast-forward
  updates, since a forced push can't be told apart on the wire, and
  `main` can't be deleted (branch protection is a later feature)
- Amended: 2026-10-25, in M2 Task 9 (found while building repack, the
  sweep and reindex): item 5's repack outputs keep their oldest input's
  `created_at`; a pack row records when it was committed, which dates a
  multi-pack-index generation's retirement; the sweep keeps files another
  row still names; item 4's reindex commits cruft packs and logs the refs
  it rolls back; item 3's ref log retention is pruned by the sweep
- Amended: 2026-10-25, in M2 Task 9's review: item 5's sweep ages
  in-flight rows by `inserted_at`, not `created_at`, holds the repack
  lease, and deletes files only under a deletion claim in the index; the
  leaseholder renews its lease while it works; GC refuses a grace period
  below 28 days; item 4's ref fallback stays within the ref's current
  lifetime, and discard is one transaction whose lost packs keep a full
  grace period; reindex holds the repack lease, and maintenance finishes
  stale deletion claims
- Amended: 2026-10-26, in M2 Task 20: item 3's `git.thin_bases`
  defaults are measured, no longer placeholders (`max_objects`
  1,000,000, `max_ms` 2,000)

## Context

Design §10.9 makes OpenXet a Git host: smart HTTP and SSH, `upload-pack`
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
  216 MB. git without bitmaps needed 1.2 to 1.6 s CPU for the full clone.
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
  - A cold node hydrated 80 MB of packs from MinIO in 0.4 to 0.6 s.
  - A geometric repack with a MIDX bitmap took 0.7 s for 19 packs, and
    left the base pack untouched.
- **Reachability (source).** git's v2 `parse_want` accepts any object in
  the odb, alternates included. git 2.37 to 2.44 also advertises and serves
  the v2 `object-info` command unconditionally, which returns the size of
  any OID in the odb. Reachability is therefore OpenXet's job (item 2).
- **Alternatives (source).**
  - JGit DFS fits the storage model best, but needs a JVM sidecar (start-up
    alone 1.6 to 2.0 s CPU, about 77 MB, observed), and the DFS API is
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
   - For each request, OpenXet builds a snapshot repo in a temp dir on
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
   - OpenXet never runs `git http-backend`; it is the HTTP layer itself.
   - Pinned config in every snapshot:
     - `uploadpack.allowAnySHA1InWant=false`
     - `uploadpack.allowReachableSHA1InWant=true`
     - `uploadpack.allowRefInWant=false`, so every want is an OID that
       OpenXet checks
     - `uploadpack.allowFilter=false` until partial clone is designed
       and tested; turning it on needs its own ADR amendment
     - `core.alternateRefsCommand=true`, as defence in depth: snapshots
       have no alternates. It is written `: no alternate refs`: git looks
       a value without shell metacharacters up on the sanitised `PATH`
       (item 7), which has no `true`, and receive-pack, whose quarantine
       has the snapshot as an alternate, then printed `cannot run true`
       to every pusher. With a space the value runs through `/bin/sh`,
       whose `:` builtin prints nothing, as `true` would.
     - `receive.procReceiveRefs=refs/`
     - `receive.fsckObjects=true`
     - `receive.unpackLimit=1`
     - `core.hooksPath` set to OpenXet's own hook directory
     - `transfer.advertiseObjectInfo=false`,
       `uploadpack.advertiseBundleURIs=false`, `promisor.advertise=false`
       and `transfer.advertiseSID=false`. Older git ignores some of these
       keys (item 2 lists the versions), so they are defence in depth.
       The command allowlist in item 2 is the control that holds from
       2.37 on.
2. **OpenXet answers the ref-independent parts in-process, and enforces
   reachability for every `fetch` want (R22).**
   - **In-process:** the protocol v2 capability advertisement and
     `ls-refs` (from the database). The advertisement is captured from the
     installed git at start-up, with the pinned config, and then filtered
     to the allowlist below.
   - **v2 command allowlist.** The interposer accepts exactly two
     commands: `ls-refs`, which OpenXet answers, and `fetch`, whose wants
     it checks before forwarding. Any other `command=`, including
     `object-info`, `bundle-uri` and unknown names, gets an `ERR` from
     OpenXet and never reaches git. The advertisement lists only `agent`,
     `ls-refs`, `fetch` (without `filter`), `server-option` and
     `object-format`. Adding a command needs an ADR amendment.
     - The same holds one level up. Over SSH, only `git-upload-pack` and
       `git-receive-pack` are accepted, and `git-upload-archive` or any
       other command is refused. Over HTTP, only the two smart services
       exist; the dumb protocol's `objects/` paths are not served.
     - **What git ≥ 2.37 serves by default** (source: git `serve.c`, the
       `capabilities[]` table, at tags v2.37.0 to v2.51.0):
       - `ls-refs` and `fetch`, always
       - `object-info` in 2.37 to 2.44, always (`.advertise =
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
       inside git, but on 2.37 to 2.44 only OpenXet's allowlist does.
   - **Why.** git's v2 `parse_want` only checks that the object exists in
     the odb. It does not check reachability, so
     `allowAnySHA1InWant=false` does not protect v2 (source: git v2.39.5
     `upload-pack.c`, `parse_want`, lines 1413 to 1442). This is git's
     documented design, not a defect: its fetch protocol is not meant to
     keep unshared objects from a peer (git `transfer-data-leaks.adoc`,
     SECURITY), so access control on git reads is OpenXet's job.
   - **Which requests.** OpenXet interposes on the request stream. It
     checks wants before forwarding them to git, so git never sees an
     unchecked want. This covers:
     - every v2 `command=fetch`, including several commands in one
       stateful SSH session
     - the v0/v1 want lines over HTTP and SSH

     Checking v0/v1 too means OpenXet does not depend on git's
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
   - **Known limitation: `have` and `shallow` existence oracles.**
     - git ACKs a `have` whose object is in the snapshot's packs,
       reachable or not. A client's `shallow <oid>` lines are likewise
       treated differently when the object exists.
     - That confirms an object exists, but never sends it.
     - It still matters, because a blob OID is a hash of its content. A
       reader can confirm a guessed low-entropy content, such as a short
       secret or a config value, by asking whether its blob exists.
     - Filtering haves and shallow lines through the same walk is part
       of the `git-reachability` follow-up. Until then, this is a
       documented residual.
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
     OpenXet process. OpenXet then:
     1. validates the request (item 7)
     2. re-checks the commands and branch policy itself: each command's
        old value must equal the ref's value in the snapshot, which fences
        a stale update (a client checks fast-forward against that old
        value, so a non-forced push is a fast-forward of the server's
        ref); a non-fast-forward update is accepted, because a forced
        push (`+ref`) can't be told apart from one on the wire, as with
        git's own default `receive.denyNonFastForwards=false`; `main`
        can't be deleted. Branch protection is a later feature.
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
     <snapshot ref tips>` must be one the writer **sent itself**.
     - **Sent means sent by the client, not appended by git.**
       receive-pack always runs `index-pack --fix-thin` (source: git
       v2.39.5 `builtin/receive-pack.c` line 2263), and `index-pack`
       appends every missing delta base, in full, from the snapshot to
       the end of the pushed pack (`builtin/index-pack.c`,
       `fix_unresolved_deltas` → `append_obj_to_pack`, line 1430).
       Those appended bases are not sent objects.
     - **How OpenXet tells them apart.** OpenXet is the transport layer,
       so it reads the incoming pack header's object count and counts
       the bytes of the client's pack. Appended objects are exactly the
       entries whose `.idx` offset lies past the end of the client's
       data, after its N entries.
     - For a Hub commit, the sent objects are the ones OpenXet built
       from the request. The Hub pack is written whole, with no thin
       bases.
     - **Every appended base must be reachable from the snapshot's
       refs.** A delta the client sent can copy bytes out of its base, so
       the content of a sent object can be derived from an unreachable
       base. OpenXet checks each appended base in this order:
       1. **Fast paths.** The base is the root tree of, or the entry at
          the path where the client's delta uses it in, the tree of a
          boundary commit (`git rev-list --boundary N --not <snapshot
          ref tips>`) or of any snapshot ref tip. Boundary commits and
          tips are reachable by definition, so a match proves the base
          reachable. The path comes from the sent trees, so each check is
          one path lookup per candidate tree, never a tree walk.
       2. **Fallback.** Bases no fast path matched share one full walk
          of the objects reachable from the snapshot's refs, trees and
          blobs included. It has its own budget, separate from R22's
          walk budget:
          - `git.thin_bases.max_objects` and `git.thin_bases.max_ms`,
            by default 1,000,000 objects and 2,000 ms, measured in M2
            Task 20 (`crates/git/tests/thin_bases_budget.rs`,
            [benchmarks](../reference/benchmarks.md#thin-base-budget)).
            Over the 10,001-commit test repository (90,074 objects) the
            walk placed a root-commit base after 90,022 objects; a full
            `rev-list --objects` took 93 to 188 ms on CI's Linux runners
            and 110 ms on the local Apple M3 (479 to 969 objects per ms),
            and a push through the walk 113 to 215 ms. 2,000 ms, R22's
            limit, leaves that push 9 to 17 times its time. In 2,000 ms
            the walk covers 0.96 to 1.94 million objects on those
            machines, so 1,000,000 objects binds first on all but the
            slowest, and nodes answer a push alike; it is 11 times that
            repository and 4.6 times S9's (218,574 objects).
          - The fallback walk shares R22's concurrency limits
            (`git.reachability.max_concurrent_per_repo` and
            `max_concurrent_per_principal`), so concurrent pushes can't
            stack full walks.
       3. **Fail closed.** A base not found within the budget refuses
          the push, with a hint to fetch and push again with
          `git -c pack.window=0 push`, which sends no thin bases. Over
          smart HTTP, git ignores `git push --no-thin`: git v2.39.5
          `transport-helper.c` lists `TRANS_OPT_THIN` among the options a
          remote helper never receives, and `remote-curl` always passes
          `--thin` to `send-pack` (observed with `GIT_TRACE`). Without a
          delta search (`pack.window=0`) pack-objects computes no new
          deltas, and in the stale-client test, whose new objects were
          loose, the retry sent no thin bases. It can still reuse a delta
          already stored in the client's packs against a base it believes
          the server has, so the hint is not a guarantee; such a push
          stays refused, never accepted wrongly.
       - Stock git takes its default thin-pack bases from the edge
         commits' trees at the same path (source:
         `builtin/pack-objects.c`, `show_edge` → `add_preferred_base`,
         which reads the commit's tree), so a default push from an
         up-to-date full clone takes the fast paths.
       - **Known limitation: some honest thin pushes can be refused.**
         Their bases miss the fast paths, and in a large repo the
         fallback walk can run out of budget. Until the
         `git-reachability` bitmap follow-up, these cases may be refused
         (fail closed, never accepted wrongly):
         - **bitmap-enabled clients**, whose bases can come from anywhere
           in what they believe the server has, not from the edge trees
           at the same path
         - **aggressive shallow edges**, where a shallow client's bases
           come from its own shallow boundary rather than from the
           server's boundary commits
         - **stale clients**, whose view of the server's refs is older
           than the snapshot, so their bases sit in commits that are
           still reachable but are neither boundary commits nor tips
         - **commit-type bases**, which are never tree entries, so no
           fast path can match them

         Each refusal carries the thin-pack hint above, which sends no
         thin bases.
     - A writer with no pack (a Hub ref create or update) must get an
       empty list, so N must already be reachable from the snapshot's
       refs. N is checked exactly like an R22 want (item 2): a ref tip or
       peeled tag target passes, a commit or tag needs the walk within
       its budget, a tree or blob passes only as a direct ref target, and
       everything else is refused (fail closed).
     - **Why.** receive-pack's connectivity check, and gix, accept any
       object present in the snapshot's committed packs. Those include
       objects no ref reaches any more: force-pushed away, not yet
       expired. Without this rule a writer could point a ref at one,
       name it as a parent or tree entry without sending it, or send a
       `REF_DELTA` against it so that `--fix-thin` copies it in. Then
       the writer could fetch it, which would bypass R22.
     - Every refusal under this rule uses the same reason text that git
       gives for a missing object, so that the answer doesn't separate
       "exists but unreachable" from "absent".
     - The sent-object part never refuses an honest client: a push sends
       every object that is not reachable from the refs the server
       advertised. Only the appended-base check can, in the known
       limitation above.
     - Cost: listing the new objects is proportional to them, the same
       order as receive-pack's own connectivity check. Placing appended
       bases costs one path lookup per base and candidate tree on the
       fast paths. The fallback walk is proportional to the repo, not to
       the push, which is why it has its own budget.
     - The operator's `reindex` restore is the one exception, since
       making an unplaced commit or tag reachable is its purpose (item 4).
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
     - Hub writes retry inside OpenXet, up to 3 times. Each attempt takes
       a fresh snapshot and, for commits, builds a new pack with a new
       row. A pack row is never reused across attempts. After that, the
       Hub API returns a retryable error.
     - Hub commits to one branch take turns on each node, so they don't
       spend their retries racing each other; one that waits longer than
       `hub.commit_wait` (default 30 s) for its turn gets a retryable
       error.
   - It answers `ok` or `ng <ref> <reason>`, and the reason reaches the
     client verbatim.
   - A pack is durable in the bucket before any ref points to it.
   - **Known limitation: writers get an existence oracle only.**
     - With the rule above, no content of an unreachable object can be
       made reachable or derived through a push.
     - What remains is existence. A push that names an unreachable
       object fails in OpenXet's check, and one that names an absent
       object fails earlier, in git's connectivity check. The reason
       text is the same, but the timing and the stage of failure may
       still differ.
     - It needs write access to the same repo. It is deferred to the
       `git-reachability` follow-up, together with the `have` oracle of
       item 2.
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
     - At start-up, OpenXet checks this with `st_dev`, and with a probe
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
     - It also holds the repo's repack lease (item 5) for its whole run,
       renewing it, and checks it again right before its index
       transaction. Stopping writes doesn't stop repacks or pack sweeps;
       the lease does. Without it, a sweep holding a deletion claim could
       go on deleting the files of a row reindex had just committed.
     - Refs and `ref_log` rows come from the bucket ref log. If a ref's
       closure is incomplete in the listed packs, the ref falls back to
       its newest earlier ref-log value whose closure is complete, and
       reindex reports it. The fallback only considers values since the
       ref was last created: a ref that was deleted and created again
       never comes back at a value from before the deletion. If no value
       of its current lifetime is complete, the ref is left out.
     - Every pack under `repos/<id>/packs/` is listed. A pack that holds
       any object reachable from a rebuilt ref is `committed`. Objects
       duplicated across committed packs are harmless, and the next full
       repack folds them.
     - A cruft pack (one with a `.mtimes`) is `committed` too: its objects
       were unreachable at the last full repack and keep their cruft
       lifecycle, which only a committed pack has.
     - A ref rolled back to an earlier value, or left out, gets a
       `ref_log` row from `openxet reindex` that records the change, so
       the log and the refs agree.
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
       **Unplaced tags** are annotated tag objects in such packs that no
       rebuilt ref reaches; their target may be any object kind.
     - Objects that sit only in cruft packs were already unreachable from
       every ref and keep ref at the last full repack. They keep their
       normal cruft lifecycle and are listed for information only.
     - `multi-pack-index` files are not reindexed. The next geometric
       repack writes a new one.
   - **Unplaced data is persisted, protected, and never blocks repacks
     (ruling R24).** Full repacks are never blocked, and data is never
     discarded automatically.
     - **The `reindex_unplaced` table.** Reindex persists what it could
       not place, one row per entry: repo, kind, OID, pack and
       `found_at`. There are four kinds:
       - `commit`: an unplaced commit tip
       - `tag`: an unplaced annotated tag, a tip of any target kind.
         Without it, the tag object itself would have no keep root,
         whatever it points at.
       - `pack`: a `recovered` pack
       - `edge`: an object in a committed pack that a `recovered` pack's
         objects reference directly
     - The `openxet reindex` command prints the table, and `openxet verify`
       reads it and lists every entry as recoverable until it is resolved.
     - **Keep roots.** Full repacks run normally. Before each one, the
       leaseholder adds every `commit`, `tag` and `edge` entry whose
       object is in the working copy as an extra keep ref (item 5).
       - Unplaced commits in committed packs therefore keep their whole
         closure.
       - So do the committed-pack objects that recovered commits build
         on. Refs may point at trees and blobs, so `edge` entries need
         no commit.
       - Nothing an operator might restore is dropped.
     - **`recovered` packs** stay out of every snapshot and out of every
       repack input. GC never collects them.
     - **Restore** (`openxet reindex restore <repo> <oid> <ref>`) makes
       an unplaced commit or annotated tag a ref. `<oid>` may be a commit
       or a tag OID.
       - OpenXet takes a snapshot of the committed packs plus the
         recovered packs that hold the object's closure, and checks
         connectivity against it.
       - The commit transaction (item 3) then moves those packs
         `recovered` → `committed`, with `WHERE state = 'recovered'`
         matching every one.
       - The same transaction deletes the `reindex_unplaced` entries
         that the new ref now covers.
     - **Discard** (`openxet reindex discard`, per repo or bulk with
       `--all`, and `--dry-run` for both) ends recovery for a repo.
       - The remaining recovered packs go `recovered` → `lost` with
         `WHERE state = 'recovered'`, and GC deletes them after the
         grace period. That grace period runs from the discard: the same
         update stamps the rows' `inserted_at`.
       - The repo's entries are deleted, so their keep roots go too. Both
         changes are one transaction.
         Unplaced commits and tags in committed packs return to the
         normal cruft lifecycle.
       - **Discard is immediate and irreversible for objects in
         committed packs.** Their mtime is their pack's creation time
         (item 4, hydration), not the discard time. So the next full
         repack drops every one whose pack is older than the grace
         period, with no further grace. Only the recovered packs, now
         `lost`, keep GC's grace period. This is intended: the operator
         has reviewed the entries.
       - `--dry-run` prints the packs, objects and bytes it would
         release, and changes nothing. Its object count is an upper
         bound: a keep ref of the `ref_log` window may still hold some
         of those objects until the window passes. It lists committed-pack objects
         under the heading "deleted at the next full repack, with no
         grace period; this cannot be undone", separately from the
         recovered packs that GC deletes after the grace period. The
         operator docs use the same wording.
     - **Visibility.** OpenXet exports the gauges
       `openxet_reindex_unplaced_entries{repo}` and
       `openxet_reindex_unplaced_oldest_seconds{repo}`.
       - A shipped alert fires when a repo has entries older than
         `reindex.unplaced_alert_after`, default 7 days.
       - The alert says what to do: review the entries, then restore or
         discard.
     - **No auto-discard.** There is no time-based expiry. Data is kept
       until an operator decides.
       - Cost: the recovered packs, and the objects that keep roots hold
         in committed packs, stay stored, and billed to the repo, until
         then.
       - That is bounded by what the bucket held at reindex time.
5. **Repacking, the repack fence and orphan reclaim.** These run on one
   leaseholder per repo.
   - **Lease fencing.** The lease row carries an `epoch` that increases
     with every acquisition. Every repack publish transaction, geometric
     or full, requires `epoch = $epoch AND holder = $me AND expires_at >
     now()`, on the database clock. A leaseholder that has lost its
     lease, even one paused mid-publish, therefore can't publish.
     - The holder renews the lease every third of its TTL while it works,
       keeping the epoch, so a full repack longer than the TTL doesn't
       fence itself. The TTL (10 minutes) only bounds how long a holder
       that died keeps the lease from others.
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
     - A repack's outputs take the oldest input's `created_at`, so an
       object keeps its age through any number of repacks. `created_at`
       is the age of a pack's objects. It drives mtime pinning and cruft
       expiry, and nothing else: GC ages rows by `inserted_at` (below). Otherwise a
       geometric repack would restart the cruft clock of every unreachable
       object it rolls up, and a discard (item 4) would not be immediate.
       Objects that became unreachable recently are held by the keep refs,
       not by their mtime.
     - Why: hard links share an inode, and git may freshen a pack's mtime
       while serving. Re-pinning makes the leaseholder's view
       deterministic.
     - It costs one `utime` per pack, and needs neither copies of
       multi-GB packs nor a separate uid that would complicate
       deployment.
   - **Extra roots.** Before every repack, the leaseholder writes
     temporary refs `refs/openxet/keep/<n>` into the working copy. They
     hold every old and new OID in the database `ref_log` within the
     grace window, plus every `reindex_unplaced` `commit`, `tag` and
     `edge` entry whose object is in the working copy (item 4).
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
     `git repack -adb --cruft --cruft-expiration=<grace>`. It is never
     blocked by unresolved `reindex` entries. They add keep roots
     instead (item 4).
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
   - **GC** (§5.6, default grace 30 days) deletes:
     - `superseded` packs past their grace, counted from `superseded_at`
     - `lost` and `expired` packs past their grace
     - `pending` packs inserted more than the grace period ago, first
       marked `expired` with `WHERE state = 'pending'` so no CAS can
       commit them
     - **In-flight rows are aged by `inserted_at`**, the database clock
       when the row was inserted (or, for a discarded pack, when discard
       made it `lost`). They are never aged by `created_at`: a repack's
       outputs carry their oldest input's, so in an old repository they
       would look expired the moment they are written, and a sweep during
       the repack would delete them.
     - **Minimum grace.** GC and full repacks refuse a grace period below
       28 days, the 21-day client shard cache plus the 7-day dedup key
       (ruling M1-R5), whatever the configuration says.
     - **The sweep holds the repository's repack lease**, checked before
       every deletion claim, so no repack publishes while it decides.
     - never a `committed` or `recovered` pack, and never a bucket object
       that no row describes
     - **Side files go with what they describe.** A pack's `.idx`, `.rev`,
       `.bitmap` and `.mtimes` are deleted in the same GC step as its
       `.pack`. Each geometric publish records its `multi-pack-index`
       generation (with its `.bitmap` and `.rev`) in the publish
       transaction, and GC deletes a generation once a newer one is
       committed and the grace period has passed. Otherwise every repack
       would leak its side files into the bucket.
       - A generation stops being current when a newer one is committed,
         or when no committed pack carries it any more (after a full
         repack). The first is dated by the pack row's `committed_at`,
         which every transition to `committed` stamps on the database
         clock, the second by the carriers' `superseded_at`.
       - A generation number is above every generation the repository's
         rows record and above `epoch << 24`, so two leaseholders of
         different epochs never upload the same generation's files.
     - **Shared names.** A pack's name is its checksum, so two rows can
       name the same files: a pushed pack sent again after a repack
       replaced the first, or a repack that rewrites a pack unchanged
       (which publishes nothing). GC deletes a row's files only once no
       other row names them.
       - A row can be inserted after the sweep read the rows: a resent
         push, a Hub retry, or a byte-identical repack output. So files
         are deleted only under a **deletion claim**, a `pack_deletions
         (repo, name)` row. It is inserted in a transaction that checks
         no `pending`, `committed` or `recovered` row names the pack.
       - While the claim exists, inserting a pack row of that name fails
         with a retryable error. A push answers a retryable `ng` and Hub
         writes retry.
       - The claim is removed once the files are gone. A claim a dead
         sweep left is finished, under the lease, by the next sweep or by
         the server's next maintenance pass (every 10 s), whichever comes
         first. The files are deleted only if the check still holds. A
         claim more than an hour old is logged as a warning, since only
         a sweep that died leaves one. `reindex` drops the
         repository's claims, since it has just listed the bucket.
       - So a late row either stops the claim, and the files stay, or
         waits for the deletion and uploads the files again. A committed
         row is never left without its files.
     - The `ref_log` rows of item 3 older than twice the grace period are
       pruned by the same sweep, and only those the bucket ref log already
       holds.
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
   - **Hook channel.** It is a pair of inherited pipes, one each way,
     with no filesystem path and no listener, over HTTP and SSH alike.
     - OpenXet clears `FD_CLOEXEC` on those two descriptors for the git
       child only, which passes them on to the hook.
     - The hook finds them through `OPENXET_HOOK_FD` (`{read},{write}`)
       and adopts them by opening `/dev/fd/N`.
     - Pipes, not a socketpair: safe Rust has no way to take ownership
       of an inherited descriptor number, and `/dev/fd/N` can be opened
       for a pipe on Linux and macOS, but not for a socket on Linux.
     - **Platform limitation (macOS).** Rust's standard library creates
       pipes on macOS with `pipe()` and sets `FD_CLOEXEC` in a second
       step, so a git child another request spawns in between can inherit
       a new channel's descriptors; on Linux `pipe2(O_CLOEXEC)` is atomic.
       The single-use token, bound to repository, principal and request,
       bounds what such a child could do with them. Production targets
       Linux. Tracked in
       [issue #96](https://github.com/stubbi/openxet/issues/96).
   - **Token.** Each request gets a single-use token, delivered as
     `OPENXET_HOOK_TOKEN`. It is bound to repo ID, principal and request
     ID, and expires when the request ends.
   - **What OpenXet trusts.** It derives the repo and authorization only
     from the token's request context, never from data the hook supplies.
   - **Pack path.** The path the hook reports must resolve (after
     `realpath`) inside that request's snapshot directory, or its
     quarantine directory, and nowhere else.
   - **Sanitised environment for every git process.** It starts empty and
     contains only these variables:
     - `PATH`: a fixed list, the git exec dir and OpenXet's hook dir
     - `HOME`: a private, empty directory
     - `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL=/dev/null`
     - `GIT_PROTOCOL`: only a validated `version=0`, `version=1` or
       `version=2`
     - `OPENXET_HOOK_FD` and `OPENXET_HOOK_TOKEN`, for receive-pack only
   - Hooks run from OpenXet's directory only, via `core.hooksPath`.
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
   - OpenXet checks the version at start-up and fails with an
     actionable error.
   - **Licence.** git (GPL-2.0-only) runs as a separate program over
     pipes, never linked. That is aggregation, so OpenXet stays
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
  - CAS, durability and statelessness are OpenXet code that is
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
  - OpenXet must parse v0/v1/v2 upload-pack wants and keep the
    reachability check in step with git's protocol. Fail-closed budgets
    can refuse rare deep wants in huge repos, and some honest thin pushes
    (item 3), until the bitmap follow-up (`git-reachability`) lands.
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
    `recovered` packs and the objects its keep roots hold. They stay
    stored, and billed, until an operator restores or discards them.
    There is no auto-discard, so an alert on unresolved entries is
    shipped. Full repacks are never blocked.
  - Hook plumbing: inherited pipes and tokens.
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
    - [x] pack state machine: `pending` → `committed` | `lost` | `expired`;
      `committed` → `superseded`; reindex → `committed` | `recovered`;
      `recovered` → `committed` | `lost`, by operator action only. Every
      transition is guarded by its `WHERE state =` clause.
      Tests: `mark_pack_is_guarded`, `packs_list_every_state_and_delete_is_guarded`, `reindex_restore_and_discard` (`crates/index/tests/git_store.rs`); `failed_commit_marks_pack_lost` (`crates/git/tests/receive_pack.rs`); `pending_packs_aged_out_by_gc` (`crates/git/tests/repack.rs`).
    - [x] the commit transaction holds ref CAS, pack commit, the
      `full_repack_gen` check and the `ref_log` insert, atomically, for
      every ref writer. Any failure, the fence included, ends in `lost`
      or, after a crash, in `pending` and then `expired`.
      Tests: `commit_applies_all_updates_or_none`, `commit_fenced_by_full_repack_gen`, `commit_writes_one_ref_log_row_per_update` (`crates/index/tests/git_store.rs`); `ref_log_in_cas_transaction`, `failed_commit_marks_pack_lost`, `publish_then_cas_survives_faults` (`crates/git/tests/receive_pack.rs`, `hub_objects.rs`, `repack.rs`); `pending_packs_aged_out_by_gc` (`crates/git/tests/repack.rs`).
    - [x] Hub ref writes and Hub commits obey the fence and the committed
      set: gix reads and writes only on the request snapshot, Hub commits
      pass a connectivity check against it before the commit
      transaction, and Hub ref creates and updates target only objects
      R22 accepts
      Tests: `hub_ref_write_fenced`, `hub_commit_on_snapshot`, `hub_commit_refusals_change_nothing`, `writer_cannot_make_unreachable_reachable` (`crates/git/tests/hub_objects.rs`).
    - [x] the writer reachability rule, for pushes and Hub writes alike:
      only client-sent objects count, and every `--fix-thin` base must be
      reachable from the snapshot's refs: path-matched fast paths in
      boundary-commit and ref-tip trees, then a full walk under its own
      budget, failing closed with the thin-pack hint
      Tests: `writer_cannot_make_unreachable_reachable`, `stock_thin_push_takes_the_fast_path`, `fallback_walk_places_deep_bases_within_budget` (`crates/git/tests/thin_bases.rs`); `push_cannot_use_foreign_pending_objects` (`crates/git/tests/receive_pack.rs`); `push_cannot_use_superseded_objects` (`crates/git/tests/repack.rs`); `writer_cannot_make_unreachable_reachable` (`crates/git/tests/hub_objects.rs`).
    - [x] the known-limitation clients (bitmap-enabled, shallow, stale)
      either push or are refused with the hint, and succeed as the hint
      says
      Tests: `known_limitation_clients` (`crates/git/tests/thin_bases.rs`).
    - [x] Hub reads by OID pass R22
      Tests: `hub_read_by_sha_obeys_r22` (`crates/git/tests/hub_objects.rs`).
    - [x] the v2 command allowlist: only `ls-refs` and `fetch`
      Tests: `v2_command_allowlist` (`crates/git/tests/upload_pack.rs`); `advertisement_filtered_to_the_allowlist` (`crates/git/src/ls_refs.rs`).
    - [x] the lease epoch check at every repack publish
      Tests: `repack_publish_lease_fenced` (`crates/git/tests/repack.rs`); `publish_repack_rejects_stale_epoch_and_advanced_ref_log`, `lease_epoch_increases_and_expires` (`crates/index/tests/git_store.rs`).
    - [ ] `recovered` packs are never exposed, never repack inputs and
      never collected. `reindex_unplaced` entries (`commit`, `tag`,
      `pack`, `edge`) persist, become keep roots and never block a full
      repack. Only restore or discard clears them, and they have a
      metric and an alert.
      Tests: `recovered_packs_never_exposed`, `reindex_unplaced_persisted`, `reindex_hold_never_blocks_repack` (`crates/git/tests/reindex.rs`); `unplaced_rows_persist_and_clear_by_coverage` (`crates/index/tests/git_store.rs`). Open: the metric and the alert ([#113](https://github.com/stubbi/openxet/issues/113)).
    - [x] unplaced annotated tags stay protected across full repacks,
      and `restore` accepts a tag OID
      Tests: `reindex_unplaced_persisted` (`crates/git/tests/reindex.rs`: the tag of a tree survives a full repack, and `reindex_restore` takes the tag's OID).
    - [x] `discard --dry-run` labels committed-pack objects as deleted at
      the next full repack with no grace period, and discard behaves so
      Tests: `reindex_unplaced_persisted`, `discard_releases_committed_objects` (`crates/git/tests/reindex.rs`).
    - [x] `ref_log` has the retention of item 3, and the bucket ref log is
      derived from it, for every ref writer
      Tests: `ref_log_in_cas_transaction` (`crates/git/tests/receive_pack.rs`, `hub_objects.rs`, and `repack.rs` for derivation and retention); `ref_log_head_and_prune` (`crates/index/tests/git_store.rs`).
    - [x] new snapshots expose exactly the `committed` packs of one
      database snapshot, refs included. In-flight requests keep their
      links.
      Tests: `snapshot_exposes_only_its_pack_set` (in-flight links kept), `snapshot_serves_its_refs` (`crates/git/tests/snapshots.rs`); `snapshot_reads_refs_and_committed_packs_only` (`crates/index/tests/git_store.rs`).
    - [x] a push's own pack is visible only through its quarantine
      Tests: `push_cannot_use_foreign_pending_objects`, `push_round_trip` (`crates/git/tests/receive_pack.rs`); `hub_commit_retries_while_its_pack_is_being_deleted` (`crates/git/tests/hub_objects.rs`).
    - [x] hydration pins mtimes, and the leaseholder re-pins before a
      repack
      Tests: `hydrated_pack_mtime_pinned` (`crates/git/tests/snapshots.rs`, `crates/git/tests/repack.rs`).
    - [x] keep refs cover the `ref_log` grace window during every repack
      Tests: `keep_refs_cover_ref_log_window` (`crates/git/tests/repack.rs`).
    - [x] geometric repacks drop nothing and take no fence
      Tests: `geometric_repack_drops_nothing_and_takes_no_fence` (`crates/git/tests/repack.rs`).
    - [x] full-repack fence in both directions: the `full_repack_gen` bump,
      and the publish abort on `ref_log` advance
      Tests: `full_repack_fence_both_directions` (`crates/git/tests/repack.rs`); `commit_fenced_by_full_repack_gen` (`crates/index/tests/git_store.rs`).
    - [x] `.mtimes`, `.rev` and MIDX files travel through the bucket
      Tests: `mtimes_rev_midx_travel_through_bucket` (`crates/git/tests/repack.rs`); `midx_linked_only_when_covered` (`crates/git/tests/snapshots.rs`).
    - [x] GC deletes pack side files with their pack and superseded MIDX
      generations after the grace period, leaving no orphan
      Tests: `gc_removes_pack_side_files` (`crates/git/tests/repack.rs`).
    - [x] GC deletes only superseded, lost and expired packs past the
      grace period. Superseded bucket objects survive their grace period.
      Tests: `sweep_deletes_only_past_grace`, `sweep_keeps_files_another_row_names` (`crates/git/tests/repack.rs`).
    - [x] snapshot root on the cache filesystem, a sweep at start-up, and
      link-count-aware eviction
      Tests: `snapshot_dir_same_filesystem`, `stale_snapshots_swept`, `eviction_counts_links` (`crates/git/tests/snapshots.rs`).
    - [x] R22 reachability within its budget
      Tests: `reachability_budget_enforced`, `time_budget_stops_a_running_walk`, `concurrent_walks_hold_the_slots`, `over_the_limit_is_retryable` (`crates/git/tests/reachability.rs`).
    - [x] `reindex` rebuilds refs, `ref_log` and the pack list from the
      bucket, and lists unplaced commits as recoverable
      Tests: `reindex_unplaced_persisted`, `ref_fallback_stops_at_recreation`, `reindex_excludes_repacks_and_sweeps` (`crates/git/tests/reindex.rs`).
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
  - `hook_policy_rechecked`: OpenXet refuses, whatever the hook relays,
    an update whose old value isn't the ref's value in the snapshot (a
    stale update), a deletion of `main`, and a non-commit on a branch; a
    forced non-fast-forward update whose old value matches is accepted.
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
    - no `superseded`, `pending`, `lost`, `expired` or `recovered` pack
      is linked, for upload-pack, receive-pack and Hub requests alike
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
  - `writer_cannot_make_unreachable_reachable`. A push or a Hub write is
    refused when it makes an object that sits in a committed pack, but is
    unreachable, reachable by any of these routes:
    - pointing a ref at it
    - naming it as a parent or as a tree entry
    - sending a `REF_DELTA` against it, together with a tree that names
      it, so that `--fix-thin` appends it as a base
    - sending a delta against it that copies its bytes into a new object

    The checks:
    - The refusal text matches git's text for a missing object.
    - The same push succeeds when it sends the object itself.
    - A stock thin push from an up-to-date full clone, whose bases come
      from boundary commits' trees, succeeds through the fast paths.
    - Three real clients from the known limitation (item 3): a bare
      client with bitmaps, a shallow client with two refs pushing both,
      and the stale-client sequence (the client's view of the server's
      refs is older than the snapshot). Each push either succeeds or is
      refused with the thin-pack hint, then succeeds as the hint says.
      None of them makes an unreachable object reachable.
      The test records which path each push took (fast path, fallback
      walk, or refusal) and asserts it against the expected path, so a
      fast-path regression fails the test.
  - `hub_read_by_sha_obeys_r22`: `tree`, `resolve`, `paths-info` and
    `commits` at a commit OID succeed for a reachable non-tip commit, and
    answer "revision not found" for a force-pushed-away commit, a tree
    or blob OID, and a commit beyond the walk budget.
  - `v2_command_allowlist`:
    - the advertisement lists only `agent`, `ls-refs`, `fetch` without
      `filter`, `server-option` and `object-format`
    - `command=object-info`, `command=bundle-uri` and an unknown command
      are refused by OpenXet and never reach git
    - it runs against git 2.37 (where git itself serves `object-info`)
      and against the newest supported git
  - `repack_publish_lease_fenced`: a leaseholder whose lease expired or
    was taken over can't publish, geometric or full, and its pending
    outputs end `lost` or `expired`. A publish whose inputs are no longer
    all `committed` aborts.
  - `reindex_unplaced_persisted`: after deleting the database, reindex
    from a bucket whose ref log lags the last push:
    - the rebuilt refs have complete closures
    - unreached packs are `recovered` and get `created_at` from the
      bucket
    - `commit`, `tag`, `pack` and `edge` entries are written to
      `reindex_unplaced`, survive a restart, and are listed by `verify`.
      An unplaced annotated tag that points at a tree survives a full
      repack.
    - with the clock moved far past the grace period, nothing is
      discarded and GC keeps the recovered packs
    - the metric and the alert report count and age
    - `discard --dry-run` changes nothing, and labels committed-pack
      objects as deleted at the next full repack with no grace period
    - `restore` makes the commit, or a tag given by its OID, fetchable
      and clears its entries.
      `discard`, per repo and with `--all`, clears the rest and moves the
      recovered packs to `lost`.
  - `reindex_hold_never_blocks_repack`: with unresolved entries, full
    repacks still run and publish. Afterwards, every unplaced commit in a
    committed pack, and every `edge` object, is still present with its
    closure, and no recovered pack was an input. A restore after several
    full repacks yields a complete, fetchable closure.
  - `sweep_during_repack_keeps_outputs`: in a repository older than the
    grace period, a sweep between a repack's prepare and its publish
    leaves the outputs `pending` with their files, and the publish
    succeeds.
  - `sweep_claim_races_resent_push`: a push of a pack the sweep is
    deleting, made between the sweep's read and its claim, keeps the
    files. Made after the claim, it is refused retryably, and its retry
    uploads the files again. No committed row is ever left without its
    files.
  - `long_repack_renews_its_lease`: a repack that runs longer than its
    lease's TTL still publishes.
  - `reindex_excludes_repacks_and_sweeps`: reindex with a lease another
    holder has taken over changes nothing. While reindex holds the lease,
    no sweep or repack can get it.
  - `maintenance_finishes_stale_claims`: a maintenance pass finishes a
    dead sweep's claim, so a pack row of that name can be inserted again.
    While another holder has the lease, the pass leaves the claim alone.
  - `ref_fallback_stops_at_recreation`: a ref that was deleted and created
    again, whose new value is gone from the bucket, is left out rather
    than rebuilt at a value from its earlier lifetime.
  - `discard_releases_committed_objects`: a full repack after a discard
    drops the committed-pack objects that only the entries kept.
  - `gc_removes_pack_side_files`: after GC deletes a pack, none of its
    side files remain in the bucket, and a superseded MIDX generation is
    gone once the grace period has passed.
  - `hydrated_pack_mtime_pinned`:
    - hydrated pack files carry the row's `created_at`
    - the leaseholder re-pins mtimes right before each repack, even when
      the cached files were freshened after its snapshot was built
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
  object validation that upstream keeps out of its crates. OpenXet would
  maintain a fork-sized server alone.
- **git for reads, gix for receive-pack (H2).** It saves about 100 ms CPU
  per push. In exchange, OpenXet would own receive-pack's security
  surface (object validation, report-status, push options, atomic
  pushes) that git already provides. Not worth it at v1 push volumes.
- **gix for reads, git for pushes.** Worst of both: it keeps gix's clone
  cost.
- **JGit with DFS as a sidecar.** Closest to the storage model, with
  bucket-backed packs, a block cache and reftable. It loses on "one
  binary" (design §5.1, §9): a JVM runtime with about 77 MB and 1.6 to 2.0 s
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
