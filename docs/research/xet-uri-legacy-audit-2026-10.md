# Legacy xet:// forms: audit of Git-Xet and pyxet

<!-- description: What XetHub's deprecated Git-Xet and pyxet accepted as xet:// addresses, read from pinned sources, their fsspec registration, the registry status, and the policy OpenXet's parser takes for each form. -->

Date: 2026-10-25. Input to the [`xet://` specification](../reference/xet-uri.md)
(M2-URI, [ADR 0009](../adr/0009-xet-resource-uris.md) item 5). The policy and
its matrix live in the specification; this note records the evidence.

## Sources

Read at the commits the [URI research note](resource-uris-2026-10-24.md) pinned,
the latest default-branch commits of both repositories:

| Project | Repository and commit | Version at that commit | Files read |
|---|---|---|---|
| Git-Xet | [`xetdata/xet-core` `e45aeb2f8c110438f6017be79c0b4c854bdbd2d0`](https://github.com/xetdata/xet-core/tree/e45aeb2f8c110438f6017be79c0b4c854bdbd2d0) (2024-10-16) | `gitxet` and `gitxetcore` 0.14.5 (`Cargo.toml`); latest tag `v0.14.4` (`febbf02`) | `rust/gitxetcore/src/git_integration/git_url.rs` (parser and its tests), `rust/gitxetcore/src/config/env.rs` |
| pyxet | [`xetdata/pyxet` `95196fa4a8ce328a37b53dac4f85f1adf5d8ba61`](https://github.com/xetdata/pyxet/tree/95196fa4a8ce328a37b53dac4f85f1adf5d8ba61) (2024-10-16) | 0.2.0-rc1 (`python/pyxet/pyproject.toml`, `pyxet/version.py`); the repository has no tags | `python/pyxet/pyxet/url_parsing.py`, `python/pyxet/pyxet/file_system.py` |

Checked on 2026-10-25 through the GitHub API: neither repository is archived,
and both were last pushed on 2024-10-16 (`xet-core` 17:59:02 UTC, `pyxet`
17:58:41 UTC), the commits that added their deprecation notices. PyPI's JSON
API (`/pypi/pyxet/json`) and simple index (`/simple/pyxet/`) both answered
`404` for `pyxet`, so no pyxet release is installable from PyPI today;
copies installed earlier, and addresses saved in notebooks and scripts, may
still exist. Both READMEs open with "[DEPRECATED]" and "XetHub has joined
Hugging Face", and `https://xethub.com/` answered `404` on 2026-10-25.

## Git-Xet 0.14.5

`XetPathInfo::parse` (`git_url.rs`) takes `xet://[domain/]user/repo[/branch[/path]]`:

- The endpoint is never the URI's host. It is `XET_ENDPOINT`, or
  `PROD_XETEA_DOMAIN` = `xethub.com` (`config/env.rs` line 5) when unset.
- When the host is `xethub.com`, it is replaced by that endpoint. Any other
  host is taken as the **user** and joined back onto the path: the comment
  reads "this is of the for xet://user/repo/...". So
  `xet://example.com/user/repo` means user `example.com`, repository `user`,
  branch `repo`, on the configured endpoint.
- The path splits on `/` into user, repository, branch (one segment) and the
  remaining path, which keeps a trailing `/` (`hello/world/`). At least a user
  and a repository are required (`xet://xethub.com/user` and `xet://user` are
  errors).
- The remote becomes `{scheme}://{endpoint}/{user}/{repo}`, with `https` unless
  `XET_ENDPOINT` names a scheme (`http://localhost:1234` in the tests).

The tests cover `xet://xethub.com/user/repo/branch/hello/world`, the same with a
trailing `/`, `…/branch/`, `…/branch`, `xet://xethub.com/user/repo`, the
truncated forms `xet://user/repo/branch/hello/world` down to `xet://user/repo`,
and endpoint overrides `xetbeta.com`, `xetbeta.com:5500`,
`https://localhost:1234` and `http://localhost:3000`.

Every Git-Xet address is endpoint-relative: the same text named a different
server for each reader's `XET_ENDPOINT`. No Git-Xet address can be read as
naming its server.

## pyxet 0.2.0-rc1

`parse_url` (`url_parsing.py`) accepts:

- `xet://{endpoint}:{user}/{repo}/{branch}/{path}`, the documented form: the
  authority splits at its **last** `:`, so `localhost:8080:user` is endpoint
  `localhost:8080` and user `user`. An empty endpoint (`xet://:user/…`) falls
  back to the default endpoint.
- `xet://{user}/{repo}/{branch}/{path}` without a colon: deprecated with a
  warning ("The use of the xet:// prefix without an endpoint is deprecated and
  will be disabled in the future"), resolved against the default endpoint:
  `set_default_endpoint`, then `XET_ENDPOINT`, then `xethub.com`, with the
  warning "Endpoint defaulting to xethub.com". If the authority ends in `.com`
  it is taken as the endpoint instead ("Cheap way now to see if it's a website
  or not").
- `{user}/{repo}/…` and `/{user}/{repo}/…` without a scheme, read as `xet`, and
  `http(s)://{endpoint}/{user}/{repo}/…`.
- Components are whitespace-stripped and empty ones skipped; the branch is one
  segment ("branches with '/' are not supported"); a trailing `/` on the path
  is kept. The repository is required by default, the branch only for some
  operations. `localhost` and `127.0.0.1` endpoints use `http`, others `https`.

No percent-decoding is applied anywhere.

### fsspec registration

`file_system.py` defines `XetFS(fsspec.spec.AbstractFileSystem)` with
`protocol = "xet"` (line 76, commented "This allows pandas, etc. to implement
'xet://'") and ends with `fsspec.register_implementation("xet", XetFS,
clobber=True)` (line 722). `clobber=True` is fsspec's option to register a
name that is already registered (`fsspec/registry.py`,
`register_implementation`; without it, fsspec raises for such a name), so the
handler for `xet` is whichever package registered it last.
`_strip_protocol` strips `xet://` or `https://` and leading slashes.

fsspec 2026.9.0 (tag commit `0f76baaae4a227b085b18e8469e6cc1792a95ade`) has no
`xet` entry in `known_implementations`; `hf` maps to
`huggingface_hub.HfFileSystem`. A `xet` handler exists only where pyxet, or
another package, registers one.

## Registry status

The [IANA URI scheme registry](https://www.iana.org/assignments/uri-schemes/uri-schemes-1.csv),
downloaded on 2026-10-25 (455 lines, header included), has no entry for `xet`,
`hf`, `ox` or `openxet`. OpenXet has not filed a registration request. The
specification lists that request, and notice to the legacy projects'
maintainers, as remaining work; neither has happened.

## Findings for the policy

1. **Endpoint-relative forms can't be translated.** Git-Xet never used the
   host, and pyxet's colon-less form used a default endpoint. The server those
   addresses named depended on the reader, and `xethub.com`, the default for
   both, no longer serves XetHub. Translating them would
   mean picking a server for the reader, which ADR 0009 item 2 forbids. They
   are refused with a hint that rewrites the rest of the address around a
   `<host>` the reader fills in.
2. **pyxet's explicit-endpoint form names a host.** `xet://{endpoint}:{user}/…`
   carries the server, so it can be rewritten on the same host as
   `xet://{endpoint}/{user}/{repo}@{branch}/{path}` without sending anything
   elsewhere. The parser reports the translation and refuses to apply it
   silently. The branch was a single segment in pyxet, so it is always a valid
   one-segment revision; a trailing `/` has no place in the current grammar
   and is dropped, which the translation's note says. The form is recognised
   only when the text after the colon is a valid namespace and a repository
   follows; any other non-numeric "port" is refused as a malformed port.
3. **Ambiguity is resolved toward the named host.** A colon-less address with
   a dotted host (`xet://example.com/user/repo/branch/path`) is valid under the
   current grammar, and is read with the current meaning: only the host it
   names is contacted. A pyxet address whose "user" is all digits reads as a
   port. A single-label host, the shape of every endpoint-less legacy address,
   is refused under the current grammar anyway (it is a local alias), so the
   two meanings can't silently diverge there.
4. **`xethub.com` is refused by name** in every form, including subdomains: no
   OpenXet deployment answers there.
5. **fsspec coexistence is M6's.** With pyxet registering `xet` with
   `clobber=True`, two packages that both register `xet` resolve by import
   order; the OpenXet adapter's coexistence policy and its tests belong to
   M6-URI.
