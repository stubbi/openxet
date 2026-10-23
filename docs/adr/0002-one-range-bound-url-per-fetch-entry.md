# 0002: Serve reconstructions with one range-bound URL per fetch entry

- Status: accepted
- Date: 2026-10-23
- Scope: `cas`, `storage`, `transfer`, `hub`

## Context

Design §5.5 wants downloads to bypass openxet: the CAS returns a small
reconstruction, and the client fetches xorb bytes directly from the object
store. Design §3 assumed that one range per fetch entry, pointing at a
native presigned URL, would work with stock clients. Spike S1/S2 tested
that with the real client (`docs/research/S1-S2-direct-downloads.md`):
`huggingface_hub` 2.2.0 and `hf_xet` 1.7.0, driven by the Task 5 harness,
against a stub CAS and MinIO.

Verified by running the client:
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
  With the flag on, it sends a multi-range header:
  - MinIO answered `200` with the whole xorb, and the download failed.
  - Anonymous GETs of public objects showed AWS S3 and Azure answering
    `200` with the whole object, and GCS answering `400`.
- **v1 fallback works.** When `/v2` answered 404, the client fell back to
  `/v1/reconstructions` and succeeded.
- **Expired URLs recover.** On a `403`, the client re-requests the block's
  reconstruction once and retries. Recovery failed when URL lifetimes were
  shorter than its retry backoff (3 s base) plus clock skew. Expiry is
  checked when a request starts, not mid-transfer.

The security side:
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
   - openxet never puts more than one range in a fetch entry, on any
     backend.
2. **URLs are range-bound and short-lived.** Each URL authorises one xorb,
   one byte range and one expiry, and nothing wider. Per backend class:

   | backend class | what the fetch entry URL is |
   |---|---|
   | **native presign: S3 and S3-compatible** | SigV4 query-presigned GET with `range` in `SignedHeaders` |
   | **native presign: GCS** | V4 signed URL with `range` as a signed header, if M1 verifies GCS enforces it; otherwise the transfer role |
   | **native presign: Azure Blob** | the transfer role, unless M1 verifies a SAS can bind a range. An unbound SAS is only allowed as an explicit operator opt-in that accepts whole-xorb exposure for the URL's lifetime |
   | **transfer role: local disk, NVMe or CDN cache tier, any backend without a verified range-bound presign** | an openxet-signed URL to the transfer role, binding xorb hash, range and expiry. The transfer role answers a single `Range` GET with `206` |

   The transfer role does not need `multipart/byteranges`. With rule 1, no
   client ever sends it a multi-range request.
3. **v2 and v1.** `GET /v2/reconstructions/{file_id}` is the primary
   endpoint. `GET /v1/reconstructions/{file_id}` is served with the same
   terms and URLs for older clients. Both:
   - honour the file `Range` header
   - return `416` past EOF
   - set `offset_into_first_range`
4. **Lifetimes.** URL lifetime is configurable, with a default of 1 hour
   and a floor of 60 s. The floor stays above the client's retry backoff
   (`retry_base_delay` 3 s, exponential, 5 attempts in 1.7.0) plus
   realistic clock skew. On a refresh, openxet returns new URLs for
   the same block with the same structure. The client checks that the
   refreshed block covers the same byte range.

## Consequences

- **Easier:**
  - Downloads scale with the object store, not with openxet nodes, on S3
    and S3-compatible stores.
  - The same response shape serves every client setting:
    - `enable_multirange_fetching` on or off
    - v1 or v2
  - The transfer role stays a plain single-range server.
- **Harder:**
  - A reconstruction carries one signed URL per range instead of one per
    xorb. That means more signing work and bigger JSON for heavily
    deduplicated files. The number of object-store GETs is unchanged,
    because default clients already split.
  - Benchmarks in M1 must show the signing cost is acceptable.
- **We must keep true:**
  - No fetch entry ever has more than one range.
  - No URL is wider than one xorb range unless the operator opted in.
  - URL lifetime stays at or above the floor.
- **Tests that enforce this:** the compatibility suite downloads through
  openxet:
  - for each backend class in CI, at least MinIO and local disk
  - with `HF_XET_CLIENT_ENABLE_MULTIRANGE_FETCHING` both unset and `true`
  - with `HF_XET_CLIENT_RECONSTRUCTION_API_VERSION=1`

  A CAS unit test asserts the one-range-per-entry invariant and that every
  object-store URL carries the range binding. The redacted exchanges in
  `compat/fixtures/s1/` are the reference shapes.
- **Open, owned by M1:**
  - Verify range binding on real AWS S3 and GCS, and SAS range binding on
    Azure.
  - Until that is verified, those backends use the transfer role by
    default.

## Alternatives considered

- **Multi-range entries on presigned URLs, like the v2 type allows.**
  - Works today only because of a client default.
  - Breaks for any client that enables multi-range: whole-object `200` on
    S3, MinIO and Azure, `400` on GCS.
  - Cannot be range-bound.
- **Unbound presigned URLs: one per xorb, client sends ranges.**
  - Simplest, and it works with stock clients.
  - Grants the whole shared xorb for the URL's lifetime, so it leaks data
    across scopes.
  - Rejected as a default; kept only as an explicit operator opt-in.
- **Always proxy through the transfer role.**
  - Uniform and easy to bind.
  - Puts every downloaded byte through openxet nodes, against design §5.5,
    when the store can do it natively.
- **Multi-range entries served by the transfer role with
  `multipart/byteranges`** (the original design §5.5 b).
  - Saves requests only for opted-in clients.
  - Costs a multipart encoder, parser surface and tests for no benefit to
    default clients.
