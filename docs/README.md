# openxet docs

openxet is the open-source server and client for the
[Xet protocol](https://huggingface.co/docs/xet), Hugging Face's
chunk-deduplicated storage for large, versioned files. The official Hugging
Face clients work against it unmodified.

- **Run it:** start with the [storage-only quickstart](quickstart-storage-only.md),
  then [configure the server](reference/config.md).
- **Script it:** every [command](reference/cli.md) takes `--json`, and the
  [exit codes](reference/exit-codes.md) are stable.
- **Integrate with it:** the [CAS routes](reference/routes.md) are the API
  stock clients call.
- **What changed:** the [changelog](changelog.md).
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
0.5.4 (`cargo install mdbook --version 0.5.4 --locked`) and `python3`, from
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
lychee --offline --include-fragments --root-dir "$PWD/target/site/public" \
  --exclude-path docs/SUMMARY.md target/site/public docs
```
