# 0006: Build the client core on xet-data, and only in the client crate

- Status: accepted
- Date: 2026-10-23
- Amended 2026-10-24: corrected the server-build consequence (M1 Task 13)
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
3. The server crates never depend on them. A `cargo deny` ban (`deny.toml`
   `[bans]`, proven by `scripts/check-client-core-ban.sh`) rejects a direct
   dependency on `xet-data` or `xet-client` from any crate but
   `openxet-client`, and on `xet-runtime` from any crate but
   `openxet-client` and its upstream dependents `xet-client`, `xet-data`
   and `xet-core-structures`. The `openxet` binary reaches them only through
   `openxet-client`: it may not depend on `xet-data` directly.
4. Using `xet-client`'s server-side helpers (for example
   `build_file_chunk_hashes_response`) in a server crate needs a
   superseding ADR.

## Consequences

- The client inherits upstream's chunking and upload behaviour, so
  divergence from stock clients can only come from configuration.
  Compatibility-suite legs run the client against openxet and
  `hf_xet` against openxet.
- No server crate depends on `xet-data` or `xet-client`, or directly on
  `xet-runtime`. The server crates still compile `xet-runtime` (and with it
  `reqwest` and `tower-http` 0.6), because `xet-core-structures` depends on
  it. This was already so before M1 Task 13 (`cargo tree -p
  openxet-protocol -i xet-runtime`, checked at d68b595). No server code
  calls it, so its thread pool and HTTP client never start in `openxet
  serve`. The single `openxet` binary serves and transfers, so it links the
  client core through `openxet-client`.
- `GET /v2/file-chunk-hashes/{id}` stays `404` until M6 (design §12).
  Building it with upstream's helper would need item 4's ADR.
- The `cargo deny` rule of item 3 is added with the openxet-client crate
  (M1 Task 13).

## Alternatives considered

- **Put the client in `openxet-protocol`.** Rejected: it would make upload
  sessions, the HTTP client and the runtime's pool reachable from every
  server crate, not just compiled into it.
- **Reimplement upload and download on `xet-core-structures`.** Rejected:
  it duplicates upstream's session logic, retries and caches, and risks
  silent incompatibility (ADR 0001).
