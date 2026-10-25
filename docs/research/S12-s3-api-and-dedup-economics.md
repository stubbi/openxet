# S12: An S3 API in front of buckets, and what dedup saves

<!-- description: Whether an S3-compatible API over OpenXet buckets is feasible, what CDC dedup saves on multi-tenant data, a cost model against S3, R2, B2, Wasabi and Hetzner, and the security and billing consequences. -->

- Date: 2026-10-24. Every URL below was accessed on that date.
- Status: the measurements and prices are a record. The recommendation
  (an S3 API in M5, the price band) is a proposal pending the owner's
  decision; the design schedules nothing from it until then.
- Question (from the user): "does an S3-compatible API in front of buckets
  mean we could host a very cost-efficient S3 multi-tenancy for our
  users?" "Buckets" means OpenXet's planned generic bucket primitive
  (mutable, non-versioned, next to the versioned repository; design §10.4
  describes it today as the `bucket` repo type). The question splits into:
  is it feasible, what does dedup save, what does it cost, what does it do
  to tenant isolation, and should it be in scope.
- Versions examined:
  - Client sources, downloaded as release tarballs: `botocore` 1.43.118,
    `s3transfer` 0.19.3, `s3fs` 2026.9.0, rclone v1.75.2, s5cmd v2.3.0,
    restic v0.19.1, `minio-go` v7.4.1, `object_store` v0.14.2 (polars),
    `duckdb-httpfs` `77392a72` (the commit DuckDB v2.0.0 pins, as in
    [S4](S4-analytics-clients.md)).
  - `s3s` `main` on 2026-10-24 (crate 0.17.0, published 2026-09-24).
  - `xet-data` and `xet-core-structures` 1.7.0, as pinned by OpenXet.
  - Price pages and price-list files listed in "Prices", read on
    2026-10-24.

Labels used below:
- **Verified:** read in a source file, a vendor document or a price list
  at the cited URL or path, on the date above.
- **Measured:** our experiment, see "Experiment"; method and inputs are
  given so it can be rerun.
- **Estimated:** our model or judgement. The assumptions are stated next
  to each number.

## Answer

Feasible, yes; "very cost-efficient" only for some workloads.

- **Feasibility (Verified).** Hugging Face already runs an S3 gateway in
  front of Xet-backed Storage Buckets (`s3.hf.co`), documented for the
  AWS CLI, `boto3`, s5cmd, rclone, DuckDB and DVC. The S3 subset real
  tools need is about 20 operations plus SigV4. Mapping objects onto Xet
  files works: a PUT is a server-side-chunked upload (design §10.3), a
  multipart upload can produce byte-identical Xet hashes using
  `xet-core`'s stable chunk boundaries, a ranged GET is a reconstruction,
  and a copy is a metadata write. The `s3s` crate (Apache-2.0, Rust)
  covers request parsing, SigV4 and aws-chunked trailers.
- **Dedup (Measured and published).** CDC saves a lot on version series of
  uncompressed or CDC-aware data: the second version of an appended log
  added 10.2% new bytes (Measured), and HF's Gemma GGUF family stored
  191 GB in about 97 GB (published). It saves nothing on consecutive
  dense training checkpoints (0.0%, Measured on Pythia-70m), on
  `.tar.gz`/`.tar.xz` archives (0.0 to 0.1%, Measured), or on client-side
  encrypted backups. Generic S3 data is mostly already compressed, so its
  dedup is small.
- **Cost (Estimated).** On a Hetzner Object Storage or B2 backend, at
  1 PB and with egress equal to stored bytes each month, hosting costs
  about **$11 per TB-month for already-compressed data, $9.40 for fp16
  weights and $5.20 for text** per TB the tenant is billed for. Three
  things limit the savings: an S3 client gets no transfer dedup, every
  byte read goes through our nodes, and garbage collection's 28-day
  minimum grace keeps overwritten data stored for 28 days or more.
- **Price band (Estimated).** **$12 to 15 per TB-month of tenant-deduplicated
  bytes** ($0.012 to 0.015 per GB), with egress up to 1× stored included.
  For a tenant whose own data dedups by 25 to 50%, that is $0.006 to 0.011 per
  logical GB. It beats S3 Standard ($23 + egress), Hugging Face private
  storage ($18, logical bytes) and R2 ($15) for dedup-friendly tenants.
  It does not beat B2 ($6.95) or Wasabi ($7.99) for data that doesn't
  dedup. A flat price per logical GB that is profitable for every tenant
  can't go below about $0.012.
- **Security.** Server-side chunking means S3 clients never see a dedup
  answer, but write timing and any bill or quota that credits cross-tenant
  sharing would still reveal existence. Bill each tenant as if it were
  alone: its own deduplicated bytes, with cross-tenant savings kept as
  margin. That closes the billing channel and costs nothing in storage.
- **Recommendation.** Put a minimal S3 API in **M5**, with buckets,
  because it needs M3's backends and M4's server-side chunking, metering
  and GC, and because huggingface.co's own buckets already have one.
  There is no M8; putting it after 1.0 would contradict §12's scope rule.
  It needs four ADRs (see "ADRs needed").

