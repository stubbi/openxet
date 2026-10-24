# openxet docs

openxet stores large files and reuses their unchanged parts when you upload
another version. It is an open-source server and client for the
[Xet protocol](https://huggingface.co/docs/xet).

**What works today:** the storage-only server runs on local disk with SQLite.
You can upload and download with stock `hf_xet` or the `openxet` command line.
There is no release yet; the quickstart builds from source. The Hub API for
`hf upload` and `huggingface_hub`, Git and LFS support, cloud storage, external
identity providers, the web UI, and MCP are planned. Hosted sign-ups are not
open yet.

openxet is an independent project, not affiliated with or endorsed by
Hugging Face.

- **Run it:** start with the [storage-only quickstart](quickstart-storage-only.md),
  then [configure the server](reference/config.md).
- **Script it:** every [command](reference/cli.md) takes `--json`, and the
  [exit codes](reference/exit-codes.md) are stable.
- **Integrate with it:** the [CAS routes](reference/routes.md) are the API
  stock clients call.
- **What changed:** the [changelog](../CHANGELOG.md).
- **Why it is built this way:** the [design](superpowers/specs/2026-10-23-openxet-design.md),
  the architecture decisions and the research notes, under Project.

## For agents

Every page is also published as plain Markdown at the same path with `.md`
in place of `.html`, for example
[`reference/cli.md`](https://openxet.com/docs/reference/cli.md).
[`/llms.txt`](https://openxet.com/llms.txt) lists them all.

## Editing these docs

This directory is the only place the docs are written; the site at
<https://openxet.com/docs/> is built from it on every merge to `main`.
`SUMMARY.md` is the navigation, and every page under `docs/` belongs in it
except the implementation plans in `superpowers/plans/` and the ADR template.
Links are written relative to the file, as GitHub shows them; the build
points links to files outside the book at GitHub.

Preview while you edit, with [mdBook](https://rust-lang.github.io/mdBook/)
0.5.4 (`cargo install mdbook --version 0.5.4 --locked`), Python 3 and Node.js 22+
for the full site build, from
the repository root:

```bash
mdbook serve site --open   # live preview of the docs at http://localhost:3000
scripts/build-site.sh      # the whole site as deployed, into target/site/public
```

`scripts/build-site.sh` fails on an mdBook warning, on a page missing from
`SUMMARY.md` and on a link to a file that does not exist. CI's `docs-site`
job runs it, then checks every link and anchor in the built site and in
`docs/` offline with [lychee](https://lychee.cli.rs/) 0.24.2:

```bash
lychee --offline --include-fragments --index-files index.html,. --root-dir "$PWD/target/site/public" \
  --exclude-path docs/SUMMARY.md target/site/public docs
```

The build also adds each page's canonical URL, social metadata, and Markdown
link. Its description comes from a `<!-- description: ... -->` comment, or the
opening prose paragraph. A missing description fails the build. Indexable
marketing pages need a canonical URL matching their published path; all such
pages enter the sitemap. Pages marked `noindex` stay out.

When several tables share a heading, give each a factual title using
`<!-- table-caption: What this table measures -->` immediately before it.
The HTML build renders a native caption, and the Markdown keeps the annotation.
Task-list checkboxes get accessible names from their own item's text, without
including nested tasks.

The public [security contact](https://openxet.com/.well-known/security.txt)
points to the private reporting route in [SECURITY.md](../SECURITY.md).
At each website release, verify that GitHub private vulnerability reporting is
still enabled. Review and renew the six-month `Expires` date in
`website/.well-known/security.txt` before it lapses; the build rejects an expired
or invalid date, or one a year or more away. After deployment, check the public
URL returns the contact as `text/plain`.

Pages includes hidden files so this standard discovery URL works. The final
artifact check permits only `.well-known/security.txt` and mdBook's
`docs/.nojekyll`, with `.well-known` as their only hidden directory. Unexpected
hidden paths and all symlinks fail the build; remove them from the source rather
than widening the allowlist. An intentional new public path needs its own review.
