# 0002: Serve reconstructions with one range-bound URL per fetch entry

<!-- description: Every reconstruction answer has one byte range per fetch entry, each with its own short-lived, range-bound URL. -->

- Status: accepted
- Date: 2026-10-23
- Scope: `cas`, `storage`, `transfer`, `hub`, `server`

## Context

Design §5.5 wants downloads to bypass OpenXet: the CAS returns a small
reconstruction, and the client fetches xorb bytes directly from the object
store. Design §3 assumed that one range per fetch entry, pointing at a
native presigned URL, would work with stock clients. Spike S1/S2 tested
that with the real client (`docs/research/S1-S2-direct-downloads.md`):
`huggingface_hub` 2.2.0 and `hf_xet` 1.7.0, driven by the Task 5 harness,
against a stub CAS and MinIO.

**Observed by running the client against MinIO:**
- **Native presigned URLs work.** Stock `hf_xet` downloaded the reference
  file byte-exact from SigV4 presigned URLs:
  - one entry with one range, the shape huggingface.co returns
  - four entries with one range each
  - with `Range` as a signed header (`X-Amz-SignedHeaders=host;range`)
- **The client sends exactly the entry's range.** One range per entry gives
  `Range: bytes=start-end`, with no `X-Xet-Signed-Range` and no URL
  parsing. No CAS `Authorization` is sent to the object store.
- **Multi-range entries only work because of a client default.** In 1.7.0,
  `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` is `false` by default, and the
  client splits multi-range entries into single-range GETs on the same URL.
  With the flag on, it sends a multi-range header. MinIO answered `200`
  with the whole xorb, and the download failed.
- **v1 fallback works.** When `/v2` answered 404, the client fell back to
  `/v1/reconstructions` and succeeded.
- **Expired URLs recover.** On a `403`, the client re-requests the block's
  reconstruction once and retries with the new URLs. Recovery failed when
  every URL, including the refreshed ones, was already expired on arrival.
  Expiry is checked when a request starts, not mid-transfer.

**Observed only through anonymous GETs of public objects** (not presigned,
not `hf_xet`; commands and responses in
`compat/fixtures/s1/cloud-multirange.txt`):
- AWS S3 and Azure answer a multi-range GET with `200` and the whole
  object.
- GCS answers it with `400`.

How presigned URLs behave on AWS S3, GCS and Azure is **unverified**.
That includes whether they can bind a range.

**Inferred from xet-core v1.7.0 source, not observed:**
- On a 403, the client refreshes before it sleeps
  (`remote_client.rs:633-638`).
- It then sleeps a pre-jitter 3, 9, 27, 81 and 243 s for attempts 1–5.
  Each sleep is capped at `retry_max_duration`, 6 min, and jitter only
  shortens it (`retry_wrapper.rs:272`, `485-497`; `client.rs` retry
  defaults).
- So a refreshed URL must outlive the next sleep, not just the 3 s base
  delay.

**The security side:**
- An unbound presigned GET reads the **whole** xorb.
- Xorbs are globally content-addressed and shared across scopes (design
  §5.4), so a whole-xorb URL can expose chunks of files the caller may not
  read.
- huggingface.co binds each URL to its exact range: the CloudFront policy
  carries `ByteRange.ExpectedHeader`.

## Decision

1. **Reconstruction shape.** Every reconstruction response, v2 and v1, has
   exactly **one range per fetch entry**, and every entry has its own URL:
   - v2: `xorbs[hash]` is a list of `{url, ranges: [one descriptor]}`.
   - v1: one `fetch_info` entry per range.
   - OpenXet never puts more than one range in a fetch entry, on any
     backend.
2. **Every URL is range-bound and short-lived.** Each URL authorises one
   xorb, one byte range and one expiry, and nothing wider. OpenXet never
   serves an unbound URL.
3. **Direct mode is gated per deployment by a range-binding enforcement
   probe, not by a backend list.**
   - **When it runs:** at startup, and on demand in `openxet doctor` and
     `openxet config check`.
   - **What it does:** OpenXet writes a probe object to each storage pool
     and presigns a GET for one byte range with the pool's signer. Then it
     checks all three of:
     1. that signed range → `206`
     2. a different range on the same URL → `403`
     3. no `Range` header → any `4xx`
   - **Pass:** only if all three pass does that pool serve direct URLs.
   - **Fail:** otherwise the pool falls back to the transfer role. OpenXet
     logs which check failed and with what status, and `doctor` reports it
     with the next step.

   This applies to every backend: S3, R2, Garage, MinIO, and GCS and Azure
   once their signers can bind a range. MinIO passed all three checks by
   hand in this spike.

   | pool | what the fetch entry URL is |
   |---|---|
   | **probe passed** (any presign-capable store) | native presigned GET with the range bound into the signature, for example SigV4 with `range` in `SignedHeaders` |
   | **probe failed, or no native presign** (local disk, NVMe or CDN cache tier, any store whose signer cannot bind a range) | an OpenXet-signed URL to the transfer role, binding xorb hash, range and expiry. The transfer role answers a single `Range` GET with `206` |

   The transfer role does not need `multipart/byteranges`. With rule 1, no
   client ever sends it a multi-range request.
4. **v2 and v1.** `GET /v2/reconstructions/{file_id}` is the primary
   endpoint. `GET /v1/reconstructions/{file_id}` is served with the same
   terms and URLs for older clients. Both:
   - honour the file `Range` header
   - return `416` past EOF
   - set `offset_into_first_range`

   On a refresh (the same file and block `Range` asked again), OpenXet
   returns fresh URLs with the **same terms and the same per-entry ranges**.
   The client itself only checks that the refreshed block covers the same
   byte range (`retrieval_urls.rs:103-108`).
