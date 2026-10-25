# Resource URI schemes for OpenXet

<!-- description: Evidence behind the chosen future xet:// resource scheme, including HF URIs, legacy maintenance status, endpoint identity and remaining compatibility work. -->

Date: 2026-10-24. **Name decided; syntax and implementation remain pending.**
[ADR 0009](../adr/0009-xet-resource-uris.md) records the founder's choice of
`xet://`. This note does not change the Xet wire protocol or promise availability.
Sources were inspected on this date; legacy source revisions are pinned below.

## Decision and research recommendation

Use **`xet://`** as the future resource scheme, with the actual deployment host
explicit in every exported address and equal semantics for hosted/self-hosted
installations. Keep HTTPS as the portable, canonical browser/share address.
The founder chose the name after considering this note; the initial recommendation
to evaluate `openxet://` is superseded. `ox://` is not a second planned scheme.
Specify and test compatibility rather than treating the name as a working parser.
The work is scheduled as M2-URI and M6-URI in [product design §12](../superpowers/specs/2026-10-23-openxet-design.md#12-milestones-v1--all-of-them).

A resource scheme can give OpenXet a consistent addressing experience across
its tools. It does not establish ownership of the underlying storage protocol:
[RFC 7595 §3.8](https://www.rfc-editor.org/rfc/rfc7595.html#section-3.8)
distinguishes a scheme name from a protocol. OpenXet's compatibility commitment
remains the published [Xet specification](https://huggingface.co/docs/xet/index),
whose inspected version is 1.1.0.

## What exists

| Surface | Verified behavior | Implication |
|---|---|---|
| Current HF URI reference | `hf://[<TYPE>/]<namespace>/<name>[@<revision>][/<path>]`; buckets use `hf://buckets/...` and have no revision. The URI carries no endpoint. | Resource identity needs endpoint context when used with another server. |
| HF storage buckets | Official CLI and Python docs expose bucket transfers through `hf://buckets/` paths. | This is a Hub-level object namespace, not the Xet CAS wire address. |
| Legacy Python Xet SDK | `pyxet` documents `xet://<endpoint>:<owner>/<repo>/<branch>/<path>` and registers the `xet` fsspec implementation. | A new `xet` handler would collide with an existing handler name. |
| Deprecated Git-Xet | Rust parser/tests accept `xet://xethub.com/user/repo/branch/path` and endpoint-relative forms. | There is more than one legacy grammar to consider. |

HF syntax and endpoint behavior come from the current
[HF URI reference](https://huggingface.co/docs/huggingface_hub/package_reference/hf_uris):
its `parse_hf_uri(endpoint=...)` recognizes custom HTTP endpoints, while
`to_url(endpoint=...)` supplies the endpoint when rendering a web URL.
Its current [filesystem guide](https://huggingface.co/docs/huggingface_hub/guides/hf_file_system)
and [bucket guide](https://huggingface.co/docs/hub/storage-buckets) describe their
respective clients. These documents are not an OpenXet compatibility test.

Legacy evidence: [pyxet README](https://github.com/xetdata/pyxet/tree/95196fa4a8ce328a37b53dac4f85f1adf5d8ba61),
[URL parser](https://github.com/xetdata/pyxet/blob/95196fa4a8ce328a37b53dac4f85f1adf5d8ba61/python/pyxet/pyxet/url_parsing.py),
[fsspec registration](https://github.com/xetdata/pyxet/blob/95196fa4a8ce328a37b53dac4f85f1adf5d8ba61/python/pyxet/pyxet/file_system.py#L722),
and [Git-Xet parser/tests](https://github.com/xetdata/xet-core/blob/e45aeb2f8c110438f6017be79c0b4c854bdbd2d0/rust/gitxetcore/src/git_integration/git_url.rs).
The latter belongs to the [deprecated XetHub project](https://github.com/xetdata/xet-core),
not evidence that today's Hugging Face Git-Xet accepts these resource URIs.

### Legacy maintenance status

Checked the official GitHub repository and latest-commit APIs on 2026-10-24:

| Repository | Latest `main` commit (UTC) | Repository `pushed_at` (UTC) | `archived` |
|---|---|---|---|
| [xetdata/xet-core](https://api.github.com/repos/xetdata/xet-core) | [e45aeb2, 2024-10-16 17:59:00](https://github.com/xetdata/xet-core/commit/e45aeb2f8c110438f6017be79c0b4c854bdbd2d0) | 2024-10-16 17:59:02 | `false` |
| [xetdata/pyxet](https://api.github.com/repos/xetdata/pyxet) | [95196fa, 2024-10-16 17:58:39](https://github.com/xetdata/pyxet/commit/95196fa4a8ce328a37b53dac4f85f1adf5d8ba61) | 2024-10-16 17:58:41 | `false` |

Both final commits add deprecation notices, and both current READMEs direct
readers to Hugging Face. This supports describing these projects as deprecated,
with no later push shown in the checked metadata. GitHub's `updated_at` is not
the last code-change date. Neither inactivity nor an archive flag would establish
that no installed clients or saved URIs remain; compatibility still needs tests.

The [IANA URI-scheme registry CSV](https://www.iana.org/assignments/uri-schemes/uri-schemes-1.csv)
had no exact entry for `hf`, `xet`, `ox` or `openxet` when checked (451 rows).
Absence does not establish that a name is unused or available without conflicts;
`xet` above is a concrete counterexample. The name is now chosen; specify its
semantics, evaluate legacy use and seek registration under
[RFC 7595 §§4–7](https://www.rfc-editor.org/rfc/rfc7595.html#section-4).

## Compatibility and endpoint rules to settle before implementation

The existing [S4 experiment](S4-analytics-clients.md) tested DuckDB 2.0.0,
polars 2.0.0 and `huggingface_hub` 2.2.0. Their native `hf://` handling did not
share one endpoint policy: HfFileSystem accepted a custom endpoint; DuckDB and
polars used Hugging Face's host. This result is version-specific. Renaming the
scheme cannot make an unmodified client recognize it; adapters and client tests
would be necessary. Do not replace working `hf://` instructions with new syntax.

[Product design §5.5](../superpowers/specs/2026-10-23-openxet-design.md#55-data-paths)
already specifies an HTTPS read gateway as the common path. That gateway and
Hub resource addressing remain planned beyond the released storage-only CAS;
the [current CLI reference](../reference/cli.md) is the runnable contract.

An illustrative shape, **not valid OpenXet syntax today**, is:

```text
xet://storage.example.org/datasets/team/corpus/data.parquet?revision=<commit>
```

ADR 0009 settles the name and explicit authority. M2's specification must resolve:

- **Server identity:** host and optional port identify the deployment. Define
  HTTPS mapping, reverse-proxy base paths and explicit local development rules.
  A copied address must not silently follow a recipient's default server.
  Local aliases may be conveniences; exported URIs must resolve independently.
- **Resource identity:** decide supported resource kinds, mutable paths versus
  immutable revisions, path escaping, Unicode, slash-containing refs and hash
  identifiers. Use one parser and canonical formatter across CLI, SDK and agents.
  Follow [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986.html); the example above
  does not settle the grammar or add bucket support to the product roadmap.
- **Credentials and trust:** keep tokens and passwords out of resource URIs;
  select credentials by exact endpoint and authorized resource scope. Never
  forward a default server's credential to a URI-selected host or redirect.
  Untrusted URIs must not give hosted services unrestricted network-fetch access.
- **Safe actions:** opening a URI may identify or read a resource; it must not
  upload, delete or run commands. Human and agent previews should expose the
  resolved server, resource and revision before a separately authorized write.
- **Adoption:** preserve HTTPS and existing client contracts; publish parser
  vectors, endpoint-isolation tests and documented client support. Evaluate
  fsspec integration separately from native DuckDB/polars support. Browser/OS
  deep-link registration is an additional feature, not implied by a CLI parser.

Until then, website examples should use released commands and ordinary HTTPS
links. Describe `xet://` as selected but not implemented; make no protocol-ownership claim.
