# OpenXet: shared working storage for agents

<!-- description: Proposed vision and roadmap for portable agent work: comparison with the approved design, implementation evidence, priorities, release gates and decisions that need approval. -->

Date: 2026-10-25 · Status: proposal for review

This proposal compares the [approved design](2026-10-23-openxet-design.md)
with the owner's request for storage that serves agents using computers and
the internet: selective data access, fast mounts, smaller local workspaces,
and handoffs between agents. It proposes priorities, not available features.
[ADR 0013](../../adr/0013-shared-working-storage-for-agents.md) records the
proposed changes to accepted decisions. Until accepted, the existing design
and milestone commitments remain in force.

The code baseline is `3f5add9206bf8a5d1df3884cd6380c45060d1221` on
`main`, inspected on 2026-10-25. Implementation status below means source
and tests are present at that commit; it is not a fresh runtime or release
certification. M2 is in progress. The README and AGENTS current-state
paragraphs still describe M1 and are insufficient to assess M2 progress.

## 1. Vision and the first users

**OpenXet keeps work usable as it moves between agents, tools and machines.**
Workers attach the inputs they need, experiment in isolation, publish
durable results, and pass precise references to the next worker. Shared
content stays shared. Local disk holds the working set rather than every
workspace's complete history and generated data.

The hypothesis is that cheaper delegation produces more parallel attempts,
retries and intermediate results. The resulting pressure falls on preparing
environments, moving data, retaining state and deciding what can be reused.
This is a workload hypothesis, not a forecast of adoption or storage savings.

The first user is a developer running several coding or research agents on
one machine. They need to retain unfinished work without filling local
storage or slowing active work. The second is a platform operator, such as
aqora or ScienceHub, attaching inputs and preserving outputs for temporary
compute environments. Independent platforms are needed to test whether the
benefit generalizes beyond products the owner operates.

Humans remain owners and reviewers. Existing self-hosters must retain a
small, useful Xet server without adopting workspace management. The hosted
service must use the same open-source core and external identity, preserving
the existing design commitments.

Success means a worker can continue useful work from an exact saved state
on another machine, with less preparation, local disk and data transfer
than a well-configured existing workflow. The unit of value is a completed,
recoverable task, alongside the existing protocol and storage benchmarks.

## 2. What OpenXet should own

The architecture has three layers, with one storage foundation:

| Layer | Responsibility | Relationship to the current design |
|---|---|---|
| Bytes | Verified Xet content, scope membership, reconstruction, durability and retention | Keep the CAS and existing wire contracts |
| Artifacts | A versioned description of content, its format, inputs and outputs | Add a small manifest convention over existing repositories and, later, buckets |
| Workspaces | Assemble selected inputs, isolate writes, capture state and restore it elsewhere | Extend the client; keep active writes on a native local filesystem initially |

An artifact and a workspace snapshot are manifests referencing ordinary
content. They are not a third storage primitive, a new Xet encoding or a
new URI scheme. ADRs 0001, 0006, 0009, 0010 and 0012 remain intact.

Git is a supported interface and the source-code collaboration model.
Agents can use the Hub API without manually organizing Git operations.
Buckets provide the existing route toward non-Git object access. Binary
storage does not provide semantic merging: tools still resolve code,
record and table conflicts using their own rules.

Messaging frameworks carry tasks, status and artifact references. Databases
or queues retain responsibility for task scheduling and transactional
coordination. OpenXet does not become an agent runtime, message broker,
model memory service, compute scheduler or general distributed POSIX disk.

## 3. Comparison with the approved design

