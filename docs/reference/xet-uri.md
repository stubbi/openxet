# xet:// resource addresses

<!-- description: The xet:// specification: grammar, resource kinds, revisions, escaping, hosts and ports, the HTTPS mapping, credential selection and the legacy XetHub policy. -->

A `xet://` address names one resource on one OpenXet deployment: a
repository, a typed view of it, a bucket, a revision, or a file or folder
inside one. This page is the contract that every OpenXet parser, formatter
and client follows ([ADR 0009](../adr/0009-xet-resource-uris.md), M2-URI).
The shared implementation is the `openxet-uri` crate (`crates/uri`); its
golden vectors (`crates/uri/testdata/vectors.json`) are part of this
specification, and a test fails when the crate and the vectors disagree.

Status, as of OpenXet M2:

- The grammar, the parser and the canonical formatter are implemented in
  `openxet-uri`. No released command takes a `xet://` address yet; the
  CLI, SDK and UI adopt it in M6 (M6-URI). Until then, use the released
  commands and HTTPS links.
- HTTPS stays the canonical link to share in a browser, and every stock
  client (`hf`, `huggingface_hub`, `git`) keeps using HTTPS. Adding
  `xet://` changes nothing they recognise.
- `xet` is **not** registered with IANA. The
  [URI scheme registry](https://www.iana.org/assignments/uri-schemes/uri-schemes-1.csv)
  had no `xet` entry when checked on 2026-10-25, and OpenXet has not yet
  filed a registration request under
  [RFC 7595](https://www.rfc-editor.org/rfc/rfc7595.html). The name was
  used before by XetHub's deprecated tools; see
  [Legacy forms](#legacy-forms) and the
  [legacy audit](../research/xet-uri-legacy-audit-2026-10.md).
- A `xet://` address is an OpenXet resource address, not a name for the
  Xet protocol. Chunking, hashes, xorbs, shards and the CAS API are the
  [Xet specification](https://huggingface.co/docs/xet/index)'s, unchanged.

## Shape

```text
xet://{host}[:{port}]/[{kind}/]{namespace}/{name}[@{revision}][/{path}]
```

```text
xet://hub.example.org/team/corpus
xet://hub.example.org/datasets/team/corpus@v2/train/part-0001.parquet
xet://hub.example.org/team/model@refs/pr/3/config.json
xet://hub.example.org/team/model@feature%2Fsplit/README.md
xet://hub.example.org/team/model@0f9a6e1c8d2b4a7f5e3c1b9d8a7f6e5d4c3b2a19/weights.bin
xet://127.0.0.1:8080/dev/scratch/notes.txt
xet://[2001:db8::7]:8443/team/corpus
```

The host is always written: an address means the same thing for everyone
who reads it, and never falls back to a reader's default server.

## Grammar

ABNF ([RFC 5234](https://www.rfc-editor.org/rfc/rfc5234)), using
`ALPHA`, `DIGIT`, `HEXDIG`, `IPv4address`, `IPv6address`, `pct-encoded`,
`unreserved` and `sub-delims` from
[RFC 3986](https://www.rfc-editor.org/rfc/rfc3986). A `xet://` address
is an RFC 3986 URI with an authority, no userinfo, no query and no
fragment.

```abnf
xet-uri      = scheme "://" authority "/" resource
scheme       = "xet"                            ; case-insensitive, written lowercase
authority    = host [ ":" port ]                ; no userinfo: an "@" here is refused
host         = "[" IPv6address "]" / IPv4address / reg-name
reg-name     = "localhost" / label 1*( "." label )
label        = lchar [ *( lchar / "-" ) lchar ] ; at most 63 octets
lchar        = ALPHA / DIGIT / "_"
port         = %x31-39 *4DIGIT                  ; 1-65535, no leading zero

resource     = [ kind "/" ] namespace "/" name [ "@" revision ] [ "/" path ]
kind         = "datasets" / "spaces" / "kernels" / "buckets"
namespace    = id
name         = id
id           = 1*( unreserved / pct-encoded )      ; decoded: M2-R2's rule, below

revision     = special-ref / rev-segment
special-ref  = "refs/pr/" 1*DIGIT
             / "refs/convert/" 1*( ALPHA / DIGIT / "_" / "." / "-" )
rev-segment  = 1*( rchar / pct-encoded / ucschar )
rchar        = unreserved / sub-delims / ":" / "@"

path         = segment *( "/" segment )
segment      = 1*( pchar / ucschar )
pchar        = unreserved / pct-encoded / sub-delims / ":" / "@"
ucschar      = <any Unicode scalar value above U+007F, encoded as UTF-8>
```

The grammar is read left to right: the authority ends at the first `/`;
the first path segment is a `kind` when it is one of the four kind names;
the next two are the namespace and the name; the name ends at its first
`@`, which starts the revision; the revision ends at the next `/` (after a
`special-ref`, at the end of the ref); everything after that `/` is the
path.

Each rule also carries a constraint the ABNF can't express:

| Part | Constraint | Error code |
|---|---|---|
| scheme | `xet`, in any case | `not_xet_uri` |
| authority | no `@` (userinfo); the refusal never repeats what was before the `@` | `credentials_in_uri` |
| host | present; IPv4 in strict dotted-decimal form (no leading zeros, four parts); a name whose last label is all digits is not a name | `missing_host`, `invalid_host` |
| host | a name has two labels or more, or is `localhost` | `host_not_qualified`, `legacy_no_host` |
| host | ASCII only: an internationalised name is written as its A-label (`xn--…`); the error names the A-label | `non_ascii_host` |
| host | IPv6 without a zone identifier (`%25…`) and no `IPvFuture` | `invalid_host` |
| host | not `xethub.com` or a subdomain of it | `legacy_xethub` |
| port | 1-65535, decimal, no leading zero. A non-numeric "port" is pyxet's explicit-endpoint form only when it is a valid namespace and a repository follows (see [Legacy forms](#legacy-forms)); anything else is a malformed port | `invalid_port`, `legacy_pyxet_address` |
| kind | exactly one of the four names, written literally in lowercase. The first segment is matched after percent-decoding: `models`, `model`, `dataset`, `space`, `kernel` and `bucket` there are refused, never read as a namespace, and so is a percent-encoded spelling of any kind word (`dataset%73`) | `unknown_kind` |
| namespace, name | after percent-decoding, `[A-Za-z0-9][A-Za-z0-9._-]{0,95}` with no `..` (ruling M2-R2); a name does not end in `.git`. An escaped unreserved character is accepted and written plainly in the canonical form (`%63orpus` is `corpus`) | `invalid_namespace`, `invalid_name`, `missing_repository` |
| namespace | not reserved (ruling M2-R2), ignoring ASCII case: `models`, `datasets`, `spaces`, `kernels`, `buckets`, `api`, `v1`, `v2`, `openapi.json`, `healthz`, `readyz` | `reserved_namespace` |
| revision | not on a bucket; not empty; a valid Git ref name or object id after decoding (below) | `revision_on_bucket`, `empty_revision`, `invalid_revision` |
| revision | a bare `refs` followed by a path is refused: a ref with `/` is written with `%2F` | `ambiguous_revision` |
| path | no empty segment (so no trailing `/` and no `//`), no `.` or `..` segment, no encoded `/` (`%2F`) and no NUL (`%00`) | `invalid_path` |
| everywhere | `%` is followed by two hex digits, and decoded octets are UTF-8 | `invalid_percent_encoding` |
| everywhere | no space, control character, `"`, `<`, `>`, `\`, `^`, `` ` ``, `{`, `|`, `}`, `[` or `]` outside the host | `invalid_character` |
| everywhere | no `?` (query) and no `#` (fragment); the refusal never repeats the query | `query_not_supported`, `fragment_not_supported` |

A revision is valid when it is a Git ref name under the rules of
[`git check-ref-format`](https://git-scm.com/docs/git-check-ref-format)
(one-level names such as `main` included): no control character, space,
`~`, `^`, `:`, `?`, `*`, `[` or `\`; no `..` and no `@{`; not `@` alone;
no leading, trailing or doubled `/`; no component starting with `.` or
ending in `.lock`; not ending in `.`. A 40- or 64-character hexadecimal
object id passes the same rules.

Every error has a stable `code`, a `message` that says what is wrong and
a `hint` that says how to write the address instead.

## Resource kinds

The kind says which view of the deployment's storage the address goes
through ([ADR 0010](../adr/0010-two-storage-primitives.md), design §6):

| Written | `Kind` | Means | Resolves from |
|---|---|---|---|
| *(none)* | `Repository` | the repository `{namespace}/{name}`, whatever its labels | M2 |
| `datasets/` | `Dataset` | the repository through its `dataset` view; a repository without the `dataset` label answers as not found | M2 |
| `spaces/` | `Space` | the `space` view | M2 |
| `kernels/` | `Kernel` | the `kernel` view | M2 |
| `buckets/` | `Bucket` | the bucket primitive `{namespace}/{name}`; buckets have no revisions | M5 |

There is no `models/` kind. A repository is addressed by
`{namespace}/{name}` alone, so the address stays valid when its labels
change. On huggingface.co, `hf://models/…` is the model view; in a
`xet://` address the first segment `models` is refused with a hint rather
than read as a namespace, as are the singular forms (`dataset/` and the
rest), which `hf://` refuses too. The plural kind words, and the first
segments of OpenXet's own routes, are reserved namespaces (ruling M2-R2,
ignoring case), so no repository can take a name that an address would
read as something else.

Bucket addresses parse and format in M2 so that one parser serves every
milestone; resolving them needs the bucket primitive, which ships in M5.

## Mutable and immutable references

- **No revision** names the repository's default branch, `main` (ruling
  M2-R5), as it is when the address is used. It is mutable.
- **A branch or tag** (`@main`, `@v2`, `@refs/pr/3`) is mutable: the
  same address can name different content tomorrow.
- **A full object id**, 40 or 64 lowercase hexadecimal characters, is
  immutable: the address names one commit's content for as long as the
  commit exists. `XetUri::is_immutable` reports this. An abbreviated id is
  read as a ref name, never expanded.
- A **bucket** address is always mutable: buckets have no history.

Revisions are opaque to the parser apart from the rules above: it never
looks a ref up, expands it, or changes its case.

Revisions containing `/` are written with `%2F` (`@feature%2Fsplit`),
because the first `/` after the revision starts the path. The two special
ref families of huggingface.co, `refs/pr/{n}` and `refs/convert/{name}`,
are written as they are (`@refs/pr/3/config.json`), matching `hf://`
([`huggingface_hub` 2.2.0, `_hf_uris.py`](https://github.com/huggingface/huggingface_hub/blob/v2.2.0/src/huggingface_hub/utils/_hf_uris.py)).
Any other `refs/…` ref is written with `%2F`; `@refs/heads/main/x` is
refused as ambiguous rather than read as the branch `refs` and the path
`heads/main/x`.

Namespaces and names can't contain `@` (ruling M2-R2), so the first `@`
in the name segment always starts the revision. A `%40` there decodes to
`@` and is refused as an invalid name. In a revision or a path, `@` needs
no escaping.

## Escaping and Unicode

- Namespaces, names, revisions and paths are compared and stored decoded.
  The parser decodes `%XX` (either case), and the formatter writes every
  octet outside `unreserved` (`A-Z a-z 0-9 - . _ ~`) as `%XX` with
  uppercase hex. `/` between path segments, and inside a special ref, is
  written as is.
- Paths and revisions are UTF-8. Raw non-ASCII characters are accepted on
  input, as in an IRI ([RFC 3987 §3.1](https://www.rfc-editor.org/rfc/rfc3987#section-3.1)),
  and the canonical form percent-encodes their UTF-8 octets.
- **No Unicode normalization is applied.** `é` written as U+00E9 and as
  `e` + U+0301 are different paths, because Git compares path bytes.
  Percent-decoded octets that are not UTF-8 are refused.
- The canonical form also lowercases the scheme and the host, writes an
  IPv6 address as the [WHATWG URL standard](https://url.spec.whatwg.org/#concept-ipv6-serializer)
  serializes it (the [RFC 5952](https://www.rfc-editor.org/rfc/rfc5952)
  compression, with an IPv4-mapped address in hex: `[::ffff:1.2.3.4]` is
  `[::ffff:102:304]`), so the host equals the HTTPS URL's, and drops port
  `443`, the default. Nothing else is rewritten:
  the namespace, name, revision and path keep their case.

Parsing is a function of the text alone. `format(parse(s))` is the
canonical form of `s`, `parse(format(u)) == u` for every valid `u`, and
formatting is idempotent; property tests and a fuzz target check all three
(`crates/uri/tests/props.rs`, `fuzz/fuzz_targets/xet_uri.rs`).

## Hosts and ports

- The host is the deployment's DNS name or IP address, exactly as clients
  reach it. Two addresses name the same deployment only if their hosts
  are equal after lowercasing and their ports are equal; no DNS lookup or
  alias is consulted.
- A single-label name such as `storage` is refused: it resolves through
  the reader's search domains, which makes it a local alias that names a
  different server on another network. Write the fully qualified name.
  `localhost` is the one single-label name allowed.
- IPv4 must be four decimal parts without leading zeros (`127.0.0.1`, not
  `127.1` or `0x7f.0.0.1`), because other parsers read those forms
  differently.
- IPv6 is written in brackets: `xet://[::1]:8080/…`. Zone identifiers are
  refused: they name an interface of the reader's machine.
- The port is optional. Without one, the address names port 443.

## Local development

The `openxet serve` command listens on `http://127.0.0.1:8080` by default, and a local
address names it directly:

```text
xet://127.0.0.1:8080/dev/scratch
xet://localhost:8080/dev/scratch/notes.txt
```

Loopback hosts (`localhost`, `127.0.0.0/8`, `[::1]`) are reached over
plain HTTP unless the client is configured otherwise; every other host is
reached over HTTPS. An address without a port names port 443 over plain
HTTP too: `xet://localhost/…` is `http://localhost:443/…`, so a local
server's address carries its port. Such an address is meaningful only on the machine
that runs the server: share an HTTPS link or a public host's address
instead.

## The HTTPS mapping

Each address maps to an HTTPS URL on the same host and port
(`XetUri::https`), the routes `huggingface_hub` uses
(`REPO_TYPES_URL_PREFIXES`: `datasets/`, `spaces/`, `kernels/`, none for
the unprefixed form):

| Address | HTTPS URL |
|---|---|
| `xet://h/{ns}/{name}` | `https://h/{ns}/{name}` |
| `xet://h/datasets/{ns}/{name}` | `https://h/datasets/{ns}/{name}` |
| `xet://h/{ns}/{name}@{rev}` | `https://h/{ns}/{name}/tree/{rev}` |
| `xet://h/{ns}/{name}/{path}` | `https://h/{ns}/{name}/resolve/main/{path}` |
| `xet://h/{ns}/{name}@{rev}/{path}` | `https://h/{ns}/{name}/resolve/{rev}/{path}` |
| `xet://h/buckets/{ns}/{name}` | `https://h/buckets/{ns}/{name}` |
| `xet://h/buckets/{ns}/{name}/{path}` | `https://h/buckets/{ns}/{name}/resolve/{path}` |

The revision is percent-encoded as one segment (`/` as `%2F`), except a
special ref, which is written as is, as `huggingface_hub`'s `to_url`
does. A file path maps to `resolve`, the download route OpenXet serves
from M2 (Task 13), not to a web viewer.

The unprefixed resolve route is the `model` view (design §6), so in M2 a
file address without a kind resolves on repositories that carry the
`model` label, and a file of a repository with only another label is
addressed through that label's kind (`xet://h/datasets/…`). The unprefixed
Git URL serves every repository whatever its labels. M6 revisits the
mapping of a kind-less file address, so that it resolves whatever the
repository's labels.

## Reverse-proxy prefixes

A deployment served under a base path, such as `https://example.org/hub/`,
can't be addressed in M2: the first segment after the authority is always
a kind or a namespace, so there is no room for a prefix, and guessing one
would let the same text name different resources. A client configured
with an endpoint that has a path refuses it (`base_path_unsupported`) and
says to serve OpenXet at the root of its own host name. M6 revisits base
paths together with endpoint discovery.

## Credentials and endpoints

A `xet://` address never carries a credential:

- `xet://user:token@host/…` is refused (`credentials_in_uri`), and the
  error does not repeat the userinfo. There is no query string either, so
  no `?token=`.
- A client selects the credential for an address by the address's exact
  host and port, from the endpoints it is configured with
  (`openxet_uri::Endpoints`). A default endpoint's credential is never
  sent to another host: an address naming a host the client has no
  endpoint for is fetched without a credential.
- A redirect keeps the credential only when it stays on the same origin
  (scheme, host and port). A redirect to another host, or from HTTPS to
  HTTP, drops it, even when the client holds a credential for the new
  host: a server can't choose which of the client's credentials to
  receive. A `Location` with userinfo is refused.
- A credential is configured only for an HTTPS endpoint, or for a plain
  HTTP endpoint on a loopback host (`credential_over_http` otherwise), so
  it never crosses a network unencrypted.
- Opening an address identifies or reads a resource. It never uploads,
  deletes or runs anything; a write needs its command's own
  authorization.

These rules are tested at the library level in
`crates/uri/tests/endpoints.rs` (`endpoints_isolate_credentials`): two
endpoint fixtures hold the same names with different content and
different credentials, and each address reaches only its own host with
only its own credential.

## Legacy forms

XetHub's deprecated tools used `xet://` with other grammars: Git-Xet
(`git-xet` 0.14.5) read the host as a user name on the server chosen by
`XET_ENDPOINT` (default `xethub.com`), and pyxet (0.2.0-rc1) wrote the
endpoint before a colon (`xet://{endpoint}:{user}/{repo}/{branch}/{path}`)
and defaulted to `xethub.com` without one. Both projects are deprecated
since XetHub joined Hugging Face, and the
[legacy audit](../research/xet-uri-legacy-audit-2026-10.md) records the
sources, versions and registrations this policy is based on.

The policy, tested row by row in `crates/uri/tests/legacy.rs`
(`legacy_matrix`), is:

- A form that names no host is refused: it pointed at whichever server the
  reader had configured, and OpenXet never sends an address to a default
  server.
- A form that names `xethub.com` is refused: no OpenXet deployment answers
  there, and the data's new home can't be inferred.
- pyxet's explicit-endpoint form names a host, so it is translated to the
  equivalent address on the same host and reported. The parser never
  applies the translation silently: `classify_legacy` returns it, and
  parsing fails with the translated address in its hint.
- An address that is valid under the current grammar is read with the
  current meaning, even where a legacy tool read it differently; only the
  host it names is ever contacted.

| # | Input | Origin | Outcome |
|---|---|---|---|
| L1 | `xet://hub.example.org/team/corpus@main/a.txt` | current grammar | Accepted |
| L2 | `xet://xethub.com/user/repo/branch/hello/world` | Git-Xet (tests), pyxet (`.com` rule) | Rejected, `legacy_xethub` |
| L3 | `xet://xethub.com:user/repo/main/data.csv` | pyxet explicit endpoint | Rejected, `legacy_xethub` |
| L4 | `xet://www.xethub.com/user/repo` | XetHub subdomain | Rejected, `legacy_xethub` |
| L5 | `xet://user/repo/branch/hello/world` | Git-Xet and pyxet without a host | Rejected, `legacy_no_host`; hint `xet://<host>/user/repo@branch/hello/world` |
| L6 | `xet://user/repo/branch/` | Git-Xet and pyxet without a host | Rejected, `legacy_no_host`; hint `xet://<host>/user/repo@branch` |
| L7 | `xet://user/repo` | Git-Xet and pyxet without a host | Rejected, `legacy_no_host`; hint `xet://<host>/user/repo` |
| L8 | `xet://user` | Git-Xet and pyxet without a host | Rejected, `legacy_no_host` |
| L9 | `xet://:user/repo/main` | pyxet with an empty endpoint | Rejected, `legacy_no_host` |
| L10 | `xet://hub.example.org:user/repo/main/a/b.csv` | pyxet explicit endpoint | Translated to `xet://hub.example.org/user/repo@main/a/b.csv` |
| L11 | `xet://localhost:8080:user/repo/main/x` | pyxet explicit endpoint with a port | Translated to `xet://localhost:8080/user/repo@main/x` |
| L12 | `xet://hub.example.org:user/repo/main/dir/` | pyxet, folder with trailing `/` | Translated to `xet://hub.example.org/user/repo@main/dir` (the `/` is dropped and the note says so) |
| L13 | `xet://hub.example.org:user/repo` | pyxet explicit endpoint, no branch | Translated to `xet://hub.example.org/user/repo` |
| L14 | `xet://hub.example.org:user` | pyxet, user only | Rejected, `invalid_port` (no repository follows, so it isn't read as pyxet) |
| L15 | `xet://storage:user/repo/main` | pyxet with a single-label endpoint | Rejected, `host_not_qualified` |
| L16 | `/user/repo/branch/path` | pyxet without a scheme | Rejected, `not_xet_uri` |
| L17 | `https://xethub.com/user/repo/branch/path` | pyxet HTTPS form | Rejected, `not_xet_uri` |
| L18 | `xet://example.com/user/repo/branch/path` | Git-Xet read `example.com` as a user; pyxet as an endpoint with branch `branch` | Accepted, current meaning: host `example.com`, repository `user/repo`, path `branch/path` |
| L19 | `xet://hub.example.org:8080/user/repo/main` | pyxet would read `8080` as a user | Accepted, current meaning: port 8080, repository `user/repo`, path `main` |
| L20 | `xet://localhost/user/repo` | Git-Xet would read `localhost` as a user | Accepted, current meaning: host `localhost` |

### fsspec

pyxet registers its fsspec filesystem for the protocol `xet` when it is
imported, with `clobber=True`, fsspec's option to take over a name that is
already registered. fsspec 2026.9.0 has no built-in `xet` entry. OpenXet's own fsspec adapter
arrives in M6 and will not silently replace another installed `xet`
handler (ADR 0009 item 5); its coexistence policy is part of M6-URI.

## Registration and coordination

Neither an IANA registration nor any agreement with Hugging Face or the
former XetHub maintainers has happened. The work that remains, tracked
under M6-URI and M7 (design §12):

- request provisional registration of `xet` under RFC 7595 §7, with this
  page as the specification, once a released client implements it;
- tell the deprecated projects' maintainers (`xetdata/xet-core`,
  `xetdata/pyxet`) about the legacy policy before the first release that
  parses `xet://`.
