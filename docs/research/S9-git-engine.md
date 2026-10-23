# S9: Git hosting engine

- Date: 2026-10-23
- Question (design §10.9, §11): which engine serves Git smart HTTP and SSH
  (upload-pack with protocol v2, receive-pack) for openxet-hosted repos,
  with packs stored in the bucket, refs in the database under
  compare-and-swap, and stateless nodes that may keep a local pack cache?
- Candidates:
  1. gitoxide (`gix` 0.90.0) in-process
  2. the `git` binary on a node-local cached bare repo
  3. libgit2 (`git2` 0.21)
  4. JGit with DFS storage
  5. go-git
  6. Gitaly
  7. Mononoke (Sapling)
  8. hybrids
  9. other reference designs: Gitea, Soft Serve
- Versions measured:
  - `gix` 0.90.0 with `gix-pack` 0.77.0, `gix-odb` 0.87.0 and
    `gix-protocol` 0.68.0. `gix-transport` 0.62.0 was read but not used.
  - git 2.39.5 (Apple Git-154), for both server and client
  - go-git `v6.0.0-beta.1` (2026-10-04)
  - JGit 7.8.0.202609011348-r (`org.eclipse.jgit.pgm`, sha1 verified) on
    OpenJDK 26.0.1
  - MinIO RELEASE.2026-09-22T19-25-18Z
    (`cgr.dev/chainguard/minio@sha256:d88920d6…`)
- Hardware: Apple M3 (8 cores), 16 GiB RAM, macOS 15 (Darwin 24.6.0, arm64).
  MinIO ran in Docker in a dedicated colima VM (2 vCPU, 2 GiB), reached
  through colima's port forward.
  - **The host was shared and heavily loaded:** the load average was 24–31
    on 8 cores for the whole run, from other workloads.
  - Wall-clock numbers are therefore noisy, by more than 3x between
    identical runs.
  - Per-request CPU time and peak RSS were far more stable, typically
    within 15–25%, with the outliers shown as ranges. They are the basis
    of the comparison. Every table states which numbers are which.
- Labels: **observed** means measured or run here. **Source** means read in
  code or docs, with a link or path. **Inferred** means reasoned from the
  two, not tested.
- The prototype code is throwaway and is not in the repo. It is summarised
  in "Method", which is enough to rebuild it.

## Answer

**Use the `git` binary for the wire protocol. openxet owns everything
around it: refs, packs in the bucket, the node cache, and pack-list
consistency.** gitoxide stays in-process for object reads and writes that
are not wire protocol. This is the hybrid "H1+" below. ADR 0004 records the
decision.

- **gix has no server side today (source).**
  - It ships no upload-pack, receive-pack, server transport or push.
  - Its pack generator does no delta compression and has no reachability
    bitmaps.
  - The maintainer closed three server-side PRs in 2025–2026 and asked
    that server work live in forks (see "gitoxide").
  - A gix server would be openxet's to write and maintain: negotiation,
    shallow, filter, sideband, report-status, push options, atomic pushes,
    connectivity and object checks, and repack.
- **Reads decide it (observed).** Serving a full clone of the 10k-commit
  repo cost git 0.18 s CPU and 89 MB peak RSS. The prototype in-process gix
  server cost 2.86 s CPU and 178 MB, and sent 6% more bytes. The gap is
  16x in CPU and 2x in memory. Server wall time differed by about 13x,
  which is indicative only, given the host load.
  - An incremental fetch of 100 commits cost git 45 ms CPU and 33 MB.
    gix cost 284 ms and 216 MB.
  - Most of git's lead comes from reachability bitmaps: without them git
    needed 1.2–1.6 s. git also reuses deltas verbatim, and gix-pack 0.77
    can only copy deltas that already exist in the source pack.
- **Pushes are cheaper in-process, but the gap is cheap to close
  (observed).**
  - A one-commit push cost gix 13 ms CPU and git 120 ms. The git cost is
    process spawns: `receive-pack`, `index-pack`, the connectivity
    `rev-list`, the hook and `merge-base`.
  - Neither number includes object validation:
    - `receive.fsckObjects` was off in the git engine. The production
      config turns it on, so the ~100 ms rises with the number of pushed
      objects.
    - gix has no fsck of object contents, and its connectivity check was
      written for this spike.
  - Pushing the whole 218k-object history cost about the same in both: git
    6.3 s CPU, gix 7.4 s.
- **The hybrid gets the cheap parts of gix (observed).** In protocol v2 the
  capability advertisement does not depend on refs, and `ls-refs` can be
  answered straight from the database. The H1+ prototype does both
  in-process and spawns `git upload-pack` only for `command=fetch`. Fetch
  and clone CPU stay git's, and a no-op fetch drops to gix's cost.
- **Concurrent pushes are correct with either engine (observed).** In 60
  rounds of 8 simultaneous pushes to one branch, spread over two stateless
  nodes (git+git, gix+gix and git+gix sharing one database and bucket):
  - every round had exactly one winner
  - the database tip was always the winner
  - every loser got a clean `non-fast-forward` or `fetch first`
  - a fresh node hydrated from the bucket and its clone passed
    `git fsck --strict`
  - CAS lives in the database, not in the engine, so the engine choice
    does not affect it.
  - The database was SQLite (`BEGIN IMMEDIATE`). The integration test
    required by ADR 0004 also runs on PostgreSQL's conditional-update
    CAS, which S9 did not test.
- **Bucket-backed packs keep nodes stateless (observed, with one invariant
  inferred).**
  - A push publishes its pack and index to the bucket and registers it in
    the database before the ref CAS. A cold node downloads the registered
    packs: 80 MB took 0.4–0.6 s.
  - Each request runs on a throwaway snapshot repo. The prototype used
    `packed-refs` from the database and `objects/info/alternates` to the
    node cache. The post-review design links exactly the request's
    committed pack set instead.
  - The invariant: **read the ref snapshot before syncing the pack list**,
    both from the primary or one consistent snapshot. The prototype first
    did it the other way round, which leaves a race window.
