# Compatibility suite

Drives the official, unmodified Hugging Face clients against a target
endpoint. It is the project's most important test asset: it never gets
weaker to make a change pass.

Today the target is huggingface.co, as a baseline. The agent leg drives the
`openxet` CLI the way an agent would (JSON output, no prompts, stable exit
codes).

```bash
cargo build
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest   # everything
uv run --frozen --directory compat pytest -m "not agent"                           # HF baseline only
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m agent
```

- `OPENXET_COMPAT_TARGET`: endpoint under test (default
  `https://huggingface.co`).
- `OPENXET_BIN`: the built CLI. Tests marked `agent` fail, not skip, without
  it.

Client runs use an isolated `HF_HOME` and `HF_XET_CACHE` under pytest's
`tmp_path`; the real `~/.cache/huggingface` is never touched. Downloads run in
a subprocess because `huggingface_hub` reads `HF_HOME` at import.

The Xet path is proven from `hf_xet`'s own log under `$HF_XET_CACHE/logs`
(verified for `hf_xet` 1.7.0): it must record reconstructing the expected Xet
hash. If a client upgrade changes that log, the nightly `latest` leg fails
and the assertion in `conftest.py` is updated.

The client versions are pinned exactly in `pyproject.toml` and `uv.lock`.
`huggingface_hub` 2.2.0 has no `cli` extra: the `hf` CLI ships in the base
package. The nightly workflow also runs the latest releases.

Add a CLI command to `JSON_COMMANDS` in `tests/test_agent_cli.py` whenever you
add one.