| Area | Already on track | Missing or different | Recommendation |
|---|---|---|---|
| CAS and compatibility, §3, §5.2 | Verified chunks, stock clients, scoped reads and immutable content | A content hash alone says nothing about format, meaning or retention | Preserve the wire protocol; add descriptors above it |
| Repositories and buckets, §10.4, ADR 0010 | Generic storage, free labels, both Git and mutable object workflows | Labels do not describe artifact schemas or input/output relationships | Keep both primitives; store manifests as ordinary content |
| Production composition, §5.3, §5.4, §15 | Existing object stores, external identity, stateless nodes, hosted/core parity | These must support the lifecycle promises too | Preserve these commitments and their failure tests |
| Agent interfaces, §7.1 | JSON, structured errors, retries, OpenAPI, MCP and delegated authority | No contract for publishing a complete result, retaining it, or restoring work | Add lifecycle operations and two-worker conformance scenarios |
| Selective reads, §5.5 and S4 | Range gateway and analytics interoperability already specified | No public client seek/range interface or end-to-end selective-read benchmark | Finish the gateway and expose a reusable reader early |
| Mount, §10.8 | Read-only, fixed revisions, lazy fetch on Linux and macOS are planned | No mount workload targets or metadata, cache and prefetch contract | Bring the reader/cache and a measured read-only mount forward from M6 |
| Caching, §10.5, §10.8 | Regional cache and HF-compatible client caching are planned | No machine-wide capacity policy or coordinated use across workers | Add an explicit local working-set budget; preserve HF cache compatibility |
| Workspace lifecycle | Versioned files and local cache are useful foundations | No Git-aware capture, writable isolation, suspension or cross-machine restoration | Add these in a client layer, starting with native local directories |
| Recovery, §5.3, §5.6 | Bucket-first content, pins, explicit GC, recoverable Git objects | Rebuilt branch refs may lag by 60 s; workspace offloading needs an independently recoverable root | Require a synchronous durable snapshot root before local release |
| Mutable publication, §10.4 | Bucket keys are last-writer-wins; Git ref writers are fenced | A shared result name needs conflict detection and idempotent publication | Use existing fenced Git publication first; specify conditional bucket publication before using it for this purpose |
| Measurements, §1a, §5.8 | Protocol latency, ingest, bytes and abuse/load tests | No completed-task latency, allocated workspace disk or restoration benchmark | Add workload traces and baselines before choosing a filesystem architecture |
| Discovery and previews, §2, §10.10 | Full HF product features and content previews are excluded | Agents need bounded format/schema inspection | Propose client-side inspection; leave the current web UI scope unchanged |
| Delivery, §12 | Broad v1 scope, early public source, no waits for external feedback | Useful client workflows and packaging are late in M6/M7 | Propose packaged previews and measured client slices while core work continues |

## 4. What is implemented at the baseline

| Evidence | What it establishes | What it does not establish |
|---|---|---|
| [CAS reconstruction][cas-source], [published benchmarks](../../reference/benchmarks.md) | Range reconstruction and documented protocol measurements | Mounted filesystem speed, production load results, or task speed |
| [Client exports][client-source], [download][download-source], [session options][session-source] | Upload/download by hash and use of the upstream cache | A seekable public reader, machine-wide eviction policy, mount or workspace API |
| [Hub route composition][hub-source], [Git HTTP][git-http-source] | Repository, upload, ref, tree and Git HTTP implementations have landed | Completion of all M2 stock-client paths |
| [M2 plan](../plans/2026-10-25-m2-hub-git-lfs.md), Tasks 13, 14, 18–21 | Gateway, LFS, OpenAPI, compatibility, benchmarks and documentation are required M2 work | That every task is merged or its acceptance checks pass |
| [URI contract](../../reference/xet-uri.md), [parser][uri-source] | Explicit-host syntax, parser, formatter and immutable commit references exist | CLI/SDK adoption, bucket revision support or artifact-specific syntax |
| [ADR 0012](../../adr/0012-repository-identity-and-authorization-rules.md) and [auth rules][auth-source] | Immutable repository ids and an authorizer seam are implemented | OIDC, delegated tokens, per-artifact authorization or a hosted identity service |

At inspection, the Hub route list has no gateway or LFS module. Those
paths remain work to integrate; they must not be inferred from the landing
page or from the existence of the Hub crate. Finish the existing M2 plan
and its compatibility tests rather than abandoning the Git implementation.
Recheck this inventory when planning implementation; main is moving.

## 5. The first complete workflow

Two workers use the same authorized repository on a self-hosted deployment.
Worker A publishes an input dataset and a manifest at a fixed commit. Worker
B receives the reference through its existing task channel, inspects the
manifest and reads selected data through a library or ordinary HTTPS. B
publishes a result and a new manifest linking its exact inputs. A third
process restores and verifies the result after the first two have exited.

The first manifest is a versioned UTF-8 JSON file with:

- a schema identifier and version;
- content references with deployment, authorized scope, immutable version,
  path or file identity, digest algorithm, digest, media type and byte length;
- input references and optional producer/run and transformation metadata;
- optional format schema metadata, whose completeness is explicit;
- retention ownership and an explicit retain-until or retained-until-release
  policy, resolved by the publication service rather than asserted by a file.

