# S15: Feasibility of shared working storage for agents

<!-- description: Feature-by-feature feasibility audit, verified upstream APIs, local probe results, platform limits and prototype gates for the proposed agent-storage roadmap. -->

- Date: 2026-10-25
- Scope: the proposed agent storage vision of
  [PR #103](https://github.com/stubbi/openxet/pull/103) and the storage,
  identity and recovery capabilities it depends on. That vision is
  superseded by the [agent workspaces
  design](../superpowers/specs/2026-10-27-agent-workspaces-design.md)
  ([ADR 0013](../adr/0013-agent-workspaces-come-next.md)); this audit's
  findings and gates still apply to it.
- Baseline: `3f5add9206bf8a5d1df3884cd6380c45060d1221`, the main
  commit used in the vision. This audit does not certify the whole approved
  design or rerun every protocol conformance test.

## Verdict

The bounded product has concrete implementation paths: immutable artifact
handoffs, selective reads, a coordinated local cache, platform-specific
read-only mounts, isolated native workspaces and explicit capture/restore.
None needs a new storage encoding or a breakthrough in model capabilities.
Several need substantial integration and failure testing.

The broadest interpretations are not supportable promises: a universal
writable remote disk, transparent migration of running processes, safe
automatic eviction of arbitrary directories, instant revocation of already
delivered data, hard egress caps enforced by presigned URLs alone, or
local-SSD performance for uncached remote reads. The roadmap narrows those
claims and blocks the dependent features on specific prototypes.

This is an engineering feasibility assessment. Performance, savings,
operating margins, adoption and a production-ready implementation remain
separate things to demonstrate.

## Evidence levels

- **Verified mechanism:** inspected source or a local probe demonstrates
  the stated primitive. The assembled OpenXet feature is not implied.
- **Established path:** documented APIs or existing implementations provide
  the mechanism; OpenXet integration and failure behavior remain untested.
- **Prototype gate:** a plausible route exists, but a platform constraint,
  system-level interaction or required guarantee needs a focused proof.
- **Excluded promise:** outside the bounded proposal or stronger than the
  available mechanism. Do not publish it as a feature target without a
  different design and evidence.

Complexity below is relative integration effort, not a calendar estimate.
Small means a narrow interface or recipe; medium spans several existing
components; large adds lifecycle or operating-system behavior.

## Feature register

| Feature | Assessment / complexity | Concrete route | Boundary or required proof |
|---|---|---|---|
| Immutable artifact identity and descriptors | Verified mechanism / small to medium | Ordinary versioned JSON plus existing content hashes and fixed Git commits | Schema validation, reference limits and authorization remain required; a hash is not permission |
| Agent-to-agent artifact handoff | Established path / medium | SDK/CLI operations; existing A2A or task transport carries the reference | Demonstrate two different consumers; content still transfers when absent locally; OpenXet is not the message broker |
| Complete publication and retries | Verified mechanism plus prototype gate / medium | Existing expected-parent/fenced Git publication; add a stable persisted publication identity | Current fencing does not by itself implement client-request idempotency after a lost response; test concurrent publication and retry recovery |
| Non-Git artifact storage | Established path / medium | Planned bucket primitive with immutable content and manifests; retain existing repo path first | Bucket names are mutable and last-writer-wins. Conditional publication and retained roots need explicit new operations/records, not invented revision syntax |
| Public byte-range reader | Verified mechanism / medium | Pinned `xet-data` public range-to-writer API, with a seek adapter above it | The range API does not take its session file semaphore; add request/memory admission, cancellation, EOF and overlap handling |
| Parquet slicing before full download | Verified local mechanism / small to medium | Format-aware reader over HTTP Range or the seek adapter | Projection and layout/statistics govern pruning. The local probe is not an OpenXet range/mount benchmark |
| Bounded format/schema inspection | Established path / medium | Read relevant metadata with existing parsers; return bounded structured output | Not every format has a cheap header/footer. Unsupported or costly inspection must report a limit or require explicit processing |
| Nearby query/transform worker | Established path / large | Separate bounded execution service using an established engine and a scoped storage credential | Result size is not scan size; arbitrary code needs separate isolation. Not an initial storage-server feature |
| Coordinated machine-wide cache | Prototype gate / large | One owner/coordinator with byte reservations, immutable entries and reader leases; preserve upstream cache format where used | No hard bound over independent HF processes or writable workspaces outside its control; account for both caches, open files and partial downloads |
| Cache sharing with access control | Established path plus prototype gate / large | Authenticate each operation at a host broker, isolate cache storage from untrusted workers, retain scope checks | Agents sharing an OS user and readable plaintext are not isolated by a token. Start within one trusted user/security domain; separate tenants through OS/container boundaries |
| Linux read-only mount | Established path / large | FUSE callbacks over the reader, fixed revision, paginated metadata and bounded readahead | Requires host FUSE support and an allowed mount path/device. Not guaranteed inside a restricted container or hosted sandbox |
| macOS read-only mount | Prototype gate / large | Native FSKit app extension or an explicitly supported macFUSE backend | Framework presence is not API parity. Generic URL resources start at macOS 26; test signed packaging, exact OS/backend, read-only behavior and application compatibility |
| Windows mounts | Prototype gate / large | A separate adapter using an established API such as [WinFsp](https://winfsp.dev/doc/WinFsp-API-winfsp.h/) | Driver distribution, licensing and application compatibility need review. No Windows mount is implemented or tested here; ordinary client/materialization support is the fallback |
| Cheap writable workspace forks | Verified local primitive / medium to large | Native file cloning where supported; otherwise materialization, existing package stores and build caches | Same-volume/filesystem support is conditional; copies can consume space on first write. Do not hardlink mutable workspace files or promise a universal clone ratio |
| Capture ordinary Git worktree state | Verified local mechanism / large | Required Git objects plus index/working-tree state and selected extra files; rebuild administrative paths on restore | Probe covers a common subset. Conflicted indexes, in-progress Git operations, submodules, sparse modes, LFS-only local data and special metadata need support or explicit rejection |
| Resume on another machine | Established path / large | Restore selected files and Git state using a manifest, without producer-local paths | First contract is a compatible OS/architecture and supported filesystem metadata. Native dependencies may need rebuilding; file restoration is not process or environment migration |
| Reclaim an inactive local workspace | Prototype gate / large | Retained durable root, verified recovery, explicit operation and writer exclusion through completion | Initially only runtime-managed workspaces with guaranteed writer exclusion. A scan or final mtime check cannot exclude arbitrary external writers |
| Durable roots, pins, release and GC | Established path plus prototype gate / large | Extend the existing object-store record/index/GC pattern with immutable snapshot roots and retention records | Current key types and GC do not automatically discover JSON references. Add typed records and marking/reindex rules; crash-test pin/GC/publication races before eviction |
| Production backends and external identity | Established path / large | Existing storage/auth seams, SQL transactions and provider SDKs; M3/M4 work | Test each backend's actual conditional-write/durability behavior, delegation chain, quotas and restore path. No user database or new object store is required |
| Strict per-agent egress cap | Prototype gate / large | Metered transfer path with atomic byte reservations and bounded concurrent streams | Direct presigned URLs can be replayed until expiry. Charging a grant is not measuring actual egress; strict caps need a mediated path and a decision about the approved reads-never-stop policy |
| Revocation and offline reads | Established bounded path / medium | Scope checks and credential/URL expiry; an explicit offline-cache policy | No recall of plaintext already delivered. Do not promise immediate cancellation of every issued direct URL by revoking only an OpenXet token |
| Reuse completed computation | Established bounded path / medium to large | Integrate established action/build caches for reproducible computations with complete keys | A repeated prompt or matching dataset alone does not make an agent run deterministic or an answer correct |
| Two-way sync, portable links and exports | Established path / medium to large | Existing URI contract, manifests and explicit sync/conflict rules; restore-based export checks | Preserve conflict detection, deletion policy and supported metadata. HTTPS remains portable; a `xet://` parser is not native support in every third-party tool |
| Regional caching, residency and hosted operations | Established path / large | Existing transfer/storage roles, explicit placement, tenant isolation, M3 to M8 operations | Test location routing, encryption/key handling, restore and billing. Technical buildability does not establish favorable unit economics |
| Universal remote POSIX, live process migration, raw-byte semantic merges | Excluded promise | Would need materially different systems and semantics | Retain native writes, file-state snapshots, format-aware conflict handling and ordinary compute runtimes |
| Guaranteed disk reduction or remote-local speed parity | Excluded promise | Workload-specific measurements can establish bounded results | Retain the roadmap's numerical targets as decision thresholds, not feasibility conclusions |

## Source checks that change implementation choices

### Pinned Xet client has the range primitive

Downloaded the `xet-data` 1.7.0 crate archive from
[crates.io](https://static.crates.io/crates/xet-data/xet-data-1.7.0.crate),
verified its SHA-256 against this repository's `Cargo.lock`, and compared
the download-session source byte-for-byte with the locally installed source.
The archive hash is
`1287a1657fb67de846030f6605a9960dcdad401c467b490657e6e909d2e0d1c0`.

`src/processing/file_download_session.rs` exposes
`FileDownloadSession::download_to_writer` with `RangeBounds<u64>`. Its source
explicitly leaves session-level file-download admission to the caller for
this path. OpenXet currently passes a full-file range in its download
wrapper. A bounded range interface is possible without changing the Xet
wire encoding; a tested seek adapter and its resource accounting are new work.

### macOS needs a real version/backend matrix

[Apple's FSKit overview](https://developer.apple.com/documentation/fskit/)
describes filesystem modules packaged as app extensions. Its
[passthrough example](https://developer.apple.com/documentation/FSKit/building-a-passthrough-file-system)
uses an extension entitlement and an app/extension target. This is not
equivalent to distributing only a Rust CLI binary.

Apple's symbol metadata, read directly from its documentation JSON, reports:

- [UnaryFileSystemExtension](https://developer.apple.com/documentation/fskit/unaryfilesystemextension):
  macOS 15.4, confirmed by the
  [symbol metadata](https://developer.apple.com/tutorials/data/documentation/fskit/unaryfilesystemextension.json).
- [FSGenericURLResource](https://developer.apple.com/documentation/fskit/fsgenericurlresource):
  macOS 26.0, confirmed by the
  [symbol metadata](https://developer.apple.com/tutorials/data/documentation/fskit/fsgenericurlresource.json).

The latter is Apple's documented generic resource for an arbitrary URL,
including a network filesystem. An older framework version does not make
that API available. A different adapter may be possible on older macOS;
it must be tested rather than inferred.

[macFUSE 5.5.0 release notes](https://macfuse.github.io/2026/10/17/macfuse-5.5.0.html)
document read-only mounts for its FSKit backend on macOS 26.4 and later,
and the absence of a per-file fsync callback in FSKit. They also distinguish
the kernel backend from the FSKit backend. These constraints support the
decision to keep initial active workspace writes native. The audit installs
no filesystem component and changes no system extension/security setting;
it does not certify a Mac mount.

### Linux mount feasibility depends on the runtime

[Linux FUSE](https://docs.kernel.org/filesystems/fuse/fuse.html) provides
userspace filesystem operations and non-root mounting with suitable host
support. The [Rust fuser API](https://docs.rs/fuser/0.18.0/fuser/struct.Session.html)
provides a concrete session/callback implementation path. This does not
grant a container access to `/dev/fuse` or permission to mount. Docker's
[runtime capabilities](https://docs.docker.com/engine/containers/run/#runtime-privilege-and-linux-capabilities)
are a separate boundary. A reader or materialized local directory remains
the fallback where mounting is unavailable. No Linux mount was run in this
macOS audit.

### Durable roots require storage and GC work

The baseline `Bucket` trait has durable create-once writes and atomic
replacement, but its key enum is closed over current record kinds.
Repository publication already checks expected parents and fences writers.
Those are useful primitives, not an implemented snapshot or idempotency API.

Add snapshot/pin/publication record kinds, a writer ownership model and
reindex/GC rules in a reviewed design. A root that names a blob must retain
the referenced chunks through collection. Expiry, release and concurrent
publication need a recoverable ordering. For remote backends, use and test
provider preconditions such as
[S3 conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html).
Do not substitute an overwrite for a conditional operation. Preserve the
existing 28-day minimum GC grace; local disk reclamation does not imply
immediate reduction of remote retained bytes.

### Hard byte caps and direct downloads are different contracts

[S3 presigned URLs](https://docs.aws.amazon.com/AmazonS3/latest/userguide/using-presigned-url.html)
can be reused until expiry. OpenXet-token revocation does not automatically
revoke the backend credential that signed such a URL. An application-side
count of issued ranges therefore cannot enforce an actual-egress limit.

Strict delegated egress limits require a mediated transfer path that checks
and reserves budget across concurrent requests, with defined behavior on
partial transfers and retries. This must be reconciled with the approved
M6 statement that tenant reads never stop at a hard budget. Keep basic
scope/expiry delegation on the roadmap; do not promise hard actual-egress
caps or instant direct-URL revocation before a policy and protocol decision.
The audit changes no accepted token lifetime or existing read policy.

## Executed probes

The [probe script](../../scripts/probe-agent-storage.py) creates only
synthetic temporary files, uses existing local Git and DuckDB, and cleans
its fixture on exit. It implements no OpenXet server or mount. Run:

```sh
python3 -I scripts/probe-agent-storage.py --output /tmp/openxet-probes.json
```

The [recorded results](S15-agent-storage-probe-results.json) separate the
execution environment, scope of each check and upstream-source verification.
Environment: macOS 15.6.1, Python 3.14.6, Apple Git 2.39.5 and DuckDB 1.3.2
with its already-installed httpfs extension. The script installs nothing.
It disables automatic extension installation and loading before explicitly
loading the installed httpfs extension.

| Probe | Observed result | Limit |
|---|---|---|
| Git worktree restoration | Recreated an independent repository preserving the unpushed commit, branch, staged binary change, unstaged edit, index entries, selected ignored/untracked files, mode and symlink | Same host and ordinary Git state; not a remote OpenXet snapshot, full index-format adapter or failure test |
| Native file clone | An 8 MiB file cloned through `clonefile` had a different inode; editing the clone left the original bytes unchanged | Single-file behavior; no directory timing, unique-allocation ratio or proof for another filesystem |
| Selective Parquet read | A query selecting 4,096 of 200,000 rows transferred 145,145 response-body bytes in four ranged GETs from a 4,492,137-byte file | Controlled layout and HTTP fixture; not a product benchmark or arbitrary-data reduction guarantee |

The native clone route is documented in
[Apple's clonefile manual](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/clonefile.2).
It requires suitable filesystem support, and later writes may allocate new
blocks or fail for lack of space. Clone directory trees through an
appropriate directory-copy adapter; a single clone call is not a consistent
snapshot of arbitrary live writers.

Git restoration uses supported [bundle](https://git-scm.com/docs/git-bundle),
[binary diff](https://git-scm.com/docs/git-diff) and
[apply](https://git-scm.com/docs/git-apply) operations. A bundle transports
Git objects and references, not every piece of a working directory. The
extra state is captured separately. Production capture must preserve a
declared subset of index/administrative state or reject it before local
release; the probe is evidence of a route, not an exact generic backup tool.

## Prototype gates before making the promises

| Gate | Proof required | If it fails |
|---|---|---|
| F1: range reader | Actual pinned client against OpenXet: seek, overlap, EOF, cancellation, expired credentials and concurrent readers; account for buffers and admission | Use the existing full-file/HTTP path until the adapter is correct |
| F2: shared cache | Multiple processes, crashes, full disk, read leases, bounded reservations and denied access; measure duplication with upstream caching | Restrict to one managed cache domain; no machine-wide hard-bound or tenant-isolation claim |
| F3: mounts | Linux and each advertised Mac OS/backend: attach/unmount, sparse/sequential reads, mmap consumers, metadata scans, cancellation and signed distribution | Ship library/materialization for that platform; retain a prototype status for its mount |
| F4: workspace state | Supported Git-state/metadata matrix, compatible-platform restoration, untracked/ignored policy, source isolation and sufficient space | Capture only supported state, reject unsupported release and preserve originals |
| F5: durable release | Root/pin recovery after index loss, GC races, every crash boundary, rechecks and runtime-guaranteed writer exclusion through local release | Capture/restore only; no automated local removal for unmanaged workspaces |
| F6: delegation and costs | Explicit revocation/expiry/budget contract on direct and mediated paths, concurrent/replayed requests, retained-byte and egress economics | Ship supported scope/expiry semantics and soft cost reporting; require a separate decision before hard caps |

Performance gates from the vision apply after these mechanisms work. The
50% disk target and 10% warm-task threshold remain proposed acceptance
criteria. None of the probes proves them. Basic buildability is established
more strongly than before; production feasibility remains conditional on
passing these named integration and failure tests.
