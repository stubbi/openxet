# openxet

Open-source server and client for the [Xet protocol](https://huggingface.co/docs/xet),
Hugging Face's chunk-deduplicated storage for large, versioned files.
Models and datasets are common examples; any large file that changes over
time benefits.

The goal: the official tools (`hf`, `huggingface_hub`, `hf_xet`, `git-xet`,
`git-lfs`) work against your own infrastructure unmodified. Data stays in your
bucket, identity stays in your login system, and only changed chunks move.
A hosted version will be available at [openxet.com](https://openxet.com).

> **Status: foundation in place, server not yet usable.** The protocol crate
> (hashing, xorb and shard validation on `xet-core`'s crates) and the CI and
> compatibility harness exist; the CAS server is next (milestone M1).
> The approved design is in
> [`docs/superpowers/specs/2026-10-23-openxet-design.md`](docs/superpowers/specs/2026-10-23-openxet-design.md).
> Contributor and agent guidance is in [`AGENTS.md`](AGENTS.md).

## Project

- [Contributing](CONTRIBUTING.md) and [`AGENTS.md`](AGENTS.md) for agents
- [Governance](GOVERNANCE.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Security policy](SECURITY.md)

## License

[Apache-2.0](LICENSE)