The publisher hashes the exact stored bytes. The manifest format needs a
reviewed schema, bounded size/depth and deterministic writer behavior before
implementation; field spelling is left to that schema review. No new
`xet://` grammar is implied. Use the existing address contract and HTTPS.

The v0 manifest and referenced content live in one repository at a complete
Git commit, reachable through a retained ref. Repository authorization is
the boundary. A recipient must already have explicit read access; a digest
or manifest grants none. Sharing a restricted subset requires materializing
it into a separately authorized resource. A local filter is not an access
control. Cross-deployment transfers use separate credentials and verify the
copied content; the first demonstration does not promise global resolution.

Publish content first, then the complete manifest, then expose the result
through a fenced ref update with an expected predecessor. Retries carry a
stable publication identity; a repeated request returns the original result
or a conflict, never a second unrelated publication. Readers see the old
complete result or the new complete result. The v0 design must specify how
this retry identity is persisted and recovered with the repository.

P1 inherits M2's recovery contract: content and commits are durable, but a
named ref may roll back by up to 60 s after total index loss, and an unplaced
commit can require operator recovery before it is served. Return the exact
commit and manifest reference and document that limitation. P1 never uses
publication success as permission to discard a producer's local work. P3's
synchronous retained root adds automatic restoration after index loss.

Agent provenance is attributed metadata. Reusing a computation requires
deterministic inputs, code, parameters and relevant environment state;
neither a prompt hash nor an artifact hash proves the result is correct.

## 6. Fast reads and mounts

Implement the seekable reader and cache before binding them to a filesystem.
Use the approved client core; keep its dependencies out of the server.
The read-only mount fixes its revision for its lifetime. Following a moving
branch requires an explicit refresh or remount, never mixed file versions.

Required behavior:

- Lazy, paginated metadata access. Attaching a repository does not traverse
  every file or hydrate all content.
- Concurrent range fetching, bounded in-flight bytes and cancellation.
- Adaptive read-ahead for sequential access and bounded speculative reads
  for sparse access. Measure amplification, not only throughput.
- Reuse verified content across revisions and workers where authorized.
  Access decisions remain credential-scoped and fail closed, including
  cache hits; a shared physical cache does not imply shared authority.
- An explicit local cache capacity, accounting, pins for selected hot data,
  coordinated eviction and recovery after an interrupted cache write.
- Documented offline behavior: selected cached content can be available
  under the adopted credential policy; an uncached read fails clearly.
  Revocation cannot recall plaintext already delivered to a local process.

Start with a library and local materialization on macOS and Linux. Prototype
a Linux FUSE read-only adapter, and evaluate macOS FSKit/macFUSE separately
for installation, permissions and tool compatibility. Do not make macOS users
wait for a writable FUSE implementation to benefit from snapshot/restore.
Environments without mount privileges keep the reader/HTTPS path. Windows
keeps the existing client support commitment; mount support is advertised
only after its own tested implementation.

Parquet selection belongs to DuckDB, Polars or another format-aware reader.
Random CSV scans, compressed streams and broad filters can still require
most content. A mount makes file access available; it does not invent
indexes or turn small result size into small scan size. A server-side query
service, if justified later, runs outside the storage request path and
reports scan, compute and result costs separately.

## 7. Workspaces and local disk

The first workspace implementation uses ordinary writable directories,
native copy-on-write cloning where available, existing package stores and
build caches. A shared mutable `target` or `node_modules` directory is not
safe isolation. Reuse immutable inputs and correctly keyed build results;
keep per-worker mutable state separate. Start on macOS and Linux.

Capture and restore must understand Git worktrees. A copied `.git` file
pointing at another machine is not a portable snapshot. Capture the required
Git objects, including unpushed commits, the selected HEAD/ref state, index,
staged and unstaged edits, and selected untracked/ignored files. Recreate
worktree administrative paths on restoration. Preserve supported modes and
symlinks without following them outside the capture root; list unsupported
metadata and refuse an advertised exact restoration if it would be lost.

The first capture requires quiescent writers. The caller or runtime stops
them and keeps the workspace quiescent through local release; OpenXet
reports a busy workspace rather than killing processes or
claiming a consistent live snapshot. A filesystem snapshot adapter may
relax this later only with a documented consistency contract. Process RAM,
open connections and running VM/container state are outside this feature.