## 1. Feasibility

### What real tools need

Read in each client's source at the version above (Verified) unless
marked. "ETag use" is what the client does with the ETag value.

| Client | Operations it uses | ETag use | Integrity checks it sends or expects |
|---|---|---|---|
| AWS CLI / `boto3` (`botocore`, `s3transfer`) | Put, multipart (Create, UploadPart, Complete, Abort), Get with `Range`, Head, ListObjectsV2, Copy, UploadPartCopy, Delete, DeleteObjects, presign | `s3transfer` sends the HEAD's ETag as `If-Match` on every ranged part GET of a multipart download (`download.py:515`, `:591`) and on copies (`copies.py:279`). Never compared with an MD5. | Since the "flexible checksums" default, `request_checksum_calculation=when_supported` sends `x-amz-checksum-crc32` (`httpchecksum.py:51`), framed as `aws-chunked` with a trailer for streamed bodies (`:474`). Responses are checked only if they carry `x-amz-checksum-*`. |
| rclone | Put, multipart, Get with `Range`, Head after each upload, ListObjects (V1 for some providers, V2 configurable), Copy (server-side move, modtime update), Delete; modtime in `x-amz-meta-mtime` | Treated as an MD5 **only** if it matches `^[0-9a-f]{32}$` (`backend/s3/s3.go:4028`); otherwise it reads `X-Amz-Meta-Md5chksum` or skips the check. | Sends `Content-MD5` on single-part uploads and compares the returned ETag; checks downloads against the ETag or the metadata MD5 ([rclone S3 docs](https://rclone.org/s3/)). |
| s5cmd | Put, multipart, Get, Head, ListObjectsV2, Copy, Delete, batch delete | Stored and printed only (`storage/s3.go:136`, `:1200`). | None beyond the SDK's. |
| restic (`minio-go`) | Put (multipart above 200 MiB), Get with `Range`, Head (`Stat`), ListObjects (V2 by default, V1 by option), Delete | Not checked; restic verifies its own SHA-256 content addresses. | `SendContentMd5: true` on every save (`internal/backend/s3/s3.go:296`), so the server must verify `Content-MD5`. `minio-go` may add `x-amz-checksum-crc32c`. |
| `s3fs` (fsspec: pandas, HF `datasets`, DVC) | Put, multipart, Get with `Range`, Head, ListObjectsV2, Copy, UploadPartCopy, DeleteObjects | Sends `If-Match` with the HEAD's ETag on reads (`s3fs/core.py:2637`); `checksum()` parses the ETag as hex with `int(etag.split("-")[0], 16)` (`:1844`), so a non-hex ETag raises. | None. |
| DuckDB `httpfs` | Get with `Range`, Head, ListObjectsV2 for globs (`s3/s3_list.cpp:485`), multipart for `COPY … TO` | Sends `If-Match` with the ETag from the listing or HEAD on every ranged read, if the ETag is strong (`http/httpfs.cpp:283`, `http/http_request.cpp:18`). The ETag in a listing must equal the one GET returns. | None. Virtual-host addressing by default; path style with `URL_STYLE 'path'`. |
| polars (`object_store`) | Put, multipart, Get with `Range`, Head, ListObjectsV2 (`aws/client.rs:999`), conditional Put (`If-None-Match: *`, `If-Match`) | Used for conditional requests only. | MD5 only for SSE-C keys (`aws/builder.rs:1547`). |
| MLflow, PyTorch S3 loaders | Not read in source. MLflow's S3 artifact store uses `boto3`; HF `datasets` uses `s3fs` ([docs](https://huggingface.co/docs/datasets/filesystems)). | n/a | n/a |

What this means for the server (Verified from the table):
- **`If-Match` on GET is required**, not optional: AWS CLI multipart
  downloads, `s3fs` and DuckDB send it on ranged reads. huggingface.co's
  gateway documents that it does not honour `If-Match` on `GetObject`
  ([S3 compatibility](https://huggingface.co/docs/hub/storage-buckets-s3)).
  Honouring it costs one comparison.
- **The ETag must be stable and identical** across ListObjectsV2, HEAD and
  GET, hex for `s3fs`, and a strong quoted string for DuckDB. It must be
  the real MD5 whenever it is 32 hex characters, or rclone reports
  corruption. Nothing in the table requires an MD5 ETag on multipart
  objects.
- **`Content-MD5` and `x-amz-checksum-*` must be verified on upload**
  (restic, rclone and every recent AWS SDK send them). Accepting them
  unchecked would be a silent no-op success, which AGENTS.md rules out.
- **`aws-chunked` with trailing checksums must be parsed**, or AWS CLI ≥
  2.23 and recent `boto3` fail with default settings. huggingface.co asks
  users to set `request_checksum_calculation = when_required` instead
  (same page). OpenXet should need no client configuration beyond the
  endpoint and keys.
- **User metadata** (`x-amz-meta-*`, 2 KB) is needed by rclone for
  modification times. huggingface.co does not store it (same page).
- **ListObjects V1** is still the default for some rclone providers and an
  option in restic. It shares V2's implementation apart from paging.

### Mapping objects onto Xet files

| S3 concept | Xet mapping | Notes |
|---|---|---|
| `PutObject` | Server-side chunking (§10.3): chunk, hash, compress into xorbs, register a shard, then point `bucket/key` at the Xet file. | The 200 must follow durable registration (no silent no-op success). |
| Multipart upload | Each part is chunked as it arrives, from the first **stable chunk boundary** after its start; `CompleteMultipartUpload` re-chunks only the seams. | `xet-core-structures` 1.7.0 `next_stable_chunk_boundary` (`xorb_object/constants.rs:57`) defines a boundary after which "any possible changes in the data before `starting_position` would produce the same chunk boundaries" (Verified). Chunking from there gives the same chunks, and so the same Xet file hash, as `hf_xet` uploading the whole file. The seam costs at most a few chunks of rework per part. |
| `GetObject` + `Range` | Reconstruction (v2 plan) for the range, then fetch the xorb ranges, decompress, trim and stream. | S3 clients can't read xorbs, so the node (or the cache tier, §10.5) serves the bytes. Direct presigned xorb URLs (ADR 0002) don't apply. |
| `HeadObject` | Index lookup: size, ETag, content type, metadata, last-modified. | No backend request. |
| `ListObjectsV2` | Keyset scan over the bucket's key index, with `/` delimiter grouping. | AWS S3 guarantees that a LIST after a successful PUT or DELETE reflects it ([consistency model](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html#ConsistencyModel)). That needs reads from the index primary, or a read-your-writes token, never a lagging replica. |
| `CopyObject` | New key pointing at the same Xet file; ETag and metadata copied. | Free and instant, as on huggingface.co. Cross-tenant copies need read permission on the source. |
| `DeleteObject(s)` | Drop the key → file reference; bytes leave only through GC (§5.6). | S3 answers `204` for a missing key; that is S3's idempotent contract, not a silent success. |
| Presigned URL (SigV4 query) | Verified on use like a header signature; expiry capped by server policy. | Not range-bound, so it needs §5.9 wording (see ADR list). |
| Content addressing | The Xet file hash (Merkle over BLAKE3 chunk hashes) is the content address; the same bytes from `hf`, `git-xet` or S3 get the same file and dedup against each other. | Expose it as a response header (for example `x-openxet-hash`) so tools can switch to it. |

**ETag design (Verified against the client table).** Compute the MD5 while
chunking a single PUT and use it as the ETag; for multipart, S3's
`md5(concat(part_md5s))-N`. For a file uploaded through a Xet client and
read through S3, no MD5 exists; return the 64-hex Xet file hash as the
ETag. rclone ignores it (not 32 hex), `s3fs` parses it as hex, DuckDB
accepts it as strong, and no client in the table compares it with an MD5.
MD5 costs about 1.6 CPU-seconds per GB (Measured, see "Experiment"), about
a third of chunking plus compression.

**Credentials (Estimated design).** SigV4 is symmetric: the server needs
the secret to verify a signature. OpenXet stores only token hashes (§5.4).
Instead of storing S3 secrets, derive them:
`secret = HMAC(master_key_v, access_key_id)`. The access key ID names the
OpenXet token (and so its external identity, scopes and expiry) and the
master-key version. Nodes then stay stateless, revocation works through the
token, and keys rotate by version. `s3s`'s `S3Auth::get_secret_key`
(`crates/s3s/src/auth/mod.rs:185`) fits this. huggingface.co does the same
in spirit: S3 keys are generated from a user access token and "inherit the
permissions of the underlying access token" (Verified, same page).

### Prior art and licences

| Project | Licence (Verified) | Language | Status (2026-10-24) | Use for OpenXet |
|---|---|---|---|---|
| huggingface.co `s3.hf.co` | proprietary | n/a | in production | The product shape to match: keys from Hub tokens, path-style URLs with the namespace in the endpoint, `302` to a CDN for reads, ListObjectsV2 only, no user metadata, 7-day multipart expiry ([docs](https://huggingface.co/docs/hub/storage-buckets-s3)). |
| `s3s` | Apache-2.0 | Rust | 0.17.0, about 615k downloads in 90 days on crates.io; used by RustFS | **Reuse** for routing, XML, SigV4 (header and presigned), `aws-chunked` and checksum trailers (`crates/s3s/src/stream/aws_chunked_stream.rs`). It calls itself experimental and leaves authentication and limits to the embedding service ([README](https://github.com/s3s-project/s3s)). It is a hyper service; axum can nest it. |
| MinIO | AGPL-3.0 | Go | GitHub repository archived (last push 2026-04-24) | Neither licence nor status fits. Its users need a new home, which matters for design §1's "teams who keep large files in MinIO". |
| Garage | AGPL-3.0 | Rust | active | Not linkable into OpenXet. It already works as a storage backend (§5.2), and its [compatibility table](https://garagehq.deuxfleurs.fr/documentation/reference-manual/s3-compatibility/) is a useful checklist. |
| SeaweedFS | Apache-2.0 | Go | active | Reference only; different language and storage model. |
| JuiceFS gateway | Apache-2.0 | Go | active | Runs on MinIO's gateway code; returns no ETag unless `--keep-etag` ([docs](https://juicefs.com/docs/community/guide/gateway/)). Some S3 users work without ETags, but DuckDB and `s3fs` lose conditional reads. |
| Ceph RGW | LGPL-2.1 or LGPL-3 ([COPYING](https://github.com/ceph/ceph/blob/main/COPYING)) | C++ | active | Semantics reference. `ceph/s3-tests` (MIT, archived on GitHub) is the usual S3 conformance suite and could seed a compat leg. |
| versitygw | Apache-2.0 | Go | active | Closest architecture (an S3 gateway over a pluggable backend); reference only. |
| Cloudflare R2 | proprietary | n/a | in production | Its [S3 API page](https://developers.cloudflare.com/r2/api/s3/api/) lists a similar implemented subset; multipart ETags mimic S3's since 2023-06-21 ([multipart](https://developers.cloudflare.com/r2/objects/multipart-objects/)). |

**Build or reuse: reuse `s3s` for the protocol layer and build the
semantics.** The hard parts are ours anyway: keys over Xet files,
multipart onto stable boundaries, ETags, listing on the index,
authorization through the authorizer, metering and GC. Nothing AGPL
enters the binary. `s3s`'s request limits and pagination have to be set
by us; its README says so.

## 2. Dedup economics

### Published numbers

| Source | Data | Result | Label |
|---|---|---|---|
| HF, [From Chunks to Blocks](https://huggingface.co/blog/from-chunks-to-blocks) (2025-02-12) | 29 GGUF quantizations of `google/gemma-2-9b-it`, 191 GB | about 97 GB stored, 49% saved | Verified (published measurement) |
| HF, [Parquet CDC](https://huggingface.co/blog/parquet-cdc) (2025-07-25) | OpenOrca, 100k rows, about 96 MB; Snappy | Two versions stored at "dedup ratio" (stored ÷ both) of 52% for a 10k-row append without CDC; 52% for 3k inserted rows **with** CDC against 95% without; 55% against 92% for 4k deleted rows. 50% means the second version was free. | Verified (published measurement) |
| HF, [storage page](https://huggingface.co/storage) | a raw plus processed dataset | "66 TB stored → billed for 41 TB" (38% saved), as an Enterprise example | Verified (vendor example) |
| [ZipLLM](https://arxiv.org/abs/2505.06252) (v3, 2025-11-08) | full scan of Hugging Face: 5.69 M files, 11.89 PB | 0.97 PB (8.2%) are byte-identical duplicate files | Verified (published measurement) |
| ZipLLM, same | 3,048 LLM repos, 43.19 TB | file-level dedup 3.2%, FastCDC 14.8%, tensor-level 8.3% | Verified (published measurement) |
| Meyer and Bolosky, [FAST '11](https://www.usenix.org/blog/study-practical-deduplication) | desktop file systems at Microsoft | whole-file dedup gets about 3/4 of block-level savings on live file systems, 87% on full backups | Verified (abstract) |
| Wallace et al., [FAST '12](https://www.usenix.org/conference/fast12/characteristics-backup-workloads-production-systems) | about 10,000 EMC backup systems | "average of 10x deduplication" for repeated full backups | Verified (poster) |

### Experiment

Method (Measured, 2026-10-24, Apple M3, macOS, shared host at load
average 50 to 60):
- A scratch Rust program (not committed) chunks each file with
  `xet_data::deduplication::Chunker::default()` from `xet-data` 1.7.0, the
  chunker stock `hf_xet` uses, and records each chunk's hash, length, and
  compressed length (the smaller of `LZ4` and `ByteGrouping4LZ4` from
  `xet-core-structures`' `CompressionScheme`).
- Check first: its chunk list for the reference file
  `Electric_Vehicle_Population_Data_20250917.csv` is byte-identical to the
  golden `….csv.chunks` (796 chunks), so the boundaries are exactly
  `xet-core`'s.
- For each pair (old, new), "new bytes" are the bytes of the new file's
  chunks that don't occur in the old one: what a second upload adds.
  "Saving" is 1 − (unique chunk bytes of both) ÷ (bytes of both).
- This is chunk-level dedup, an upper bound. Real dedup is slightly lower
  because stock clients skip very short matches
  (`deduplication/defrag_prevention.rs`, design §1a), and server-side
  dedup finds matches through the eligible-chunk index.
- Inputs, all public and fetched without credentials:
  - `EleutherAI/pythia-70m` `pytorch_model.bin` at revisions
    `step142000` and `step143000` (sha256 `eba3cc4d…` and `f629437d…`,
    matching the Hub's LFS OIDs): two consecutive training checkpoints.
  - CPython 3.13.0 and 3.13.1 source releases from python.org (`.tar.xz`
    sha256 `086de588…` and `9cf9427b…`), plus their `.tar`, and `.tar.gz`
    made with `gzip -n -6`: two snapshots of a source tree, as a backup
    would see them.
  - The reference CSV written as Parquet with pyarrow 26.0.0 (Snappy,
    default row groups, so one row group): v1 is its first 90% of rows;
    "append" is all rows; "delete" removes 2% of rows from the middle of
    v1; "insert" adds 2% of rows there. Each without and with
    `use_content_defined_chunking`.
  - The reference CSV as a log: its first 90% of lines, then all lines.

Results (Measured):

| Pair (old → new) | Bytes, both | New bytes in `new` | Saving, dedup only | Saving, dedup + compression |
|---|---:|---:|---:|---:|
| Pythia `step142000` → `step143000` | 332,098,198 | 166,049,099 (100.0%) | 0.0% | 15.1% |
| CPython 3.13.0 → 3.13.1, `.tar` | 218,675,200 | 84,335,451 (77.0%) | 11.6% | 65.6% |
| same, `.tar.gz` | 58,921,779 | 29,508,458 (100.0%) | 0.0% | 0.1% |
| same, `.tar.xz` | 45,122,672 | 22,534,213 (99.8%) | 0.1% | 0.1% |
| Parquet append, no CDC | 9,935,177 | 2,722,672 (52.1%) | 25.2% | 26.2% |
| Parquet append, CDC | 9,938,883 | 2,389,241 (45.7%) | 28.5% | 29.6% |
| Parquet delete 2%, no CDC | 9,327,203 | 3,587,733 (77.8%) | 11.0% | 12.2% |
| Parquet delete 2%, CDC | 9,329,215 | 2,912,004 (63.1%) | 18.3% | 19.5% |
| Parquet insert 2%, no CDC | 9,528,423 | 3,920,025 (81.4%) | 9.4% | 10.6% |
| Parquet insert 2%, CDC | 9,527,243 | 3,492,947 (72.6%) | 13.9% | 15.1% |
| CSV log, append 10% | 120,643,711 | 6,473,518 (10.2%) | 47.3% | 87.8% |

What it shows:
- **Dense checkpoints don't dedup.** Every weight changes between
  training steps, so no 64 KiB chunk repeats. Only compression helps (15%
  on fp16, from byte grouping). Dedup on model data comes from
  quantization families, partially frozen fine-tunes and re-uploads of the
  same base model: HF's Gemma 49%, and ZipLLM's 8.2% of duplicate files.
- **Compressed archives and encrypted backups don't dedup.** A
  `.tar.gz` or `.tar.xz` of nearly the same tree shares no chunk. restic,
  Borg and similar tools deduplicate and encrypt on the client, so their
  repositories look like random data to Xet.
- **Uncompressed snapshots and append-only logs dedup well.** The appended
  log's second version added 10.2% new bytes. A point release of CPython
  (many changed files) still shared 23% of its tarball.
- **Small Parquet files dedup less than HF's blog shows.** Our files are
  about 5 MB in one row group, close to the 64 KiB chunk and 1 MiB page
  scale, and dictionary pages change with the data. CDC still helped in
  every case (append 45.7% new against 52.1%; delete 63.1% against 77.8%).
  HF's 96 MB files reached near-ideal results. We didn't rerun at that
  size.
- **Compression often matters more than dedup.** On text it is the
  largest saving (CSV: 47% from dedup, 88% with compression). Xorbs are
  compressed per chunk anyway, so S3 tenants get it for free.

Ingest CPU (Measured, user CPU time, so less sensitive to the host's load
than wall-clock time): chunking, BLAKE3 hashing and **both** compressions
of 275.6 MB took 0.78 CPU-seconds, about 2.8 CPU-seconds per GB; MD5 took
0.43 CPU-seconds, 1.6 per GB. Wall-clock throughput on this host was not
meaningful. At Hetzner CCX33 prices (€138.99 per month for 8 dedicated
vCPUs) that is about $0.03 of CPU per TB ingested (Estimated): negligible
next to storage and bandwidth.

### Prices

List prices, read 2026-10-24, excluding tax. EUR converted at the ECB
reference rate of 2026-10-23, 1 EUR = 1.1193 USD
([ECB](https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml)).

| Offer | Storage per TB-month | Egress to the internet | Requests | Minimums | Source |
|---|---|---|---|---|---|
| AWS S3 Standard, us-east-1 | $23 (first 50 TB), $22, $21 (over 500 TB) | $90/TB for the first 10 TB, down to $50/TB over 150 TB; 100 GB/month free | PUT/COPY/POST/LIST $5 per million; GET $0.40 per million | none | AWS price list files, [S3](https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonS3/current/us-east-1/index.json) (published 2026-10-15), [data transfer](https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AWSDataTransfer/current/us-east-1/index.json) (2026-09-16) |
| Cloudflare R2 Standard | $15 (Infrequent Access $10, $0.01/GB retrieval) | free | Class A $4.50, Class B $0.36 per million | IA: 30 days | [R2 pricing](https://developers.cloudflare.com/r2/pricing/) |
| Backblaze B2 | $6.95 | free up to 3× stored, then $0.01/GB | Classes A to C free | none; first 10 GB free | [B2 pricing](https://www.backblaze.com/cloud-storage/pricing) |
| Wasabi (pay as you go) | $7.99 | free while egress ≤ active storage | free | 1 TB per account; 90 days per object | [pricing](https://wasabi.com/pricing), [FAQ](https://wasabi.com/pricing/faq) |
| Hetzner Object Storage | €6.49 base (includes 1 TB stored and 1 TB egress), then €0.0087/TB-hour ≈ €6.47 ≈ $7.24 | €1.00 ≈ $1.12 per TB | free | 64 KB billable object size | [Hetzner](https://www.hetzner.com/storage/object-storage/) |
| Hugging Face buckets, private | $18; $16 over 50 TB; $14 over 200 TB; $12 over 500 TB | included up to 8:1 of storage | n/a | billed on deduplicated bytes only on Enterprise plans | [hf.co/storage](https://huggingface.co/storage), [storage limits](https://huggingface.co/docs/hub/storage-limits) |
| Hugging Face buckets, public | $12; down to $8 over 500 TB | same | n/a | same | same |

Compute for gateway nodes: Hetzner CCX33, 8 dedicated vCPUs, €138.99 ≈
$155.57 per month with 30 TB of traffic included, €1.00 per further TB in
EU locations ([Hetzner](https://www.hetzner.com/cloud/general-purpose/)).

### Cost model

Billing unit: a **billed TB** is a TB of the tenant's own deduplicated,
uncompressed bytes (see "Fair billing"). Our cost per billed TB-month is:

```
storage = P × c × (1 + o) × (1 + garbage) × (1 − g)
serving = E × (node_cost / TB_served_per_node + traffic_beyond_included)
cost    = storage + requests + serving + database
```

- `P`: backend price per physical TB-month (table above).
- `c`: compression factor of unique chunks: 1.00 for already-compressed
  objects, 0.85 for fp16/bf16 weights, 0.30 for text, CSV and uncompressed
  tar (Measured above: 0.85 Pythia, 0.23 CSV, 0.39 CPython tar; rounded).
- `g`: cross-tenant dedup, which only we see: 0.02, 0.08 and 0.05
  (Estimated; 0.08 from ZipLLM's 8.2% duplicate files on a public hub).
- `o` = 0.05: design §1a's ceiling on storage overhead.
- `garbage` = 0.10: data kept through the GC grace period as a share of
  live data, for a tenant with modest churn (Estimated; see "Risks" for
  high churn).
- `E`: bytes read per month ÷ bytes stored. One node serves 100 TB a month
  (about 0.3 Gbit/s on average, Estimated), so serving costs $2.34 per TB
  read. Requests: $0.40 per TB-month for R2 (Estimated from its Class A/B
  prices at 64 MiB xorbs and 1 MiB reads); free on the others.
- `database`: $0.50 per TB-month (Estimated: a primary and a replica on
  dedicated servers, holding the index for 1 PB, design §5.8).

Cost per billed TB-month at about 1 PB (Estimated, computed from the
formula above):

| Backend, E = 1 | Already-compressed | fp16/bf16 weights | Text, CSV, tar |
|---|---:|---:|---:|
| Hetzner Object Storage | $11.04 | $9.38 | $5.22 |
| Backblaze B2 | $10.71 | $9.12 | $5.13 |
| Cloudflare R2 Standard | $20.22 | $16.79 | $8.18 |
| AWS S3 Standard, nodes in AWS (egress $90/TB) | $116.93 | $111.67 | $98.47 |

| Backend, E = 3 | Already-compressed | fp16/bf16 weights | Text, CSV, tar |
|---|---:|---:|---:|
| Hetzner Object Storage | $15.71 | $14.06 | $9.90 |
| Backblaze B2 | $15.38 | $13.79 | $9.81 |
| Cloudflare R2 Standard | $24.90 | $21.47 | $12.86 |

What drives it:
- **The backend price is the floor.** Dedup and compression multiply it;
  they can't take it below the backend's own list price unless `c × (1 −
  g)` is small. A hosted service on R2 or S3 can't undercut R2 or S3 on
  data that doesn't dedup.
- **Serving is the second cost.** S3 clients can't fetch xorbs, so every
  byte read crosses our nodes. With cheap-traffic nodes it is $2.34 per TB
  read; with AWS egress it is $90. This is why §15 asks for cheap-egress
  storage, and why R2's free egress doesn't help us: the bytes leave
  through our nodes, not R2's.
- **Upload bandwidth isn't saved.** An S3 PUT sends the whole object;
  dedup saves only storage. Transfer savings need `hf`, `git-xet` or our
  client.

### What we could charge

- **Flat price per logical GB.** It must cover a tenant that doesn't
  dedup: about $11 per TB-month on Hetzner or B2 at E = 1. With a margin,
  $0.012 to 0.015 per logical GB, which is R2's price: it beats S3 and
  Hugging Face private storage, but not B2, Wasabi or Hetzner. A flat
  price also attracts tenants with the least dedup.
- **Price per deduplicated GB (recommended).** $12 to 15 per billed
  TB-month, egress up to 1× stored included, $0.005 to 0.01 per GB beyond.
  Gross margin at $15: about 26% for already-compressed data, 37% for
  weights, 65% for text (Estimated, E = 1, Hetzner). What a tenant pays
  per logical GB:

| Tenant's own dedup | Pays per logical GB at $15 | at $12 | Cheaper than |
|---|---:|---:|---|
| 0% (archives, media, encrypted backups) | $0.015 | $0.012 | S3, HF private; equal to R2 |
| 25% (some versions kept, fine-tune family) | $0.011 | $0.009 | also HF public at its base tier ($12) |
| 50% (quant family, appended datasets, snapshots) | $0.0075 | $0.006 | also Wasabi; about B2 |
| 80% (10 retained versions, 10% change each) | $0.003 | $0.0024 | everything listed |

- **The band: $12 to 15 per TB-month of tenant-deduplicated bytes**, with
  volume tiers inside it, as Hugging Face does. Lower than $11 loses money
  on tenants that don't dedup, unless the backend is cheaper than list
  price (committed-use contracts, or an operator's own disks under Garage
  or Ceph).

For self-hosters there is no margin question: they pay their backend's
price × `c` × (1 − tenant dedup), and gain an S3 endpoint over storage
they already own.

## 3. Multi-tenancy and security

### Where the existence oracle goes

Xet's dedup query (`GET /v1/chunks/…`) answers whether a chunk exists;
design §5.4 keys answers with HMAC and offers `per-scope` visibility for
multi-tenant hosting. An S3 client never calls it: the server chunks and
dedups for it. Two channels remain, and both are OpenXet's to design:
- **Write timing.** A PUT of bytes already stored skips the xorb write.
  For a 64 MB xorb the difference is visible. It is visible under
  `per-scope` too, because §5.4 keeps storage shared ("the upload of a
  xorb already stored writes nothing new").
- **Billing and quotas.** If a tenant's bill or quota drops when another
  tenant holds the same chunks, the bill is an oracle.

### Per-tenant versus global dedup

| Mode | Storage saved across tenants | What a tenant can learn | Cost |
|---|---|---|---|
| Shared storage, stand-alone billing (recommended default) | all of `g` | timing of large PUTs only | none |
| Shared storage, sharing credited in bills | all of `g` | existence, through the bill | rejected |
| Pool per tenant (§10.7) for tenants that need it | none | nothing across tenants | `g` of their data: about 8% on hub-like public data (ZipLLM's duplicate files), Estimated 0 to 5% on private working data, which mostly dedups against its own versions |

`g` is the only thing per-tenant separation costs, because intra-tenant
dedup (versions, checkpoints, snapshots) stays. On the experiment's
workloads that's where nearly all the savings came from.

### Quotas, accounting and fair billing

- **Logical bytes** (sum of object sizes) and **billed bytes** (the
  tenant's own unique chunk bytes, uncompressed) per tenant, matching
  §5.7's logical and physical metering, plus **physical bytes** for the
  operator.
- **Billed bytes are exact without a per-chunk index.** GC's mark phase
  already walks files → terms → xorb ranges. Per tenant, the union of
  `(xorb, chunk range)` intervals over its files, summed in raw chunk
  lengths, is exactly its stand-alone deduplicated size. Run it daily
  with the mark phase. That fits §1a's "per-tenant physical bytes are
  exact for … `per-scope` tenants".
- **Fair billing is stand-alone billing.** Each tenant pays for its data
  as if it were the only tenant. The sum of bills exceeds physical
  storage; the difference (cross-tenant dedup and compression) is margin,
  and no bill depends on another tenant's data. Splitting shared chunks'
  cost between tenants would be "fairer" in the accounting sense and would
  turn the bill into an existence oracle.
- **Quotas** on logical or billed bytes, both stand-alone, so they leak
  nothing either. Hot-path enforcement uses logical bytes (known at PUT
  time); billed bytes lag by a day.

## 4. Recommendation

### Scope and milestone

In scope, in **M5**, delivered with buckets:
- It depends on M3 (Postgres, S3/GCS/Azure backends), M4 (server-side
  chunking, metering, GC) and the bucket primitive itself.
- huggingface.co's buckets ship an S3 API, so an OpenXet bucket without
  one is a compatibility gap for users moving from the Hub. §1's MinIO
  users are a second audience, now that MinIO's repository is archived.
- §12 defines M0 to M7 as 1.0. "M8" would be the first post-1.0 milestone,
  which the scope rule ("nothing deferred for convenience") argues
  against.

### Minimal v1 subset

- **Auth:** SigV4 headers, including `UNSIGNED-PAYLOAD`, signed
  `aws-chunked` and `STREAMING-UNSIGNED-PAYLOAD-TRAILER` with
  `x-amz-checksum-*` trailers; SigV4 presigned GET, PUT and HEAD. Path-style
  and virtual-host-style addressing (DuckDB defaults to the latter).
  Region `us-east-1` accepted.
- **Buckets:** ListBuckets, CreateBucket, HeadBucket, DeleteBucket (empty
  only), GetBucketLocation.
- **Objects:** PutObject (with `If-None-Match: *` and `If-Match`),
  GetObject (single range; `If-Match`, `If-None-Match`,
  `If-Modified-Since`, `If-Unmodified-Since`), HeadObject, DeleteObject,
  DeleteObjects, CopyObject, ListObjectsV2 and ListObjects V1 (prefix, `/`
  delimiter, paging), user metadata and standard content headers.
- **Multipart:** Create, UploadPart, Complete, Abort, ListParts,
  ListMultipartUploads, UploadPartCopy. Incomplete uploads expire after a
  configurable period.
- **Integrity:** verify `Content-MD5` and every `x-amz-checksum-*` on
  upload; store them and return them with `ChecksumMode`.
- **Out of v1:** ACLs and bucket policies (the OpenXet authorizer
  decides), object versioning (§10.4 makes it optional; map later),
  lifecycle, tagging, SSE-C, Object Lock, Select, website hosting,
  notifications.
- **Compat leg:** AWS CLI and `boto3` with default settings, rclone, s5cmd,
  restic, `s3fs`, DuckDB, polars. Add a subset of `ceph/s3-tests`, and a
  round trip: the same bytes via S3 and via `hf` give the same Xet file
  hash.

### Risks

- **GC grace under churn.** §5.6 sets a 28-day minimum, because stock Xet
  clients cache shards for 21 days and dedup keys last 7. A bucket that
  keeps the last three hourly 10 GB dense checkpoints has 30 GB live but
  writes 240 GB of new chunks a day, so about 7.2 TB stays stored through
  the default 30-day grace (6.7 TB at the 28-day minimum): 240× its billed
  size (Estimated). Xorbs written by
  server-side chunking and never offered in a dedup answer have no client
  holding a cached shard. Their grace could be the transfer-URL lifetime
  (≤ 60 min, M1-R42) plus margin. Until that is decided, a minimum storage
  duration in billing (as Wasabi and R2 IA have) covers the cost.
- **Small objects.** Each PUT costs a xorb write, a shard write and an
  index transaction, against one backend PUT for plain S3. On R2 that is
  2 to 3× the Class A cost, and Hetzner bills 64 KB per tiny object. Writes
  must be durable before the 200, so small PUTs can't wait to be batched.
  Needs a benchmark target and probably a packing pass (an ADR, since it
  rewrites xorbs).
- **Read latency.** The first byte needs an index lookup and a backend
  range read. Parquet readers issue many small ranges, each decompressing
  up to a 128 KiB chunk. The cache tier (§10.5) and the reconstruction
  latency work apply; set a p99 target per range GET.
- **List consistency.** Strong list-after-write needs reads from the index
  primary or read-your-writes fencing. With replicas (§5.8), listings must
  not go to them blindly.
- **ETag semantics.** MD5 for S3-ingested objects, the Xet hash otherwise;
  multipart ETags depend on part sizes as on S3, so the same bytes can
  have two ETags. Tools that compare ETags across stores (rclone `check`)
  fall back to sizes or `X-Amz-Meta-Md5chksum`.
- **Bucket as the source of truth.** Key → file mappings must reach the
  bucket like refs do, with at most §1a's 60 s staleness on `reindex`.
  For a write-heavy bucket that is an extra ordered log in the bucket; its
  cost needs measuring.
- **Expectations.** S3 users don't get faster uploads. The docs must say
  that transfer dedup needs a Xet client.

### ADRs needed

1. **S3 API surface:** the subset above, `s3s` as the protocol layer,
   addressing, and how S3 operations map onto bucket semantics (overwrite,
   delete, copy, conditional writes, list consistency).
2. **S3 credentials:** access keys derived from OpenXet tokens with a
   versioned master key; presigned URLs as delegated credentials, with
   capped expiry and revocation through the token, and the wording §5.9
   needs, since they are signed but not range-bound.
3. **ETags and checksums:** MD5 or Xet hash, multipart ETags, which
   checksums are verified and stored.
4. **Stand-alone billing and short GC grace:** billed bytes as the
   tenant's own deduplicated bytes, computed in the mark phase; and a
   shorter grace for xorbs never offered to a Xet client, which refines
   §5.6 and ruling M1-R5. Superseding the bucket-as-repo-type wording of
   §10.4 with the two-primitive model belongs here or in its own ADR.

## Not verified

- MLflow, PyTorch S3 loaders and DVC were not read in source; DVC works
  against huggingface.co's gateway per its docs.
- No real S3 client was run against a prototype; the client table is from
  source reading.
- HF's reported dedup for the Llama 4 family (about 25%) appears in Hub
  community posts; we did not find the primary source.
- Parquet results at HF's file sizes (about 100 MB and more) were not
  rerun; our files were about 5 MB.
- Node throughput (100 TB served per node-month), database cost and `g`
  are estimates; the release rig should measure the first two.
- Whether Wasabi's and Hetzner's prices include tax or regional variation
  beyond what their pages state.
