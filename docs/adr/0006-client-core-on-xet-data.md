# 0006: Build the client core on xet-data, and only in the client crate

- Status: accepted
- Date: 2026-10-23
- Scope: `client`, `protocol`, `cas`

## Context

Design §10.8 requires the client to be byte-identical with stock clients,
so it builds on `xet-core` rather than reimplementing chunking, xorb and
shard building. Spike S10 compiled `xet-data`, `xet-runtime` and
`xet-client` `=1.7.0` and round-tripped a 5 MB file through a logging
proxy (`docs/research/S10-cas-wire-contract.md`, "Our client core").

`xet-data` brings the async runtime, the HTTP client and the session
layers. ADR 0001 keeps the server's use of upstream behind
`openxet-protocol`, which depends only on `xet-core-structures`.

## Decision

1. `crates/client` (`openxet-client`) is the only crate that may depend on
   `xet-data`, `xet-runtime` and `xet-client`.
2. They are pinned `=1.7.0`, in lockstep with `xet-core-structures`. A bump
   moves all four together, with the compatibility suite run.
3. The server crates never depend on them. A `cargo deny` ban, added
   with the crate, rejects them anywhere but `openxet-client` and the
   binaries that wrap it.
4. Using `xet-client`'s server-side helpers (for example
   `build_file_chunk_hashes_response`) in a server crate needs a
   superseding ADR.

## Consequences

- The client inherits upstream's chunking and upload behaviour, so
  divergence from stock clients can only come from configuration.
  Compatibility-suite legs run the client against openxet and
  `hf_xet` against openxet.
- Server builds stay free of the HTTP client stack and the `xet-runtime`
  thread pool.
- `GET /v2/file-chunk-hashes/{id}` stays `404` until M6 (design §12).
  Building it with upstream's helper would need item 4's ADR.
- The ban in item 3 fails CI when a server crate depends on them.

## Alternatives considered

- **Put the client in `openxet-protocol`.** Rejected: it would pull the
  HTTP client and runtime into every server build.
- **Reimplement upload and download on `xet-core-structures`.** Rejected:
  it duplicates upstream's session logic, retries and caches, and risks
  silent incompatibility (ADR 0001).