Capture policy distinguishes durable work, reconstructible dependencies and
disposable scratch. Credentials and external files are not automatically
captured. Exclusions are shown before capture. An excluded non-reconstructible
file blocks local release, even if Git ignores it. Restoring a manifest
never runs install hooks or code automatically.

Local release is a separate, retryable operation after capture:

1. Capture immutable content and the complete root manifest.
2. Verify uploads and synchronously persist a retained snapshot root in
   the object store, with ownership and restoration metadata.
3. Verify restoration from that root and return a durable receipt.
4. Recheck that the local workspace has not changed and that all data
   selected for release is covered. Abort on any difference.
5. Release the covered local files; retain the root until explicit release
   or the agreed expiry, with visible policy and dry-run output.

Steps 2–3 require the durability/retention slice of M4. Existing asynchronous
Git ref logging, with up to 60 s rollback after index loss, is insufficient
as the only restoration address. The root and its pin must survive reindex
without manual recovery of an unplaced commit. Deletion from the server
still follows GC and its existing grace. Evicting a verified local cache
entry is distinct from deleting retained remote data.

Automatic eviction of active writable workspaces is excluded initially.
A general writable virtual filesystem is considered only if the native
directory/cache approach fails the measured workload for a specific reason.

## 8. Roadmap in dependency order

These are proposed delivery slices, not replacement names for M0–M8.
No calendar promises are made. Core compatibility and recovery work continue
while workload evidence and bounded client prototypes are gathered.

| Slice | Deliverable and user outcome | Dependency and relationship to current milestones | Exit evidence |
|---|---|---|---|
| P0: Establish the workload | Reproducible traces for agent workspaces, selective analytics and artifact handoffs; choose the first fixture from actual disk attribution | Start now alongside M2; extend §1a and the M3 load harness | Published method, sanitized fixture, actual allocation/transfer/task-time baselines; no claimed savings from directory size alone |
| P1: Publish, inspect, hand off | Complete M2, a minimal artifact manifest and client workflow; two workers exchange immutable inputs/results | Existing CAS, Hub commits, gateway, URI parser and auth rules; bring selected M6 SDK/URI adoption and a minimal M7 preview package forward | Stock-client M2 checks; conflicting/retried publication tests; fixed-version handoff on two machines; ordinary files still export/read with stock tools |
| P2: Read quickly with bounded disk | Seekable client reader, coordinated local cache and a read-only mount prototype; repeat work reuses cached content | P1, gateway conformance and cache policy; bring part of M6 forward, keep regional M5 caching as a separate deployment feature | Cold/warm selective-read and mount results, bounded cache under concurrent readers, no mixed revisions, authorization on cache hits |
| P3: Preserve and resume a workspace | Git-aware capture, isolated native directories, verified restoration and explicit local release | P1/P2 plus durable roots, pin/release, reindex, verify and export from the M4/M5 safety work; macOS and Linux adapters | Crash tests across capture/release; complete edits and Git state restored on a second host; root recovery after database loss; measured disk benefit |
| P4: Operate across platforms | A maintained SDK/integration contract used by independent runtimes; managed service on the same released core | Production M3 backends/auth/quotas, M4 audit/metering/recovery, required M5 export, M6 delegation/UI controls, M8 operational readiness | Two different runtimes complete a handoff; real repeated use; failure/load/restore drills and documented export; hosted/core parity |

P1's manifest/handoff is smaller and tests more of the thesis than a new
writable filesystem. P0 determines which local duplication matters. If
dependencies are already effectively shared, prioritize inactive-workspace
offloading and durable handoffs over another dependency store. If generated
content has little byte reuse, sell portability and recovery only where
they earn their cost; do not promise dedup savings.

Production work runs alongside the client slices. A private test on local
disk is not evidence of production readiness. No public multi-tenant launch
or local release against a production service skips persistence, isolation,
bounded resource use, audit requirements or restore drills. Prioritize the
S3-compatible backend and chosen database deployment for the first remote
pilot; keep GCS/Azure and the rest of M3 on the approved roadmap.

Continue the broader roadmap as explicit backlog: remaining backends,
SSH, mirror, multiple pools, regional cache, full web self-service and
distribution channels retain their compatibility and safety requirements.
For this vision they are not all prerequisites to a private P1/P2 preview.
An S3-compatible *frontend* remains a separate pending S12 decision; using
an S3-compatible *backend* does not decide it.

