# S1/S2 exchange fixtures

These are real request/response exchanges captured on 2026-10-23 while
`huggingface_hub` 2.2.0 and `hf_xet` 1.7.0 downloaded the reference CSV
through `hf_hub_download`. They come from spike S1/S2; the method and
results are in `docs/research/S1-S2-direct-downloads.md`, and the decision
is ADR 0002.

Each `*.jsonl` file is one variant, one exchange per line, in time order:

```json
{"role": "hub" | "cas" | "object-store",
 "request":  {"method": "...", "url": "...", "headers": {...}},
 "response": {"status": 200, "headers": {...}, "json": {...}}}
```

Where the exchanges came from:
- The `hub` and `cas` exchanges are from the spike's stub server.
- The `object-store` exchanges are from MinIO's request trace.
  `response.json` is present only for JSON bodies. Hub and CAS URLs are
  `localhost:18080` and `localhost:18081`, and MinIO is `127.0.0.1:19000`.
  The `linux-*` file uses `host.docker.internal` for Hub and CAS and
  `127.0.0.1:9000` for MinIO.
- `huggingface-co-reference.json` is the same flow against huggingface.co,
  as a shape reference: resolve HEAD, read token and v2 reconstruction.
- `cloud-multirange.txt` has the exact `curl` commands, with status and
  header lines, for multi-range vs single-range GETs on public AWS S3, GCS
  and Azure objects. These are anonymous GETs, not presigned URLs and not
  `hf_xet`.

Redaction:
- Every secret value is replaced by the literal string `REDACTED`. That
  covers:
  - presigned query values: `X-Amz-Signature`, `X-Amz-Credential`,
    CloudFront's `Signature`, `Policy`, `Key-Pair-Id` and `xip`
  - `Authorization`
  - `X-Xet-Access-Token`
  - `accessToken`
- Parameter names are kept, so the URL's shape (for example
  `X-Amz-SignedHeaders=host;range`) stays visible.
- Volatile headers are dropped: dates, request IDs, session IDs, and
  MinIO's trace-only `X-Amz-Signature-Age`.
