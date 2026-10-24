# openxet

Open-source server and client for the [Xet protocol](https://huggingface.co/docs/xet),
Hugging Face's chunk-deduplicated storage for large, versioned files.
Models and datasets are common examples; any large file that changes over
time benefits.

The goal: the official tools (`hf`, `huggingface_hub`, `hf_xet`, `git-xet`,
`git-lfs`) work against your own infrastructure unmodified. Data stays in your
bucket, identity stays in your login system, and only changed chunks move.
A hosted version will be available at [openxet.com](https://openxet.com).

> **Status: the storage server works; the Hub API is next.** Milestone M1
> is done: `openxet serve` is a Xet CAS that stock `hf_xet` (1.7.0 and
> newer) uploads to and downloads from directly, with global dedup,
> verified uploads, scoped tokens and signed, range-bound download URLs.
> Reconstruction answers a 100k-term file within 50 ms at p99 on an idle
> node, gated in CI. It runs on local disk with a SQLite index. Next, M2
> adds the Hub API, so
> `hf upload`, `huggingface_hub` and Git work too; S3, GCS, Azure and
> Postgres come in M3. There is no release yet.

## Try it

[Quickstart](docs/quickstart-storage-only.md): start a server and upload
and download a file with `hf_xet` and with the `openxet` command line.
Once `openxet` is built, the whole walkthrough takes under a second; CI
runs it on every change.

## Documentation

- [Quickstart](docs/quickstart-storage-only.md)
- [Server configuration](docs/reference/config.md),
  [command line](docs/reference/cli.md),
  [exit codes](docs/reference/exit-codes.md) and
  [CAS routes](docs/reference/routes.md)
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