## 9. Benchmarks and decision gates

Use one representative TypeScript workspace and one Rust workspace, at
1, 5 and 20 concurrent workers. Add a dataset with selective Parquet reads,
sequential scans and small random reads. Capture file creation, metadata
operations and unique writes as well as large transfers. Raw private paths,
contents and credentials stay local; publish sanitized fixtures and
aggregate results with operator opt-in, consistent with no default telemetry.

Compare with configured Git worktrees, pnpm's shared store, native filesystem
cloning where supported, existing compiler/build caches, archive/restore
for suspended workspaces, and ordinary S3/HTTPS + Parquet + DuckDB access.
Use Mountpoint for applicable Linux read workloads. Compare equivalent
retained state and cache budgets, not an optimized candidate against an
unnecessarily duplicated baseline.

Measure:

- allocated bytes on a controlled filesystem, including cache, clones,
  metadata and temporary peak usage; do not sum `du` across hardlinks or
  assume APFS clones have independent physical extents;
- time to an attached directory, first useful read, first passing task/test,
  snapshot completion and verified restoration;
- cold/warm p50 and p95, throughput, metadata operations, bytes requested
  and actually fetched, cache hit rates, CPU/RAM and origin request counts;
- stored unique bytes, retained unreachable bytes, GC backlog, egress and
  total cost over the same lifecycle trace, including the 28-day minimum
  GC grace rather than only live logical bytes;
- acknowledged-data loss and restoration mismatches under interruption,
  disk-full, token expiry, concurrent publication and index-loss scenarios.

Run repeated trials, record hardware, filesystem, versions, network and
cache conditions, and publish variance. Instrument deterministic scripted
workloads first; use real agents to check task usability separately so
model variability does not masquerade as storage performance.

Initial decision thresholds, proposed here and not measured results:

| Gate | Threshold and consequence |
|---|---|
| Correctness | Zero lost acknowledged content, lost released workspace state, cross-scope reads or silent restoration mismatches in the specified fault suite; verify P1's documented ref-recovery limitation and P3's stronger root-recovery contract separately |
| Local workspace value | Target at least 50% less peak allocated disk on the 20-worker lifecycle trace than the best applicable baseline, with warm task p95 within 10%; if unmet, do not expand a custom workspace filesystem without a newly identified benefit |
| Selective-read value | On a fixture whose layout allows pruning, fetch metadata plus required column/row-group data without whole-file hydration; demonstrate improvement or parity in task time and disclose amplification |
| Cache capacity | Accounted cache stays within its configured bound plus a documented capped in-flight allowance; unuploaded work never becomes an eviction candidate |
| Portable handoff | Restore selected inputs, outputs and supported workspace state on a clean second machine; receive the same digests and sufficient metadata to continue, with no dependency on the producer's home directory |
| External relevance | Seek two independent runtime/platform integrations and repeated workflows; collect feedback alongside implementation rather than making development wait for interviews |

Set absolute mount latency and throughput targets after P0 fixes dataset
size, file count and network conditions. Do not advertise instant attachment,
local-SSD speed or a storage ratio without the corresponding measurement.

## 10. Changes to accepted direction

The recommendation is to keep the protocol foundation and change the order
in which useful client capabilities reach users. It changes product emphasis
from upload efficiency alone to continuity and reuse of work.

| Existing decision | Proposed change | Approval consequence |
|---|---|---|
| §1 intent and §1a value model emphasize a self-hostable Xet server and first upload | Keep that core promise; add portable work and completed-task evidence | Update the product design after acceptance; keep self-hosting simple |
| §7.1 agent parity is mainly interface behavior | Add artifact publication, retention and handoff conformance | Extend the agent leg without replacing current CLI/schema checks |
| §10.8 client delivery is concentrated in M6 | Deliver reader/cache/manifest slices earlier; put workspace capture in the client | Change dependencies and write implementation plans per slice |
| §5.3 allows up to 60 s Git-ref recovery lag | Keep that documented contract; require synchronous retained roots for local release | Specify durable snapshot/pin records and reindex semantics in a further ADR before P3 |
| §5.6 minimum GC grace is 28 days | Preserve it | Any shorter retention optimization needs a separate compatibility decision |
| §10.4 bucket keys are last-writer-wins | Keep stock bucket behavior; add a separately specified conditional publication operation only if needed | Do not silently change Hub bucket semantics or use last-writer-wins as a coordinator |
| §2/§10.10 exclude content previews | Add bounded client format inspection; preserve web UI exclusions | Specify supported formats and resource limits, without adding a dataset browser |
| §12 builds M0–M7 through 1.0 without feedback pauses | Add packaged workflow previews before M7 is complete; keep core development moving and review the final 1.0 cut after evidence | Explicit owner approval is needed to revise the existing all-features 1.0 commitment; this proposal does not silently remove features |