5. **URL lifetime: default 60 min, minimum 30 min, from this derivation;
   maximum 60 min.** *Amended (M1-R42):* the maximum bounds how long a
   leaked URL, or one issued before a purge (design §5.4), stays usable.

   `floor = (URL age at first use) + (largest sleep after a refresh) + (clock skew)`

   - **URL age at first use.** `hf_xet` requests reconstruction for blocks
     ahead of the bytes it is writing. Its prefetch buffer is
     `max(target_block_completion_time × rate, min_prefetch_buffer)`:
     15 min × rate, or 1 GB (10⁹ bytes), whichever is larger. A block is
     256 MB to 8 GB and never larger than that buffer
     (`reconstruction_terms/manager.rs:204-233`; `reconstruction.rs`
     defaults). So a URL waits about `buffer ÷ rate` before its first GET:
     - **Assumed minimum supported bandwidth `B_min`:** 1 MB/s (8 Mbit/s)
       of reconstructed file bytes.
     - At `B_min`: `max(900 s, 10⁹ B ÷ 10⁶ B/s) = 1000 s`.
   - **Largest sleep after a refresh.** Refresh happens before the sleep,
     so the new URL must survive one sleep: at most 243 s with defaults,
     and 360 s at the `retry_max_duration` cap. Take **360 s**.
   - **Clock skew bound.** This is the skew between OpenXet's signing clock
     and the store's clock; the client's clock does not matter. Take
     **60 s**. The spike measured 2 s between the macOS host and the
     colima VM.
   - **Result:** `1000 + 360 + 60 = 1420 s`, about 23.7 min. Rounded up,
     that gives a **minimum of 30 min**. The default is **60 min**, the
     same as huggingface.co's observed xorb URL lifetime (3600 s).

   Below `B_min` a download still completes. Blocks whose URLs aged out
   cost one 403 and one refresh each, the recovery the spike observed.
   Lifetimes below the minimum fail config validation with an actionable
   error.

## Consequences

- **Easier:**
  - Downloads scale with the object store, not with OpenXet nodes, on any
    pool that passes the probe.
  - The same response shape serves every client setting:
    - `enable_multirange_fetching` on or off
    - v1 or v2
  - The transfer role stays a plain single-range server.
- **Harder:**
  - A reconstruction carries one signed URL per range instead of one per
    xorb. That means more signing work and bigger JSON for heavily
    deduplicated files. The number of object-store GETs is unchanged,
    because default clients already split.
  - Benchmarks in M3, with the production backends, must show the
    signing cost is acceptable.
  - Startup does one probe write and three probe GETs per pool.
- **We must keep true:**
  - No fetch entry ever has more than one range.
  - No URL is wider than one xorb range.
  - Direct URLs only come from pools whose probe passed.
  - URL lifetime is at least 30 min and at most 60 min. If `hf_xet`
    changes its prefetch or retry defaults, re-derive the floor.
- **Tests that enforce this:**
  - **Range-binding probe:**
    - a unit test of the three checks and the fallback decision, with a
      fake store for each failure mode
    - in CI, the probe runs against MinIO and must pass
    - in CI, the probe runs against a deliberately unbound signer and must
      fall back to the transfer role
  - **Compatibility suite:** downloads through OpenXet:
    - against a probe-passing MinIO pool and a local-disk pool
    - with `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` both unset and `true`
    - with `HF_XET_CLIENT_RECONSTRUCTION_API_VERSION=1`
  - **Expiry path:**
    - a compat leg in the style of S1 variant d: the first object-store
      GETs see `403`, the client refreshes once, and the download succeeds
    - a CAS test that a refreshed reconstruction for the same block keeps
      the same terms and per-entry ranges, with only the URLs changing
  - **Lifetime floor:** a config-validation test. 29 min and 0 are rejected
    with an error naming the minimum; 30 min and 60 min are accepted.
  - **Shape:** a CAS unit test asserts the one-range-per-entry invariant
    and that every direct URL carries the range binding. The redacted
    exchanges in `compat/fixtures/s1/` are the reference shapes.
- **Open, owned by M3** (production backends, design §12):
  - Run the probe against real AWS S3, R2, Garage, GCS and Azure, and
    record which pass.
  - Until a store passes, its pools use the transfer role. That follows
    from rule 3; no backend list is needed.

## Alternatives considered

- **Multi-range entries on presigned URLs, like the v2 type allows.**
  - Works today only because of a client default.
  - Breaks for any client that enables multi-range. Whole-object `200` was
    observed on MinIO, and on public AWS S3 and Azure objects. `400` was
    observed on a public GCS object.
  - Cannot be range-bound.
- **Unbound presigned URLs: one per xorb, client sends ranges.**
  - Simplest, and it works with stock clients.
  - Grants the whole shared xorb for the URL's lifetime, so it leaks data
    across scopes.
  - Rejected.
- **A static backend allowlist for direct mode.**
  - Would trust documentation over behaviour.
  - Cannot see S3-compatible stores that ignore signed headers, proxies or
    CDNs in front of a store, or provider changes.
  - The probe checks what this deployment actually enforces.
- **Always proxy through the transfer role.**
  - Uniform and easy to bind.
  - Puts every downloaded byte through OpenXet nodes, against design §5.5,
    when the store can do it natively.
- **Multi-range entries served by the transfer role with
  `multipart/byteranges`** (the original design §5.5 b).
  - Saves requests only for opted-in clients.
  - Costs a multipart encoder, parser surface and tests for no benefit to
    default clients.
