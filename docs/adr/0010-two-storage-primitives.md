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
  share a name, so the type is part of a repository's address.
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
   rest), and buckets answer the Storage Buckets routes above. Labels
   need no work beyond M2's Hub API.
4. **Delivery.** M2 ships repositories with all four labels. M5 ships the
   bucket primitive (design §12).
5. **Invariants hold for buckets too.** A bucket's keys are recorded in
   the object store before a write is acknowledged, so `reindex` rebuilds
   them (non-negotiable 2). Overwriting or deleting a key only drops a
   reference; bytes leave through GC (non-negotiable 3).
6. **Naming.** "The bucket is the source of truth" means the object-store
   bucket. Where the two could be confused, docs say "object-store
   bucket" or "bucket primitive".

An S3-compatible API over buckets is under evaluation in spike S12
(draft PR #60) and awaits the owner's decision. This ADR does not
schedule it.

## Consequences

- This bends the earlier decision in design §4, "all repo types
  including `bucket`": the scope is the same, but `space` and `kernel`
  are no longer separate work in M5, and `bucket` is a primitive rather
  than a repo type.
- The index, GC and metering deal with two kinds of reference: commits
  in a repository, and current keys (plus kept versions) in a bucket.
- A repository's Hub scope is its label, namespace and name; a bucket's
  scope is the bucket.
- The compatibility suite tests every label on the Hub routes in M2 and
  the Storage Buckets routes in M5, with stock `huggingface_hub`.

## Alternatives considered

- **Five repo types, as before.** Rejected: four of them are the same
  thing under different names, and the taxonomy fits ML only.
- **One primitive, a bucket with optional history.** Rejected: Git and
  Hub compatibility need real commits and refs, and a bucket needs
  neither.
- **Free-form labels.** Rejected for now: stock clients know only the
  four labels above, and nothing uses others yet.
