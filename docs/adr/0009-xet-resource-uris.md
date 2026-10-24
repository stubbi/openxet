# 0009: Use xet for future resource URIs

<!-- description: Choose xet:// with an explicit deployment host for future openxet resource addresses; schedule its specification in M2 and client delivery in M6. -->

- Status: accepted (name, endpoint boundary and milestone assignment only)
- Date: 2026-10-24
- Scope: `client`, `hub`, `web`, `hosted`, `docs`

## Context

The founder explicitly chose **`xet://`** for openxet resource addresses and
asked that delivery be tracked in the milestones. This replaces the research
note's initial recommendation to evaluate `openxet://`; it does not change a
released parser or the Xet wire protocol.

[The URI research](../research/resource-uris-2026-10-24.md) records current HF
syntax, the IANA registry and legacy XetHub implementations. Both legacy
repositories label themselves deprecated. Their latest default-branch commits
and last pushes are dated 2024-10-16; GitHub reported neither as archived on
2026-10-24. That is evidence about maintenance, not evidence that no installed
clients or stored `xet://` addresses remain. Their grammars and fsspec handler
registration must be evaluated during implementation.

[Product design §10.8](../superpowers/specs/2026-10-23-openxet-design.md#108-our-own-client-decided-by-the-user-v1-100-compatible)
requires our client to coexist with stock tools. A name alone cannot make
those tools understand a new resource-address grammar.

## Decision

1. **Name.** `xet://` is the chosen future resource scheme. `ox://` and
   `openxet://` are not parallel schemes or planned aliases.
2. **Deployment identity.** The authority contains the actual deployment host
   and an optional port. Hosted and self-hosted installations use the same
   scheme and semantics. An exported resource URI must not depend on a local
   alias, silently select openxet.com, or follow the recipient's default server.
   Transport, base-path discovery and the full resource grammar remain to be
   specified; this decision does not invent a hosted API hostname.
3. **Portable links.** HTTPS remains the canonical browser/share address and
   the interoperability fallback. Existing stock-client and HTTP contracts
   remain supported. A future UI can offer separate copy actions for the web
   link and the resource URI once the corresponding client support ships.
4. **Authority and safety.** No password, access token or signed capability is
   embedded in a resource URI. Credentials are selected separately for the
   exact endpoint and resource scope; redirects cannot broaden that authority.
   Opening a resource address does not authorize a write, deletion or command.
5. **Compatibility.** The legacy audit must decide which historical forms are
   accepted, translated or rejected and test that policy. Neither deprecation
   nor a registry's lack of an entry proves that compatibility can be ignored.
   Do not silently overwrite another installed fsspec handler. Coordination
   and registration work belong to the specification task; this ADR makes no
   claim that registration or upstream agreement has occurred.
6. **Delivery.** Product design §12 assigns **M2-URI** to the specification,
   legacy audit and shared parser/formatter after Hub resource naming is
   defined; **M6-URI** delivers CLI/SDK/adapters and UI/docs integration using
   that contract. M7 publishes the verified support matrix and specification.
   [The milestone handoff](../superpowers/plans/2026-10-24-resource-uri-milestones.md)
   defines dependencies and acceptance checks.
7. **Current boundary.** The name and delivery obligations are accepted now.
   Full syntax, resolution semantics and implementation need their own reviewed
   specification and implementation plan, with the checks below, before code is
   built. No URI parser, SDK adapter, OS handler or runtime support is added by
   this decision. Public examples remain released commands and HTTPS links.

## Consequences

Resource addresses can express the same task on any openxet deployment without
a hosted-only spelling. The chosen name carries legacy compatibility work;
that work is a scheduled deliverable rather than an unscheduled naming question.

Acceptance requires parser/formatter golden vectors, round-trip properties and
fuzzing; two-endpoint credential-isolation tests; legacy-grammar decisions;
real CLI/SDK/adapter round trips; and accurate human/agent documentation.
Third-party clients are listed as supported only after a versioned test proves
the relevant path. A fsspec adapter is not native DuckDB or polars support.

This is an openxet resource-addressing decision, not ownership of Xet or a
change to its chunking, hashes, object formats or CAS API. Wire compatibility
remains non-negotiable.

## Alternatives considered

- `openxet://`: initially recommended in research for a distinct descriptive
  name; the founder chose `xet://` instead.
- `ox://`: shorter but not selected; maintaining another alias adds ambiguity.
- HTTPS alone: remains the portable link, but does not satisfy the selected
  client resource-address experience on its own.
- Endpoint-relative `xet://` addresses: rejected for exported addresses because
  the same text could target different deployments for different people or agents.
