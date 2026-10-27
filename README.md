# OpenXet

Open-source server and client for the [Xet protocol](https://huggingface.co/docs/xet),
Hugging Face's chunk-deduplicated storage for large, versioned files.
Models and datasets are common examples; any large file that changes over
time benefits.

The goal: the official tools (`hf`, `huggingface_hub`, `hf_xet`, `git-xet`,
`git-lfs`) work against your own infrastructure unmodified. Data stays in your
bucket, identity stays in your login system, and only changed chunks move.
A hosted version will be available at [openxet.com](https://openxet.com).

> **Status: the storage server and the Hub API work.** `openxet serve` is
> a Xet CAS that stock `hf_xet` (1.7.0 and newer) uploads to and downloads
> from directly, with global dedup, verified uploads, scoped tokens and
> signed, range-bound download URLs. On top of it, stock `hf` and
> `huggingface_hub` (2.2.0 and newer) create repositories, upload, download
> and list them, and `git clone` and `git push` work over HTTPS on the same
> history, with large files through Git LFS and `git-xet`. Reconstruction
> answers a 100k-term file within 50 ms at p99 on an idle node, gated in
> CI. It runs on local disk with a SQLite index; S3, GCS, Azure and
> Postgres come next, in M3. There is no release yet.

## Try it

- [Quickstart: hf and git](docs/quickstart-hub.md): start a server, upload
  a folder with `hf upload`, download it, then clone, change and push it
  with `git`.
- [Quickstart: storage only](docs/quickstart-storage-only.md): the storage
  layer on its own, with `hf_xet` and the `openxet` command line.

CI runs both quickstarts as written on every change.

## Documentation

- Quickstarts: [hf and git](docs/quickstart-hub.md) and
  [storage only](docs/quickstart-storage-only.md)
- Guides: [Git hosting](docs/guides/git-hosting.md) and
  [Git LFS for any Git host](docs/guides/lfs-for-any-repo.md)
- [Server configuration](docs/reference/config.md),
  [command line](docs/reference/cli.md),
  [exit codes](docs/reference/exit-codes.md), and the routes:
  [Hub](docs/reference/hub-routes.md), [Git](docs/reference/git-routes.md),
  [Git LFS](docs/reference/lfs.md), [CAS](docs/reference/routes.md) and the
  [OpenAPI document](docs/reference/openapi.md)
- [Security tests](docs/reference/security-tests.md): each security
  requirement and the test that enforces it
- [Benchmarks](docs/reference/benchmarks.md): the latency gate and the
  benchmarks, with their last results
- [Changelog](CHANGELOG.md)
- [Design](docs/superpowers/specs/2026-10-23-openxet-design.md),
  [architecture decisions](docs/adr/) and the
  [competitive matrix](docs/competitive-matrix.md)

## Project

- [Contributing](CONTRIBUTING.md) and [`AGENTS.md`](AGENTS.md) for agents
- [Governance](GOVERNANCE.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Security policy](SECURITY.md)

## License

[Apache-2.0](LICENSE)
