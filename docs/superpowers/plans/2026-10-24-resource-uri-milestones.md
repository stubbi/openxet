# xet resource URI delivery milestones

<!-- description: Scheduled M2 specification and M6 client delivery for xet:// resource addresses, with dependencies, compatibility checks and release criteria. -->

Date: 2026-10-24. Status: **scheduled scope; no implementation completed**.
[ADR 0009](../../adr/0009-xet-resource-uris.md) accepts the name and endpoint
boundary. [Product design §12](../specs/2026-10-23-openxet-design.md#12-milestones-v1--all-of-them)
is the authoritative milestone schedule. This handoff records the work that
the future M2 and M6 implementation plans must include; it is not a complete
implementation plan for either milestone. No separate M2/M6 plan exists yet.

## M2-URI: define resource addressing after Hub naming

**Dependency:** the M2 Hub work defines namespaces, repository kinds, paths,
revisions, authorization and HTTPS resolution. Establish those contracts before
finalizing URI syntax. The fixed choice is `xet://` with the real host and
optional port in the authority; hosted and self-hosted use identical semantics.

Deliver a reviewed URI specification, the legacy compatibility decision, and
one shared Rust parser/canonical formatter for later client bindings. Write
the implementation plan with dependency order and file ownership before coding.
Do not make the parser depend on a running hosted service.

Acceptance checks:

- The specification defines resource kinds, mutable and immutable references,
  escaping, Unicode, ports/IPv6, HTTPS mapping, reverse-proxy prefixes and local
  development behavior. An exported address never resolves through a recipient's
  default server or a local alias. The name is settled; the remaining grammar is
  reviewed here rather than inferred from an illustrative example.
- Audit pinned legacy Git-Xet and pyxet parsers and fsspec registration. Publish
  a matrix of accepted, translated and rejected forms, with actionable errors.
  Test ambiguous legacy inputs explicitly; never silently send them to a
  different deployment. Record coordination and IANA registration work without
  implying that either has already happened.
- Golden vectors and parser/formatter round-trip properties cover the approved
  grammar and malformed inputs; fuzz all untrusted parsing. Include Unicode,
  percent-encoding, separator/ref ambiguity and endpoint normalization cases.
- Two distinct endpoint fixtures contain the same resource names but different
  content. Resolution selects the URI's endpoint and preserves resource scope.
  Credential-isolation tests reject credentials embedded in URIs, prevent use of
  the default endpoint's token on another host and prevent redirect leakage.
- Preserve the existing HTTPS/stock-client contracts and storage-only hash-based
  CLI behavior. Unsupported resource kinds fail clearly; this task does not
  bring M5 bucket/space/kernel support forward into M2.

M2 completion supplies the reviewed contract and parser, not a claim that all
third-party tools accept `xet://` or that public hosted accounts exist.

## M6-URI: deliver the same address through clients and UI

**Dependencies:** M2-URI, the M6 client command/API shapes and the resource kinds
implemented by the preceding milestones. Reuse the shared parser in the CLI,
Rust SDK and Python bindings. Provide an explicit fsspec integration with a
tested coexistence policy for legacy handlers; do not silently replace them.

Acceptance checks:

- CLI and SDK upload/download round trips use `xet://` against two independently
  addressed deployments of the released server. Identical names cannot cross
  endpoints. Test the same paths against the hosted staging deployment before
  enabling hosted URI copy actions; self-hosting gets no reduced capability.
- Exercise the real Python/fsspec adapter and a supported consuming library,
  with pinned versions. List native DuckDB/polars and stock Hugging Face support
  separately. Run unchanged stock clients against the existing HTTP/Hub path;
  never claim that adding our parser changes their recognized schemes.
- `sync`, `mount` and other resource-taking commands use the shared contract as
  they ship; write actions require the command's normal authorization. Merely
  opening a URI cannot upload, delete or execute anything.
- Publish runnable CLI/SDK recipes, versioned JSON/error examples, retry and
  migration guidance, and the same canonical Markdown for agents. Errors expose
  the endpoint/resource safely without printing credentials.
- UI copy actions distinguish **Copy resource URI** from **Copy web link**.
  HTTPS remains the canonical browser/share link. Both actions exclude secrets,
  work by keyboard and provide an accessible result/fallback. Show the resolved
  endpoint and revision when a human approves an agent's separate write action.

OS/browser protocol-handler registration is a separate feature; it is not
implied by CLI support and is not required to finish this work package.

## M7: publish the tested contract

Ship the URI specification, support matrix and migration notes with the client
distribution. Verify packaged CLI/SDK versions execute the published recipes.
Record registration status accurately; a chosen name is not an IANA registration
or a claim to own the Xet wire protocol. Until these paths ship, marketing and
quickstarts retain released commands and HTTPS links.
