# 0010: Store data as two primitives, repositories and buckets

<!-- description: openxet has two storage primitives, the versioned repository and the mutable bucket, each with generic labels; Hugging Face's repo types are labels the Hub layer reads and writes. -->

- Status: accepted
- Date: 2026-10-24
- Scope: `hub`, `git`, `cas`, `index`, `gc`, `web`, `client`, `docs`

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
the compatibility suite pins), `hf_api.py`, `utils/_http.py`,
`utils/_xet.py` and `constants.py`, and in the huggingface.co exchanges
recorded by S3 (`compat/fixtures/hub/`):
- Repositories are addressed per type: `/api/models/…`,
  `/api/datasets/…`, `/api/spaces/…` and `/api/kernels/…`
  (`REPO_TYPES_WITH_KERNEL`). Web, Git and resolve URLs carry the same
  prefixes (`REPO_TYPES_URL_PREFIXES`: `datasets/`, `spaces/`,
  `kernels/`, none for models). On huggingface.co a model and a dataset
  may share a name.
- `kernel` repositories can be created, deleted, read and downloaded:
  `create_repo` and `delete_repo` check `REPO_TYPES_WITH_KERNEL`
  (`hf_api.py` lines 4602 and 4760), as do `hf_hub_download` and
  `snapshot_download` (`file_download.py`, `_snapshot_download.py`), and
  `kernel_info` reads `/api/kernels/{id}`. But `create_commit`,
  `preupload_lfs_files`, `upload_file` and `upload_folder` accept only
  `REPO_TYPES`, which has no `kernel` (`hf_api.py` lines 5084, 5328, 5673
  and 5888). So stock clients write kernels over Git (`git-xet` and
  `git-lfs`), not through the Hub commit API.
- **A missing repository** is `404` with `X-Error-Code: RepoNotFound`
  and body `{"error":"Repository not found"}` (fixture
  `24-repo-info-not-found`). `hf_raise_for_status` turns
  `X-Error-Code: RepoNotFound` into `RepositoryNotFoundError`
  (`utils/_http.py`, line 933), so `repo_info` with the wrong
  `repo_type` fails that way.
- **An existing repository** on `POST /api/repos/create` is `409` with
  body `{"error": "You already created this model repo: {id}", "url":
  "https://huggingface.co/{id}"}` and the same text in `X-Error-Message`
  (fixtures `05-create-model-exists-conflict` and
  `06-create-model-exist-ok`). `create_repo(exist_ok=True)` swallows a
  `409` and returns `RepoUrl` from the body's `url`; with
  `exist_ok=False` it raises (`hf_api.py` lines 4703–4723).
- Storage Buckets have their own API: `/api/buckets/{namespace}/{name}`
  for create, info and settings, `/api/buckets/{id}/tree`,
  `/paths-info` and `/batch`, downloads from
  `/buckets/{id}/resolve/{path}`, and Xet tokens from
  `/api/buckets/{id}/xet-{read|write}-token` with no revision. `bucket` is
  not in `REPO_TYPES`; it appears only as a URI type (`HfUriType`).

The owner decided on 2026-10-24 that openxet has two storage primitives,
and then that labels are generic, not a fixed set.

## Decision

1. **Two primitives.**
   - A **repository** is versioned and Git and Hub compatible: commits,
     refs, and large files as LFS pointers to Xet files (design §6,
     §10.9).
   - A **bucket** holds mutable objects under keys, with no Git history.
     Every object is still a Xet file, chunked and deduplicated like any
     other (design §10.4).
2. **Addresses carry no type.** A repository is addressed by
   `{namespace}/{name}` only, and so is a bucket, in its own space.
3. **Labels are generic.** Repositories and buckets carry free-form,
   owner-defined labels: none, one or several, such as `model`,
   `dataset`, `checkpoint` or `eval-logs`.
   - A label is a lowercase slug (`a`–`z`, `0`–`9` and `-`) of at most
     64 bytes, and an object carries at most 32.
   - Labels can be added and removed at any time, through openxet's own
     API and CLI, each change with an audit entry. They never change the
     address, and they change no behaviour: `space` runs nothing.
   - Hugging Face's types `model`, `dataset`, `space` and `kernel` are
     not special in the model. They are the labels the Hub layer reads
     and writes.
4. **Hub routes are views.** `/api/models/…`, `/api/datasets/…`,
   `/api/spaces/…`, `/api/kernels/…`, and the prefixed resolve and Git
   URLs, are views of the same repository through one label:
   - Creating a repository through a typed route adds that type's label
     (`POST /api/repos/create` with `type`). A create with no `type` is
     the `model` view and adds the `model` label, as on huggingface.co:
     `create_repo` sends `type` only for non-model types (`hf_api.py`
     line 4641).
   - Listing through a typed route (`/api/models?…`) returns only
     repositories with its label.
   - Reading or writing an existing repository through a typed route
     whose label it lacks answers `404` with `X-Error-Code:
     RepoNotFound`, so stock clients raise `RepositoryNotFoundError`, as
     huggingface.co makes them do for a wrong `repo_type`. Its message
     depends on what the caller may read (the M1-R17 pattern):
     - A caller who can read the repository gets an actionable message,
       in `X-Error-Message` and the body's `error`: the repository
       `foo/bar` exists without the `dataset` label; add it with
       `openxet label add foo/bar dataset` (or the label API), or choose
       another name. `hf_raise_for_status` appends both to the raised
       error's text (`utils/_http.py`, `_format`, lines 1035–1101: it
       reads `X-Error-Message` and the JSON `error`, deduplicates them and
       adds them after its own "Repository Not Found" text), so the hint
       reaches the user of `hf` and `huggingface_hub`.
     - Any other caller gets exactly an unknown repository's response:
       the same status, `X-Error-Code`, `X-Error-Message`
       (`Repository not found`) and body (`{"error":"Repository not
       found"}`), and no other header that differs, apart from
       per-request ids and dates. Any difference would reveal that the
       repository exists.
   - The unprefixed resolve route and `/api/models/…` are the `model`
     view, as on huggingface.co. The unprefixed Git URL
     `/{namespace}/{name}.git` is openxet's own address and serves the
     repository whatever its labels.
   - Buckets answer the Storage Buckets routes above, which have no
     types; their labels are set through openxet's API.