- **Safety rules found in review (source and inferred, see "Safety
  rules").**
  - git's v2 `fetch` serves any object in the odb, alternates included.
    openxet therefore enforces want reachability itself (R22).
  - Pack rows need states, so a repack can't drop a push that sits between
    publish and CAS. Only committed packs are exposed. Repack keeps
    ref-log-recent objects through keep refs, because cruft expiry goes by
    mtime.
  - The hook route and git's environment must be locked down.
- **Licences.** git is GPL-2.0-only (source).
  - openxet runs it as a separate program. That is aggregation, so openxet
    stays Apache-2.0 (inferred, not legal advice).
  - Container images that include git must meet GPL-2.0 for that binary:
    each release publishes the exact git source package or includes a
    written offer (§3).
  JGit (EDL-1.0), go-git (Apache-2.0) and gix (MIT/Apache-2.0) are
  permissive. libgit2 is GPL-2.0 with a linking exception. Mononoke is
  GPL-2.0-or-later.

## Method

All code is throwaway, kept in the spike's scratch directory and not in
the repository. One Rust binary (`s9srv`, axum 0.8, tokio) serves smart HTTP
with one of three engines, behind the same storage layer:

- **Storage, shared by every engine.**
  - Refs live in SQLite (WAL), as `refs(repo, name, oid)`. A push is one
    `BEGIN IMMEDIATE` transaction that checks every command's old OID
    against the row and applies all of them or none.
  - Packs live in MinIO (S3 API, `object_store` 0.12) at
    `repos/<repo>/packs/pack-<hash>.{pack,idx,bitmap}`. They are registered
    in `packs(seq, repo, name)` after the upload and before the ref CAS.
  - Each accepted push appends a ref-log object to
    `repos/<repo>/reflog/<ts>-<node>` (design §5.3).
  - Each node keeps a cache, `<cache>/<repo>.git/objects/pack`. Before
    serving, it downloads every pack registered after the last `seq` it has
    seen. Two node processes with separate caches share one database and
    bucket.
- **`git` engine (candidate 2).**
  - Every request builds a snapshot repo in a temp dir:
    - `packed-refs` written from the database
    - `objects/info/alternates` pointing at the node cache
    - config `receive.procReceiveRefs=refs/`, `receive.unpackLimit=1` and
      `core.alternateRefsCommand=true`
  - It then runs `git upload-pack` or `git receive-pack --stateless-rpc`
    with `GIT_PROTOCOL` from the request header, streaming stdin and
    stdout.
  - Ref updates go through a `proc-receive` hook (githooks(5), git ≥ 2.29,
    source: `Documentation/RelNotes/2.29.0.adoc`). receive-pack has already
    indexed the pack, checked connectivity and migrated the objects out of
    quarantine.
  - The hook does a fast-forward check (`git merge-base --is-ancestor`).
    It then hands the pack path and commands to the parent server over a
    local HTTP call. The server publishes the pack, runs the CAS, writes
    the ref log and answers `ok` or `ng <reason>`.
    - A first version had the hook publish to S3 itself. Building a fresh
      S3 client per push cost 130–210 ms (observed), so the delegation
      stayed.
- **`gix` engine (candidate 1).** Everything is in-process:
  - protocol v2 capability advertisement, `ls-refs` from the database, and
    `fetch` with ACK/`ready` negotiation
  - pack generation with `gix_pack::data::output`:
    - `count::objects` with `TreeContents` for clones, and
      `TreeAdditionsComparedToAncestor` for fetches with haves
    - `entry::iter_from_counts` with `PackCopyAndBaseObjects`
    - `bytes::FromEntriesIter` into side-band 1
  - receive-pack (protocol v0, `report-status`, `atomic`): the pack is
    indexed with `gix_pack::Bundle::write_to_directory`, which resolves thin
    packs against the cache odb
  - a connectivity check, written for this spike: walk the new commits
    with the existing tips hidden, then expand their tree diffs and check
    that every object exists
  - a fast-forward check with `Repository::merge_base`
  - The template was gitoxide's own `gix pack create`
    (`gitoxide-core` 0.64.0, `src/pack/create.rs`).
  - Shallow, `filter` and `include-tag` were not implemented, and nothing
    in the tests needed them.
- **`hybrid` engine (H1+).** It is the `git` engine, except for two parts:
  - the v2 capability advertisement, captured once from
    `git upload-pack --advertise-refs` at start-up
  - `command=ls-refs`, answered in-process from the database
- **Metrics.** Each request logs its wall time and its CPU delta. The CPU
  delta is `getrusage(RUSAGE_SELF)` plus `RUSAGE_CHILDREN`, so spawned git
  processes and hooks count. It also logs the peak RSS of the server
  process and of its largest child. The server restarts before each
  measured clone and 100-commit fetch, so those peaks are per operation;
  the on-disk cache stays warm.
- **Clients:** git 2.39.5 with an empty `HOME` and
  `GIT_CONFIG_NOSYSTEM=1`. The first runs had the global git-lfs
  `pre-push` hook, which added about 0.8 s per push by calling a
  non-existent LFS endpoint.
- **Test repos:**
  - `tf10k`: `huggingface/transformers` `main`, cloned with `--depth 10000`
    and re-rooted with `git fast-export | git fast-import`. That gives
    10,001 commits and 218,574 objects. `git repack -adfb` produced one
    72.7 MB pack with a bitmap.
  - `lfs`: synthetic, 500 commits and 200 `*.safetensors` LFS pointer
    files, with 3 pointers changed per commit. It has 3,698 objects in a
    0.56 MiB pack.
  - Both were seeded into the bucket and database through the storage
    layer.
- **Benchmarks:**
  - **End to end** (`bench.py`, 3 runs, medians, `git clone --bare`):
    - cold clone, with the node cache wiped
    - warm clone
    - fetch of the last 100 commits
    - no-op fetch
    - one-commit push (5 runs, server kept warm)
    - push of the whole history to an empty repo
  - **Server only** (`microbench.py`, 5 runs after a warm-up): the same
    protocol v2 `fetch` request is replayed with curl and the pack is
    discarded. This takes client-side `index-pack` out of the measurement.
  - **Three-way stdio** (`stdio-bench.py`, 3 runs): git, go-git and JGit
    each run as the `upload-pack` program of `git clone --no-local -u`
    against the same single-pack, bitmapped copy of `tf10k`. A wrapper
    reports `RUSAGE_CHILDREN`, so git's `pack-objects` child counts.
  - **Concurrency** (`conc.py`): 2 nodes, 8 clients and 20 rounds per
    engine pairing.

## gitoxide (`gix` 0.90.0): server-side capabilities today

Read in the released crate sources and in `crate-status.md` at
GitoxideLabs/gitoxide `main` `4dc73c3` (2026-10-23):

| Capability | Status | Evidence |
|---|---|---|
| Serve upload-pack v2 (`ls-refs`, `fetch`) | **No** | `gix-protocol` 0.68.0 is client-only. Its features are `blocking-client`, `async-client`, `fetch` and `handshake`, and `src/` has no server module. `crate-status.md`: "[ ] upload-pack / receive-pack server plumbing for in-process transports". |
| Serve receive-pack | **No** | Same line. Push is unchecked even on the client: "[ ] push", "[ ] send-pack / receive-pack client plumbing", "[ ] report-status, sideband, delete-refs, push-options and atomic pushes". `gix` 0.90 `src/push.rs` only holds the `push.default` config enum. |
| Server transport | **No** | `gix-transport` 0.62.0 `src/` has only `client/`. `crate-status.md`: "**server** [ ] general purpose `accept(…)` for servers". |
| Pack generation for clones | **Partial** | `gix-pack` 0.77.0 `data::output` (feature `generate`): object counting, entries and a pack writer. `iter_from_counts::Mode` has one variant, `PackCopyAndBaseObjects`: it copies deltas that already exist in a pack and writes everything else whole. `crate-status.md`: "[ ] delta compression", "[x] create 'thin' pack". |
| Reachability bitmaps | **No** | No `bitmap` reference in `gix-pack` 0.77.0 `src/`. `crate-status.md`: "[ ] 'bitmap' file". |
| Receive and index pushed packs | **Yes** | `gix_pack::Bundle::write_to_directory` (feature `streaming-input`) indexes a pack from a stream and resolves thin packs against an odb. The prototype indexed every push with it (observed). |
| Connectivity and object checks | **Partial** | `crate-status.md` `gix-fsck`: connectivity from commits only, and "[ ] validate object hashes during connectivity traversal", "[ ] all individual checks available in `git fsck`". There is nothing like `receive.fsckObjects`. |
| Hooks, quarantine | **No** | `crate-status.md` `gix-hook`: all unchecked, including "quarantine-aware hook execution". |
| Multi-pack index | **Yes** | `crate-status.md`: MIDX "[x] read", "[x] write", "[x] verify". |
| Negotiation primitives | Client only | `gix-negotiate` 0.38 implements the client's `noop`, `consecutive` and `skipping` algorithms. |

**Upstream direction (source).** Three PRs proposed a server-side
upload-pack and were closed unmerged:
- [#2104](https://github.com/GitoxideLabs/gitoxide/pull/2104),
  `gix-upload-pack`, 2025-08
- [#2465](https://github.com/GitoxideLabs/gitoxide/pull/2465), 2026-03
- [#2887](https://github.com/GitoxideLabs/gitoxide/pull/2887), v2
  upload-pack, 2026-08

The maintainer's decision, on #2887
([comment](https://github.com/GitoxideLabs/gitoxide/pull/2887#issuecomment-5207697033),
2026-08-06): "Kindly ask to keep them in a fork, while offering to take
fixes and supporting modifications into `gix-` crates if the amount of code
remains reviewable." Asked when a fork might merge back, he answered that
he is busy "to the end of this year" with Git 3 compatibility and a `gix`
1.0 RC.

**Maintenance (source, GitHub API, commits since 2025-10-23):**
- Byron: 1,241 commits
- an automated `codex` author: 721
- EliahKagan: 93
- cruessler: 92

The project is active, but its bus factor is effectively one human
maintainer.

## Numbers

### Server only: serving `tf10k` (observed, 5 runs, medians)

These numbers come from replaying a protocol v2 `fetch` request with curl
and discarding the pack.

| Request | Engine | Server wall ms (median / min) | Server CPU ms | Peak RSS MB | Bytes sent |
|---|---|---|---|---|---|
| Full clone | git (bitmap) | 498 / 373 | **178** | **89** | 72.75 MB |
| Full clone | gix | 6,529 / 5,296 | 2,859 | 178 | 76.93 MB |
| Full clone | git, `pack.useBitmaps=false` (4 runs) | 7,003–14,027 | 1,200–1,620 | 216 | 72.75 MB |
| Fetch 100 commits (thin) | git | 127 / 96 | **45** | **33** | 1.72 MB |
| Fetch 100 commits (thin) | gix | 688 / 538 | 284 | 216 | 1.76 MB |

gix phase timings for a full clone (observed, from the server log):
- commit walk: 0.65–1.35 s
- object counting: 4.1–5.9 s
- entry generation and write: 3.5–16.4 s, depending on load

### Server only: git, go-git and JGit over stdio on `tf10k` (observed, 3 runs, medians)

Each engine runs as the `upload-pack` program of a `git clone --no-local`
or `git fetch` over `file://`, with protocol v2 negotiated in every case.
They all serve the same single-pack, bitmapped copy of `tf10k`. CPU and
RSS cover the whole server process tree. Wall time includes the client's
`index-pack` and host contention.

| Request | Engine | Server CPU s | Peak RSS MB | Server wall s | Bytes in client pack |
|---|---|---|---|---|---|
| Full clone | git 2.39.5 | **0.17** (0.13–0.19) | **76** | 7.1 (6.2–8.8) | 72,735,829 |
| Full clone | JGit 7.8 (includes 1.6–2.0 s JVM start-up) | 2.73 (2.29–5.56) | 180 | 4.6 (4.0–5.1) | 72,735,829 (identical: pack reuse) |
| Full clone | go-git v6.0.0-beta.1 | 116.7 (108–140) | 654 | 209 (135–382) | 71,403,722 (recomputed deltas) |
| Fetch 100 commits | git | **0.03** | **32** | 0.16 | n/a (see below) |
| Fetch 100 commits | JGit (includes JVM start-up) | 2.18 | 177 | 1.2 | n/a |
| Fetch 100 commits | go-git | 7.42 | 232 | 9.9 | n/a |

- Every client passed `git fsck`.
- Fetch sizes aren't comparable here: each run's client base pack is
  rewritten by its own `gc`. The HTTP server-only table above has git's
  fetch size.
- **JGit:** subtracting its measured JVM start-up (1.6–2.0 s CPU, about
  77 MB) leaves roughly 0.7–1.1 s for a full clone and 0.2–0.5 s for a
  fetch in a warm sidecar (inferred). That is 4–6x git's CPU and twice its
  memory, but in the same class.
- **go-git** recomputes every delta and has no bitmaps. It is 700x git's
  CPU for a clone and not viable as a server for repos of this size.

### End to end over HTTP through the prototype (observed)

`tf10k` (3 runs, medians; push 1 commit is 5 warm runs). The client wall
time includes client-side `index-pack` and the host contention described
above. The CPU and RSS columns are the comparison.

| Operation | git: client ms | git: server CPU ms | git: peak RSS MB (server / child) | gix: client ms | gix: server CPU ms | gix: peak RSS MB |
|---|---|---|---|---|---|---|
| Clone, cold node (hydrates 80 MB first) | 3,687 | 271 | 176 / 85 | 60,250 | 3,368 | 180 |
| Clone, warm node | 6,415 (2,742–8,783) | 185 | 23 / 85 | 32,848 (21,532–73,826) | 3,264 | 175 |
| Fetch 100 commits | 661 | 83 | 17 / 32 | 7,249 (799–8,830) | 333 | 219 |
| No-op fetch | 168 | 38 | 16 / 5 | 83 | 0.9 | 16 |
| Push 1 commit | 1,000 (277–1,588) | 120 | 17 / 26 | 460 | 13 | 17 |
| Push whole history (218,574 objects) to empty repo | 43,129 | 6,301 | 231 / 129 | 62,155 | 7,388 | 239 |

The H1+ hybrid on `tf10k` (git for `fetch` and receive-pack; capabilities
and `ls-refs` in-process; 3 runs, medians, same conditions):

| Operation | client ms | server CPU ms | peak RSS MB (server / child) |
|---|---|---|---|
| Clone, cold node (hydrates 80 MB in 13 packs: 0.6–1.3 s) | 8,485 | 307 | 173 / 79 |
| Clone, warm node | 6,436 | 169 | 20 / 79 |
| Fetch 100 commits | 1,554 (604–1,960) | 47 | 16 / 33 |
| No-op fetch | 136 | **1.1** (git engine: 38) | 15 / 4 |
| Push 1 commit | 226 | 102 | 17 / 27 |
| Push whole history to empty repo | 17,337 | 5,804 | 235 / 187 |

`lfs`, the LFS-pointer repo (3 runs, medians):

| Operation | git: client ms | git: server CPU ms | gix: client ms | gix: server CPU ms |
|---|---|---|---|---|
| Clone, cold node | 264 | 77 | 347 | 79 |
| Clone, warm node | 100 | 46 | 122 | 35 |
| Fetch 100 commits | 100 | 50 | 75 | 14 |
| No-op fetch | 70 | 28 | 41 | 0.5 |
| Push 1 commit | 185 | 79 | 96 | 9 |
| Push whole history to empty repo | 238 | 114 | 120 | 53 |

The H1+ hybrid on `lfs` (3 runs, medians):

| Operation | client ms | server CPU ms |
|---|---|---|
| Clone, cold node | 437 | 78 |
| Clone, warm node | 95 | 29 |
| Fetch 100 commits | 127 | 32 |
| No-op fetch | 90 | **0.7** (git engine: 28) |
| Push 1 commit | 193 | 90 |
| Push whole history to empty repo | 257 | 100 |

Correctness checks in every benchmark (observed):
- every fetched client passed `git fsck --strict`
- every whole-history push was cloned back through the same engine,
  passed `git fsck --strict` and had the same tip
- a forced non-fast-forward push was rejected with
  `! [remote rejected] main -> main (non-fast-forward)` by both engines

### Concurrent pushes to one branch (observed)

Each round: 8 clients sit at the same tip and each commits a different
change. All 8 push at once: even clients to node A, odd clients to node B.
The nodes are two server processes with separate caches, sharing the
database and bucket. The repo is the LFS-pointer history.

| Nodes | Rounds | Exactly one winner | DB tip = winner | Loser messages | Fresh-node clone `fsck --strict` | Commits on `main` |
|---|---|---|---|---|---|---|
| git + git | 20 | 20 | 20 | 108 `non-fast-forward` (server CAS), 32 `fetch first` (client saw the new tip) | clean | 520 / 520 |
| gix + gix | 20 | 20 | 20 | 140 `non-fast-forward` | clean | 520 / 520 |
| git + gix | 20 | 20 | 20 | 140 `non-fast-forward` | clean | 520 / 520 |

- The loser sees
  `! [remote rejected] main -> main (non-fast-forward: refs/heads/main moved to <oid> concurrently; fetch and retry)`.
  This is a `proc-receive` `ng` reason on git, and a `report-status` `ng`
  on gix.
- Losers' packs are already in the bucket when the CAS fails:
  - 108 orphan packs in the git+git run
  - 140 in each of the other two runs
- That follows from publish-before-CAS, which §5.3 requires. Garbage
  collection must collect them (see below).

### Node hydration and repacking (observed)

| Operation | Result |
|---|---|
| Cold node, `tf10k`: download 80.2 MB (1 pack + idx + bitmap) | 385–573 ms. 3.9–7.9 s while the gix benchmark loaded the host and MinIO's 2-vCPU VM. |
| Cold node, `lfs`: 0.63 MB in 15–21 packs | 86–644 ms |
| Full `git repack -adb` of 19 packs (`tf10k` after the benchmark pushes) | 1.13 s wall, 1.10 s CPU, 281 MB peak RSS (all child processes counted) |
| `git repack --geometric=2 -d --write-midx --write-bitmap-index`, same input | 0.69 s wall, 0.62 s CPU, 243 MB peak RSS. The 72.7 MB base pack is untouched. 18 small packs merge into one 5 KB pack, plus a 7.0 MB `multi-pack-index` and a 1.3 MB MIDX bitmap; those three files are what goes back to the bucket. |
| Full clone served through `objects/info/alternates` with the bitmap in the alternate | 178 ms CPU, the same as a local bitmap. Without bitmaps it took 1.2–1.6 s, so the alternate's bitmap is used. |

## Bucket-backed packs and stateless nodes

What the prototype showed (observed). Where the post-review design
departs from the prototype it is marked (inferred), and ADR 0004 is
binding.

1. **How pushed packs land in the bucket.**
   - receive-pack (or gix) indexes the incoming pack into the request's
     quarantine. `unpackLimit=1` means a pushed pack is never exploded
     into loose objects.
   - openxet inserts the pack row as `pending`, then uploads `.pack` and
     `.idx` to the bucket.
   - The ref CAS and the update
     `packs.state = 'committed' WHERE state = 'pending'` commit in one
     transaction, together with:
     - a `full_repack_gen` check against the value read in the push's
       snapshot (the repack fence, item 3)
     - the insert of the database `ref_log` rows
   - The client gets `ok` only after that commit.
   - A failed transaction, whether on the CAS or on the fence, rolls back
     and then sets the row to `lost` (`WHERE state = 'pending'`). A
     pending pack already expired by GC can't be committed.
   - Hub ref writes and Hub commits use the same fenced transaction, on
     the same kind of snapshot (ADR 0004 item 6).
   - The bucket ref log (§5.3) is derived from the `ref_log` rows, for
     every ref writer, Hub API included (inferred; the prototype had no
     states and wrote the bucket ref log directly).
   - A crash before the CAS leaves a `pending` pack and no ref change. GC
     expires it after the grace period. A crash after the CAS loses
     nothing, because the pack is already durable (§5.3).
2. **How a cold node hydrates.**
   - **Observed (prototype).** The node read the ref snapshot, then
     downloaded every registered pack it lacked. For `tf10k` that is
     80 MB in under 0.6 s on an unloaded path.
   - **Post-review (inferred).** New snapshots expose only `committed`
     packs. `pending`, `lost`, `expired` and `superseded` packs never
     reach a new snapshot, receive-pack's included. In-flight requests
     keep the hard links they took.
     - Otherwise a push could pass the connectivity check using objects
       that exist only in another in-flight or lost pack, or only in a
       superseded input whose expired objects a full repack has dropped.
       GC of that pack would then leave a committed ref dangling.
     - Evicting `lost` packs is defensive only, since they are never
       hydrated.
   - Refs and the pack set are read in one database snapshot, from the
     primary or a single consistent snapshot, never from replicas with
     different lag (§5.8).
   - Hydrated files are `.pack`, `.idx`, `.rev`, `.bitmap` and
     `.mtimes`, plus the generation's `multi-pack-index` with its
     `.bitmap` and `.rev`. Pack mtimes are pinned to the row's
     `created_at`, so cruft expiry measures age from creation, not
     download.
   - Git can't serve from a partial pack, so hydration is per pack, not
     per object (inferred). Large fleets want repo-affinity routing
     (consistent hashing) to keep caches warm. Any node can still serve
     any repo, just slower when cold.
   - **Snapshot exposure (inferred).** Each request snapshot's
     `objects/pack` holds hard links to exactly its committed pack set,
     with no alternates to the whole cache dir.
     - The prototype used alternates. That served the same bitmap at the
       same cost (observed), but exposed every cached pack.
     - A push's own new pack is visible only to its own receive-pack,
       through quarantine.
     - Hard links need the snapshot root on the cache's filesystem
       (`EXDEV` otherwise). Start-up checks this and fails fast, and also
       sweeps leaked snapshot directories.
     - Eviction counts space as freed only when the link count shows no
       snapshot holds a file.
     - A superseded pack's stale `.bitmap` could slow serving, but new
       snapshots never include superseded packs, so only requests
       already in flight see it.
3. **Repacking strategy** (measured above, design inferred).
   - The cost is geometric:
     - every push adds one small pack
     - a background job (one leaseholder per repo, via a database lease)
       runs `git repack --geometric=2 -d --write-midx --write-bitmap-index`
       on a copy built from `committed` packs only
     - geometric repack leaves cruft packs alone (source: git v2.39.5
       `builtin/repack.c` line 355)
     - it uploads the new pack, MIDX and bitmap, and commits a new pack
       list in the database in one transaction
   - Inputs become `superseded`, with a timestamp. They leave new
     snapshots at once, but their bucket objects are kept through the
     grace period (§5.6, default 30 days), and GC deletes them after it.
   - Geometric repacks roll packs up with `pack-objects --stdin-packs`,
     which keeps every object of every input pack (source: git v2.39.5
     `builtin/repack.c` line 937, and the `git-pack-objects(1)`
     description of `--stdin-packs`). They drop nothing and need no
     fence.
   - A periodic full `repack -adb --cruft --cruft-expiration=<grace>`
     (git ≥ 2.37) folds the history into one pack.
   - **Cruft expiry is by object mtime, not by when an object became
     unreachable** (source: git's cruft packs record per-object mtimes
     in `.mtimes`).
     - So an old commit that was just force-pushed away would be dropped
       at the next full repack.
     - The leaseholder therefore writes temporary
       `refs/openxet/keep/<n>` for every old and new OID in the database
       ref log within the grace window. Recently unreachable objects stay
       reachable for the repack.
     - Example: a pending push on branch B that builds on a commit just
       force-pushed away from branch A stays safe.
     - `pack.recentObjectsHook` would do the same, but needs git 2.42
       (source: RelNotes 2.42.0), so keep refs keep the floor at 2.37.
   - Each cruft pack's `.mtimes` is a pack artifact, uploaded and
     hydrated with the pack.
   - **mtimes on the leaseholder.** Right before each repack, the
     leaseholder re-pins input mtimes from the database `created_at`.
     - Hard links share inodes, and git may freshen pack mtimes while
       serving, so the leaseholder's view would otherwise drift.
     - Re-pinning costs one `utime` per pack. The alternatives are
       copying multi-GB packs, or running git under a uid that does not
       own the cache, which complicates deployment.
   - **Repack fence, in both directions** (found in review, inferred).
     A full repack can drop a long-unreachable object that an in-flight
     push builds on.
     - A full-repack publish increments `repos.full_repack_gen`. A push
       whose snapshot read the old generation fails its CAS, retryably.
     - The full-repack publish aborts and retries if `ref_log` advanced
       past the high-water mark recorded with its working copy.
     - The keep refs come from the database `ref_log`. Its rows are
       written in the CAS transaction and retained for at least the
       grace period plus the maximum pending age, so they are durable
       together with the ref they describe.
   - **Why only committed packs.** A plain `repack -adb` over all packs
     could drop a pending push's own pack contents, and the CAS would
     then succeed onto missing objects (inferred).
   - **Orphan reclaim.**
     - Lost and expired-pending packs are deleted whole by GC after the
       grace period. Their objects were never exposed.
     - Unreachable objects inside committed packs leave by cruft expiry,
       once they are outside the keep-ref window and older than the grace
       period.
   - The bucket stays the truth: `reindex` rebuilds refs, `ref_log` and
     the pack list by listing `repos/<id>/packs/`. A pack is `committed`
     if a rebuilt ref reaches its objects. Otherwise it is `recovered`:
     never exposed and never collected. Unplaced commits and recovered
     packs are persisted in a `reindex_unplaced` table. Full repacks
     still run, keeping those commits and the objects that recovered
     packs build on as keep roots. Nothing is discarded until an
     operator restores or discards it (ruling R24; §5.3, ADR 0004
     item 4).
4. **Two nodes accepting pushes at once.**
   - Both index and publish their pack. Only the database CAS chooses a
     winner (60 of 60 rounds above). The engine never decides.
   - With protocol v2 fetch and `fetch first`, the client usually notices
     on its own.
   - The cost is the losers' uploaded packs. They are bounded by push
     volume and reclaimed by the repack and GC above. A cheaper
     alternative: check the old OID against the database before
     uploading, and still CAS after. That narrows the window but cannot
     close it (inferred).
5. **What runs on a node.**
   - Per-request snapshot repos in a temp dir. Creating one is a handful of
     small files.
   - The pack cache, which can be evicted freely.
   - Nothing else.
   - The `proc-receive` hook talked to the server over loopback HTTP in
     the prototype. In production it is hardened as in "Safety rules".

## Safety rules

These came out of the review rounds after the spike. They are binding
through ADR 0004.

1. **Upload-pack serves any object in the odb (source).**
   - git v2.39.5 `upload-pack.c`, `parse_want` (lines 1413–1442), only
     checks that the wanted object parses. Objects from alternates
     count.
   - The v0/v1 `allowAnySHA1InWant` and `allowReachableSHA1InWant`
     checks are not applied in v2.
   - So any client that knows an OID could fetch:
     - force-pushed-away content
     - losers' orphan packs
     - with shared alternates, another repo's objects

   **Ruling R22, fully specified in ADR 0004 item 2.**
   - **Scope.** openxet parses the wants of every upload-pack request
     before forwarding them to git. openxet interposes on the request
     stream, so this covers v2 `fetch` (including several commands in
     one SSH session) and the v0/v1 want lines over HTTP and SSH.
     - Checking v0/v1 too removes the cost asymmetry of git spawning
       `rev-list` for non-advertised v0/v1 wants.
   - **Walk.** One walk per request, starting from the same ref snapshot
     written to `packed-refs`.
     - Ref tips and peeled tag targets pass without walking.
     - All other wants share one commit-graph-assisted gix walk.
   - **Commit and tag wants** are checked by the walk. **Tree and blob
     wants** pass only when a ref or peeled tag points at them directly;
     with no tree traversal, every other one is refused.
   - **Budget, per request and with config defaults:**
     - `git.reachability.max_commits` 200,000
     - `git.reachability.max_ms` 2,000
     - `git.reachability.max_concurrent_per_repo` 2
     - `git.reachability.max_concurrent_per_principal` 4
     - Over the concurrency limits, the request gets a retryable error
       (HTTP 429).
   - **Fail closed.** Anything not found within the budget is refused.
     - This can refuse a legitimate deep `fetch <sha>` or submodule pin
       in a huge repo.
     - The mitigation is bitmap-backed reachability, with the bitmap
       built from the current ref tips only. The repack's bitmaps include
       keep refs and must never be used for it. It is a follow-up issue
       labelled `git-reachability`.
   - Only non-tip wants pay for the walk, and stock clients send them
     rarely.
   - **Pinned config:**
     - `uploadpack.allowAnySHA1InWant=false`
     - `uploadpack.allowReachableSHA1InWant=true`
     - `uploadpack.allowFilter=false` until partial clone is designed
     - `core.alternateRefsCommand=true`, as defence in depth
   - **No alternates.** Snapshots link exactly their pack set, so there is
     no cross-repo object sharing (fork pools) without a layer that
     enforces reachability.
   - **v2 command allowlist (source: git `serve.c`, tags v2.37.0 to
     v2.51.0).**
     - git 2.37–2.44 advertises and serves `object-info` unconditionally
       (`always_advertise`). It returns the size of any OID in the odb,
       with no reachability check.
     - From 2.45, `object-info` is gated by `transfer.advertiseObjectInfo`
       (default false). `bundle-uri` (2.40+) is gated by
       `uploadpack.advertiseBundleURIs` (default false).
     - So openxet's interposer accepts only `ls-refs` and `fetch`, and
       refuses every other command itself. The config keys are pinned
       off as well, for defence in depth.
   - **Writers and the Hub API.** R22 also binds writers: a push or Hub
     ref write may only make reachable what its snapshot's refs already
     reach or what the client itself sent.
     - receive-pack always runs `index-pack --fix-thin` (source: git
       v2.39.5 `builtin/receive-pack.c` line 2263), which appends missing
       delta bases from the odb (`builtin/index-pack.c` line 1430).
     - openxet counts the client's pack bytes and header, so appended
       bases are not counted as sent. Each appended base must also lie
       in a boundary commit's tree, which is where stock git takes thin
       bases from (`builtin/pack-objects.c`, `show_edge` →
       `add_preferred_base`). Otherwise a sent delta could copy the
       content of an unreachable base. Hub reads by commit OID pass the same
     check. Hub reads and writes run on a request snapshot, never on the
     node cache (ADR 0004 items 3 and 6).
   - **Known residuals,** for the `git-reachability` follow-up:
     - git ACKs a `have` for any object in the snapshot, and treats
       `shallow` lines similarly. That confirms existence without
       sending content. A blob OID is a content hash, so a guessed
       low-entropy content can be confirmed this way.
     - a refused push may reveal, by stage or timing, whether a named
       object exists. This is existence only.
2. **Pack states** (inferred). See "Bucket-backed packs", items 1–3.
   - `pending`, then `committed` in the CAS transaction, and only while
     the row is still `pending`
   - `lost`, on a failed CAS
   - `expired`, a stale pending pack marked by GC
   - `superseded`, a repack input, which leaves new snapshots at once
   - `recovered`, a pack that `reindex` found but no rebuilt ref reaches.
     Only an operator restore (to `committed`) or discard (to `lost`)
     ends it. It never blocks repacks.
   - Any failed commit transaction, the fence included, ends in `lost`.
     Repack publishes also check the lease epoch.
   - Only committed packs reach new snapshots.
   - Repack runs over committed packs, with keep refs from the database
     `ref_log` grace window and the `full_repack_gen` fence in both
     directions. Pack mtimes are pinned to `created_at`, and `.mtimes`
     files are carried through the bucket.
3. **Hook route and git process** (inferred hardening).
   - **Hook channel:** an inherited `socketpair` fd with no path, over
     HTTP and SSH alike.
     - openxet clears `FD_CLOEXEC` on that one fd for the git child,
       which passes it on to the hook.
     - The hook finds it through `OPENXET_HOOK_FD`.
   - **Token:** single-use, delivered as `OPENXET_HOOK_TOKEN`, bound to
     repo ID, principal and request, and expiring at request end.
   - openxet takes the repo and authorization from the token's context,
     never from hook-supplied data.
   - It accepts a pack path only if the path resolves inside that
     request's snapshot or quarantine directory.
   - It re-checks fast-forward and branch policy itself.
   - **git's environment** starts empty and contains only:
     - `PATH`, a fixed list: the git exec dir and openxet's hook dir
     - a private `HOME`
     - `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL=/dev/null`
     - a validated `GIT_PROTOCOL` (`version=0|1|2`)
     - `OPENXET_HOOK_FD` and `OPENXET_HOOK_TOKEN`, for receive-pack only
   - Hooks run from openxet's directory only (`core.hooksPath`).
   - Snapshot and cache paths derive from internal repo IDs, never from
     user-supplied names.

## Candidate matrix

Columns per the task brief. Each cell says how it is known.

| | Server-side capabilities | Clone, fetch, push performance | CPU / RSS | CAS correctness | Fit with bucket packs and stateless nodes | Licence and distribution | Fit with a Rust single binary | Maturity, maintenance, bus factor |
|---|---|---|---|---|---|---|---|---|
| **1. gix 0.90 in-process** | No upload-pack, receive-pack, server transport or push. Pack indexing yes; pack generation without delta compression or bitmaps (source, table above). | Full clone 16x and fetch 6x git's server CPU; server wall ratios were similar but are indicative only. Push faster (observed). | 16x CPU and 2x RSS on clone; 9x less CPU on small pushes (observed). | Correct: 60/60 rounds (observed), because CAS is in the database. | Good: in-process odb over the node cache. Repack would need delta compression we would write (source, inferred). | MIT/Apache-2.0: none. | Best: pure Rust, in-process. | Active, about 1 human maintainer. Server work is explicitly kept out of upstream (source). |
| **2. git binary** (`upload-pack` / `receive-pack --stateless-rpc` on a snapshot repo) | Everything: v0/v1/v2, shallow, partial clone, bitmaps, MIDX, `fsckObjects`, `proc-receive`, quarantine (source: git docs). | Fastest reads; push about 0.2 s server wall (observed). | Lowest on reads: 178 ms / 89 MB for a full clone (observed). About 120 ms CPU per small push, all spawns (observed). | Correct: 60/60 rounds through `proc-receive` (observed). | Good: the prototype used alternates to the node cache, with packs published by openxet (observed). Production links committed pack sets (ADR 0004). | GPL-2.0-only, run as a separate program. Images ship git unmodified from a distro package with its source available (source, inferred). | A runtime dependency: openxet spawns `git`. Needs git ≥ 2.37: 2.29 for `proc-receive`, 2.34 for MIDX bitmaps and 2.37 for cruft packs (source: RelNotes). | The reference implementation, with a broad maintainer base. Gitea, Soft Serve and Gitaly all spawn it (source, rows 6 and 9). |
| **3. libgit2 / `git2` 0.21** | No server transports. Every `upload-pack` hit in libgit2 is a client transport (`src/libgit2/transports/*`). Has `git_packbuilder` (delta), `git_indexer`, custom `odb_backend.h` and `refdb_backend.h` (source, libgit2 1.9.7). | Not measured: a server would have to be written on top, as with gix. | Not measured. | Would depend on our code. | Custom odb/refdb backends map onto a bucket and DB, but pack-level object access through a custom odb is slow (inferred). | GPL-2.0 with linking exception: static linking allowed (source: `COPYING`). | C dependency through `git2`; no protocol layer. | Mature; effectively one core maintainer (ethomson 276 commits in the last year, next contributor 26; source). |
| **4. JGit with DFS** | Complete server: `UploadPack` (v2), `ReceivePack`, bitmaps, reftable. `internal.storage.dfs` (`DfsObjDatabase`, `DfsReftableDatabase`, `DfsGarbageCollector`, `DfsPackCompactor`, `DfsMidxWriter`) is built for packs on a distributed store (source, JGit 7.8). Google's hosted Gerrit is widely reported to run on it (inferred; not re-verified here). | See the stdio table. JVM start-up alone is 1.6–2.0 s CPU and about 77 MB (observed). | As above. | `DfsReftableDatabase` gives atomic batch ref updates; we would plug our DB in (source, inferred). | Best design fit: DFS **is** bucket-backed packs with a block cache and stateless servers. The package is `internal`, with no API stability promise (source). | EDL-1.0 (BSD-3-style): none. | A JVM sidecar: second runtime, process, memory floor and deployable. Breaks "one binary" (design §5.1). | Mature (Eclipse, Gerrit, Google); a small core (msohn 289, ifradeo 125, davido 47 commits in a year, source). |
| **5. go-git v6** | `v6.0.0-beta.1` (2026-10-04) has server-side `UploadPack` (v0/v1/v2) and `ReceivePack` and a smart-HTTP `backend`. TODOs on `main` in `plumbing/transport/upload_pack.go`: thin-pack and shallow-file unsupported. No bitmaps (no `bitmap` file in the tree). Storage via the `storage.Storer` interface (source). | See the stdio table. | As above. | Would depend on our code. | `Storer` could be backed by bucket and DB, but per-object access through it is slow (inferred). | Apache-2.0: none. | A Go sidecar. | v5 stable; v6 is beta. pjbgf 558, hiddeco 237 commits in a year (source). |
| **6. Gitaly** | A gRPC service around the git binary (`git cat-file --batch`, a pack-objects cache wrapping `git pack-objects`), source: GitLab docs. | Not measured: it is git underneath. | git's, plus a Go service. | Its own: Praefect with PostgreSQL. | Poor: repos live on local disks (`storage` path), not object storage. HA through Praefect replication (source: README, docs). | MIT. | A Go service plus Praefect and PostgreSQL. | Mature, GitLab staff. **Use as a reference architecture:** git binary, a pack-objects cache, and service-owned replication. |
| **7. Mononoke (Sapling)** | `eden/mononoke/servers/git/git_server`: protocol v2 `fetch`, `ls_refs`, `push` in Rust, built on `gix-pack` 0.74, `gix-object` and `gix-hash`, with S3 and SQLite backends (source). | Not measured. | Not measured. | Its own bookmarks service. | Built for blobstore-backed storage, but the OSS build "omits some functions" and has no configuration docs (source: README). | **GPL-2.0-or-later**: can't be linked into Apache-2.0 openxet; a sidecar would be its own GPL deployable. | A huge Rust monorepo: Buck/autocargo, Thrift omitted in OSS. | Meta-maintained, but not for external use. **Use as a reference:** it proves a gix-primitives server over object storage is possible at scale, with a large team. |
| **8a. H1+: git for protocol, openxet for storage, gix for non-protocol reads (chosen)** | git's, all of them (observed). | git's reads and pushes; no-op and `ls-refs` at in-process cost (see the hybrid rows). | git's on the fetch path; about 1 ms for `ls-refs`. | 60/60 (observed, the `git` engine's path). | Good (observed). | As 2. | One Rust binary plus a git runtime dependency. | As 2, plus our glue. |
| **8b. H2: git upload-pack for reads, gix receive-pack** | Reads git's; push needs our receive-pack: report-status, side-band, push options, atomic, fsck (source, inferred). | Reads as git. Push measured as gix: 13 ms vs 120 ms CPU (observed). | Saves about 100 ms CPU per push. | 60/60 (observed, the gix path). | Good. | As 2. | As 8a. | We would own receive-pack security (object validation) alone. |
| **8c. gix serving reads, git receive-pack** | Reads as candidate 1. | Worst of both: gix's 16x clone CPU (observed). | | | | | | Rejected by the numbers. |
| **9. Gitea, Forgejo, Soft Serve** | Spawn `git upload-pack` / `receive-pack --stateless-rpc` (Gitea `routers/web/repo/githttp.go`; Soft Serve `pkg/git/service.go` uses `exec.CommandContext(ctx, "git")`), source. | n/a | n/a | Local refs only. | Local disks. | Gitea and Soft Serve: MIT. Forgejo: copyleft since v9.0; files move to GPL-3.0-or-later over time, and most code is still MIT (source: Forgejo v9.0 release post). | Go. | **Use as a reference:** the common pattern for self-hosted forges is candidate 2. |

## Licences and distribution

- **git (GPL-2.0-only).**
  - openxet runs `git` as a separate program over pipes and does not
    link to it. That is aggregation, and openxet stays Apache-2.0
    (inferred from GPL-2.0 §2's "mere aggregation" clause; not legal
    advice).
  - Shipping git in openxet's container image is distribution of git.
    The image must carry git's licence.
  - Each image release publishes the exact git source package next to
    the image, or includes a written offer for it (GPL-2.0 §3).
  - The simplest compliant route is the distro's unmodified package
    (Debian, Wolfi or Alpine), whose source the distro publishes. The
    image's licence notice lists it.
  - A static `openxet` binary for self-hosters stays git-free and
    requires `git` on `PATH`, checked at start-up with an actionable
    error.
- **gix:** MIT/Apache-2.0. **go-git:** Apache-2.0. **JGit:** EDL-1.0.
- **libgit2:** GPL-2.0 with linking exception.
- **Mononoke:** GPL-2.0-or-later. Its code can't be copied into openxet.

## Not verified

- **SSH:** no benchmark. With git, SSH runs the same snapshot and
  `git upload-pack` / `receive-pack` (without `--stateless-rpc`) on the
  channel (inferred). The SSH server itself (for example `russh`) is
  separate work for §10.1.
- **Linux numbers:** all measurements are on macOS under heavy host load.
  A Linux run on an idle machine is expected to lower every wall time,
  not to change the CPU ratios (inferred). Process spawn is cheaper on
  Linux, which favours the git engine's push path.
- **`receive.fsckObjects`** was not enabled in the git engine. It adds CPU
  proportional to pushed objects, and the production config should
  enable it.
- **Shallow, partial clone and `include-tag`** were not exercised.
- **Very large repos** (millions of objects) and many repos per node.
- **MIDX bitmaps in the production layout.** The test served a
  single-pack bitmap through `objects/info/alternates` (observed). Two
  things were not tested: a multi-pack-index bitmap, and the post-review
  layout, where hard-linked pack sets replace alternates.
- **PostgreSQL CAS.** The concurrency evidence comes from SQLite only.
- **Not prototyped.** All of the following were specified after the spike
  and are untested:
  - reachability enforcement (R22)
  - pack states
  - per-snapshot pack sets
  - keep refs and pinned mtimes, and carrying `.mtimes` through the
    bucket
  - the database `ref_log` and the `full_repack_gen` fence
  - Hub writes on request snapshots, the writer reachability rule, and
    Hub reads by OID under R22
  - the v2 command allowlist
  - lease epochs at repack publish
  - the `recovered` state, the `reindex_unplaced` table and its keep
    roots
  - excluding `--fix-thin` bases from the sent objects

  ADR 0004 requires all of this to be prototyped in M2, with
  property-based and fault-injection tests, before production use.
- The JGit and go-git rows use their stdio upload-pack, not a DFS or
  bucket-backed `Storer`. They compare protocol and pack-generation cost
  on identical input, not their storage layers.

## Reproduce

All five steps run against the throwaway prototype in the spike's scratch
directory. It is not in the repository; "Method" describes it well enough
to rebuild.

1. Start MinIO on a dedicated colima profile. Create a bucket. Put its
   endpoint and keys in the environment as `S9_S3_ENDPOINT`, `S9_BUCKET`
   and `AWS_*`.
2. Build `tf10k` and `lfs` as in "Method". Seed each with
   `s9srv seed --db db.sqlite --repo <name> --src <bare repo>`.
3. Run `bench.py <engine> <repo>`, then
   `microbench.py tf10k <tip> -`, then `microbench.py tf10k <tip> <tip~100>`.
4. Run `stdio-bench.py <label> <upload-pack command>` for git, go-git and
   JGit.
5. Run `conc.py <engineA> <engineB> 20 8` for each pair.