No new public positioning is adopted here. The existing Git-based entry
point remains useful. Once a portable-work workflow is demonstrated, its
measured evidence can support an additional product story through a separate
update to `design.md`. Hosted pricing in S13 remains unapproved; no agent
count, revenue forecast or price is inferred from this vision.

## 11. Prior art and the reason to choose OpenXet

The following primary sources were reviewed on 2026-10-25. They establish
documented capabilities, not comparative performance or demand:

| Prior art | What it already covers | What OpenXet must demonstrate |
|---|---|---|
| [HF Storage Buckets](https://huggingface.co/docs/hub/storage-buckets) | Xet-backed mutable storage next to Git repositories | Operator choice and workflows that remain useful across independent platforms |
| [AWS Mountpoint](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mountpoint-usage.html) | Filesystem access and local/shared caches | A measurable advantage on the selected workload, including all reconstruction overhead |
| [Tigris](https://www.tigrisdata.com/features/) | Bucket forks, snapshots and object events | More than a feature-name match: portable workspace restoration and compatibility in practice |
| [lakeFS](https://docs.lakefs.io/use-cases/isolated-environments/) | Versioned, isolated data branches | A useful experience for the selected developer/platform workflow with transparent costs |
| [A2A](https://a2a-protocol.org/v1.0.0/specification/) | Task communication and output artifacts | A small integration for durable references, not another agent communication protocol |
| [Bazel remote caching](https://bazel.build/remote/caching) | Reuse of reproducible action outputs through a content-addressed store | Compatibility with established build-cache tools before inventing a generic execution cache |

The proposed advantage is an open, embeddable implementation that combines
Xet compatibility, controlled local footprint and portable work. Neither
deduplication, a mount nor an MCP endpoint alone establishes that advantage.
Adoption depends on maintained integrations and repeated useful workflows.

Start distribution with an installable client and one complete local
workflow. Package the same operations as a small SDK for notebook and agent
platforms, then maintain integrations where independent operators actually
use them. A platform choosing OpenXet as its default artifact layer can
bring more recurring work than agents choosing a new tool one task at a
time. This is a distribution hypothesis to test, not an adoption forecast.
Track returning organizations, recurring workflows, maintained production
integrations and cost per completed task; temporary agent counts and
logical stored bytes alone do not establish product value. Existing privacy
and opt-in measurement rules still apply.

## 12. First implementation packages after approval

1. **Workload evidence:** a trace/fixture format and measurement harness for
   the three P0 workloads, with physical-allocation methodology and best
   applicable baselines. This is the first new code to build.
2. **Finish M2:** integrate outstanding gateway/LFS/OpenAPI work and run the
   planned stock-client, Git, auth and recovery checks. Continue concurrently
   with workload measurement; do not restart the Git engine.
3. **Artifact handoff:** review the minimal manifest/publication contract,
   then implement one publish/inspect/read/result workflow over the existing
   repository. Deliver a CLI/library recipe and a reproducible two-worker
   example before a broad MCP catalog or new server query engine.
4. **Shared reader/cache:** implement the range reader and bounded cache,
   measure it directly, then attach the read-only filesystem adapter.
5. **Durable capture:** specify roots, pins, release and Git-aware restore;
   complete their recovery/export work before reclaiming user workspaces.

Detailed HTTP schemas, module layout, cache algorithms, mount library and
platform adapters belong to the reviewed implementation plans. The builder
can select them within the boundaries above, with measurements for choices
that affect task latency, allocated space or operations cost.

[cas-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/cas/src/reconstruction.rs
[client-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/client/src/lib.rs
[download-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/client/src/download.rs
[session-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/client/src/session.rs
[hub-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/hub/src/lib.rs
[git-http-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/hub/src/githttp.rs
[uri-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/uri/src/lib.rs
[auth-source]: https://github.com/stubbi/openxet/blob/3f5add9206bf8a5d1df3884cd6380c45060d1221/crates/auth/src/rules.rs
