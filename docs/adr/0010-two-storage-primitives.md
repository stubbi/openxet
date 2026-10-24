# 0010: Store data as two primitives, repositories and buckets

<!-- description: openxet has two storage primitives, the versioned repository and the mutable bucket; Hugging Face's repo types are labels on a repository. -->

- Status: accepted
- Date: 2026-10-24
- Scope: `hub`, `cas`, `index`, `gc`, `web`, `client`, `docs`

## Context

The design listed five repo types, `model`, `dataset`, `space`, `kernel`
and `bucket`, mirroring huggingface.co (design §6 and §10.4 before this
ADR), and §4 recorded "all repo types including `bucket`" as a decision
with the owner. Four of them behave identically: a Git history of
commits and refs, with large files in Xet. The fifth, `bucket`, is
different in kind: mutable objects under keys, with no Git history.

openxet serves anyone storing large versioned or deduplicated files, not
only machine learning (design §1). Mirroring Hugging Face's artefact
taxonomy would make a CI artefact store or a log archive pick between
"model" and "dataset".

What stock clients need, checked in `huggingface_hub` 2.2.0 (the version
the compatibility suite pins), `hf_api.py` and `utils/_xet.py`:
- Repositories are addressed per type: `/api/models/…`,
  `/api/datasets/…`, `/api/spaces/…`, and `/api/kernels/…` for kernels
  (`REPO_TYPES_WITH_KERNEL` in `constants.py`). A model and a dataset may
  share a name, so the type is part of a repository's address. Web, Git
  and resolve URLs carry the same prefixes (`REPO_TYPES_URL_PREFIXES`:
  `datasets/`, `spaces/`, `kernels/`, none for models).
- `kernel` repositories can be created, deleted, read and downloaded:
  `create_repo` and `delete_repo` check `REPO_TYPES_WITH_KERNEL`
  (`hf_api.py` lines 4602 and 4760), as do `hf_hub_download` and
  `snapshot_download` (`file_download.py`, `_snapshot_download.py`), and
  `kernel_info` reads `/api/kernels/{id}`. But `create_commit`,
  `preupload_lfs_files`, `upload_file` and `upload_folder` accept only
  `REPO_TYPES`, which has no `kernel` (`hf_api.py` lines 5084, 5328, 5673
  and 5888). So stock clients write kernels over Git (`git-xet` and
  `git-lfs`), not through the Hub commit API.
- Storage Buckets have their own API: `/api/buckets/{namespace}/{name}`
  for create, info and settings, `/api/buckets/{id}/tree`,
  `/paths-info` and `/batch`, downloads from
  `/buckets/{id}/resolve/{path}`, and Xet tokens from
  `/api/buckets/{id}/xet-{read|write}-token` with no revision. `bucket` is
  not in `REPO_TYPES`; it appears only as a URI type (`HfUriType`).

The owner decided on 2026-10-24 that openxet has two storage primitives.

## Decision

1. **Two primitives.**
   - A **repository** is versioned and Git and Hub compatible: commits,
     refs, and large files as LFS pointers to Xet files (design §6,
     §10.9).
   - A **bucket** holds mutable objects under keys, with no Git history.
     Every object is still a Xet file, chunked and deduplicated like any
     other (design §10.4).
2. **Repo types are labels.** `model`, `dataset`, `space` and `kernel`
   are labels on a repository. A repository carries one label, chosen at
   creation, which selects its Hub route prefix and is part of its
   address, so names may repeat across labels as on huggingface.co. A
   label changes no behaviour: every repository has the same commits,
   refs, LFS and Xet paths, and `space` runs nothing.
3. **Compatibility is unchanged.** The Hub API keeps every route stock
   clients call, per label (`/api/models/…`, `/api/datasets/…` and the
   rest), and buckets answer the Storage Buckets routes above. The label
   prefixes apply to resolve and Git URLs too. Labels need no work
   beyond M2's Hub API and Git hosting.
4. **Delivery.** M2 ships repositories with all four labels. M5 ships the
   bucket primitive (design §12).
5. **Invariants hold for buckets too** (design §10.4 has the full
   write path).
   - Each write request (one `batch` call) creates, only if absent, one
     log record `bucket-log/{sha256(scope)}/{seq}` holding all of its key
     operations, before the acknowledgement. `seq` is allocated in an
     index transaction. If the record exists, another node holds the
     number, and the writer allocates a new one, so two nodes never both
     win it.
   - The index applies an operation to a key only if its `seq` is higher
     than the key's current one, and keeps tombstones for deleted keys,
     so the index and `reindex` pick the same last writer, and applying
     a record twice changes nothing.
   - A periodic snapshot `bucket-snapshot/{sha256(scope)}/{seq}` bounds
     replay: `reindex` reads the latest snapshot plus the later records.
   - Overwriting or deleting a key only drops a reference; bytes leave
     through GC (non-negotiable 3).

   The record is written synchronously, unlike spike S12's proposal of an
   asynchronous log with up to 60 s of staleness: a bucket's key mapping
   lives nowhere else, so an acknowledged write must already be in the
   object store (non-negotiable 2).
6. **Naming.** "The bucket is the source of truth" means the object-store
   bucket. Where the two could be confused, docs say "object-store
   bucket" or "bucket primitive".

An S3-compatible API over buckets is under evaluation in
[spike S12](../research/S12-s3-api-and-dedup-economics.md) and awaits the
owner's decision. This ADR does not
schedule it.

## Consequences

- This bends the earlier decision in design §4, "all repo types
  including `bucket`": the scope is the same, but `space` and `kernel`
  are no longer separate work in M5, and `bucket` is a primitive rather
  than a repo type.
- The index, GC and metering deal with two kinds of reference: commits
  in a repository, and current keys (plus kept versions) in a bucket.
- Each bucket write costs one extra object-store PUT, next to the xorb,
  shard and registration writes it already makes. It uses the
  create-only-if-absent write M1-R37 already requires, so it adds no new
  store requirement. M5 benchmarks small-object writes.
- Churn is stored through GC's grace (28 days minimum, 30 by default,
  design §5.6). S12 estimates that a bucket keeping its last three hourly
  10 GB dense checkpoints holds about 7.2 TB through the default grace,
  240 times its live size. S12 asks whether xorbs never offered in a
  dedup answer could take a shorter grace; that needs its own decision.
- A repository's Hub scope is `{label}/{namespace}/{name}` and a
  bucket's is `bucket/{namespace}/{name}`; `bucket` is never a label, so
  they can't collide. Both map to the immutable scope ids the M2 plan
  defines, so a rename never orphans data.
- Design §5.9 requirements 2 and 7 cover buckets: credentials and
  creation are scoped per bucket, a `batch` that adds or copies a file by
  Xet hash must name a file the caller can read (else the `404` of an
  unknown hash), and an acknowledged key write survives the loss of the
  database. Their tests are in the [security test
  map](../reference/security-tests.md) for M5.
- The compatibility suite reads every label with stock `huggingface_hub`
  (repo info, listing, `hf download`) and writes it with whichever stock
  client supports it: Hub commits for `model`, `dataset` and `space`, Git
  with `git-xet` for `kernel`. It tests the Storage Buckets routes in M5
  with stock `huggingface_hub`.

## Alternatives considered

- **Five repo types, as before.** Rejected: four of them are the same
  thing under different names, and the taxonomy fits ML only.
- **One primitive, a bucket with optional history.** Rejected: Git and
  Hub compatibility need real commits and refs, and a bucket needs
  neither.
- **Free-form labels.** Rejected for now: stock clients know only the
  four labels above, and nothing uses others yet.