5. **Names are unique across labels.** Unlike huggingface.co, a model
   and a dataset can't share `{namespace}/{name}`. Creating
   `datasets/foo/bar` while a repository `foo/bar` exists answers `409`
   in the shape above. Create is authorized on the namespace before the
   name is looked up, so only callers allowed to create there see the
   `409`; for a caller who can't read the existing repository it carries
   no labels. Its `url` is the requested view's own URL
   (`{public_url}/{type prefix}{namespace}/{name}`), which `RepoUrl`
   parses (`hf_api.py` lines 545–571).
   An importer or mirror (design §10.6) that meets two Hub repositories
   of different types under one name maps the second to `{name}-{type}`
   and reports the mapping.
6. **Delivery.** M2 ships repositories, label CRUD and the typed-route
   views for all four Hub types. M5 ships the bucket primitive (design
   §12).
7. **Invariants hold for buckets too** (design §10.4 has the full
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
   - A sweeper aborts a `seq` still pending past the request deadline by
     creating that same log key, only if absent, with an abort record.
     Writer and sweeper race on one create-only key, so exactly one
     wins, and a writer that loses reports the write as failed. A write
     takes effect if and only if its record, not an abort, owns its
     `seq`, and it is acknowledged only then.
   - A snapshot `bucket-snapshot/{sha256(scope)}/{seq}` is taken only at
     a watermark W, the highest `seq` with every `seq` up to and
     including W applied or aborted. `reindex` replays from the latest snapshot, skips aborts,
     and sets the next `seq` above the highest record it found, aborts
     included.
   - Overwriting or deleting a key only drops a reference; bytes leave
     through GC (non-negotiable 3).

   The record is written synchronously, unlike spike S12's proposal of an
   asynchronous log with up to 60 s of staleness: a bucket's key mapping
   lives nowhere else, so an acknowledged write must already be in the
   object store (non-negotiable 2).
8. **Naming.** "The bucket is the source of truth" means the object-store
   bucket. Where the two could be confused, docs say "object-store
   bucket" or "bucket primitive".

An S3-compatible API over buckets is under evaluation in
[spike S12](../research/S12-s3-api-and-dedup-economics.md) and awaits the
owner's decision. This ADR does not schedule it.

## Consequences

- This bends the earlier decision in design §4, "all repo types
  including `bucket`": the scope is the same, but `space` and `kernel`
  are no longer separate work in M5, and `bucket` is a primitive rather
  than a repo type.
- **The trade-off:** a name is taken across all labels, which
  huggingface.co doesn't do. We accept it for generic storage with Hub
  types as a compatibility view: the address stays stable while labels
  change, and nothing outside ML has to pick a type. With
  `create_repo(exist_ok=True)`, a collision reads as success, so
  `hf upload --repo-type dataset foo/bar` onto a `foo/bar` without the
  `dataset` label reaches the typed view's `404`. That `404` tells a
  caller who can read the repository how to add the label or pick
  another name, so it isn't a dead end.
- A label change reaches the object store before it is acknowledged,
  like a commit object, so `reindex` restores labels; every change is an
  audit entry (M4's audit log).
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
- A repository's scope is `{namespace}/{name}` and a bucket's is
  `bucket/{namespace}/{name}`, so they can't collide (two path segments
  against three). Labels are not part of either. Both map to the
  immutable scope ids the M2 plan defines, so a rename never orphans
  data.
- Design §5.9 requirements 2 and 7 cover buckets: credentials and
  creation are scoped per bucket, a `batch` that adds or copies a file by
  Xet hash must name a file the caller can read (else the `404` of an
  unknown hash), and an acknowledged key write survives the loss of the
  database. Their tests are in the [security test
  map](../reference/security-tests.md) for M5.
- The compatibility suite reads every Hub label with stock
  `huggingface_hub` (repo info, listing, `hf download`) and writes it
  with whichever stock client supports it: Hub commits for `model`,
  `dataset` and `space`, Git with `git-xet` for `kernel`. In M2 it also
  checks that a create with no type is a model (`hf upload` with no
  `--repo-type`, then `repo_info` and `snapshot_download`), a typed-route
  view, a create collision's `409`, a wrong-type
  `404` that shows the label hint to a caller with read access, and one
  that matches an unknown repository's response (status, headers apart
  from per-request ids and dates, and body) for a caller without it
  (design §6). It tests the Storage Buckets routes in M5 with stock
  `huggingface_hub`.

## Alternatives considered

- **Five repo types, as before.** Rejected: four of them are the same
  thing under different names, and the taxonomy fits ML only.
- **One fixed label per repository, part of its address** (this ADR's
  first version). It kept huggingface.co's per-type names, but tied the
  address to an ML type, allowed exactly one, and made relabelling a
  rename. Replaced by generic labels on 2026-10-24.
- **One primitive, a bucket with optional history.** Rejected: Git and
  Hub compatibility need real commits and refs, and a bucket needs
  neither.
