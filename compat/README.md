# Compatibility suite

Drives the official, unmodified Hugging Face clients against a target
endpoint. It is the project's most important test asset: it never gets
weaker to make a change pass.

Four legs:

- **HF baseline:** stock `huggingface_hub` and `hf_xet` against
  huggingface.co.
- **Agent leg** (`-m agent`): drives the `openxet` CLI the way an agent
  would (JSON output, no prompts, stable exit codes).
- **Storage-only leg** (`-m storage_only`, `tests/test_storage_only.py`):
  stock `hf_xet` uploads to and downloads from `openxet serve` directly,
  with no Hub, as a platform embedding openxet does. It also crosses
  `hf_xet` with `openxet upload` / `download`, and signs CAS tokens with
  PyJWT as a platform would.
- **Docs leg** (`-m docs`, `tests/test_docs_quickstart.py`): runs every
  `bash` and `python` block of `docs/quickstart-storage-only.md`, in order,
  as one script in an empty directory, and fails if it takes 2 minutes.
  The end-to-end time to first upload, install included (design §5.8), is
  measured per release. Blocks fenced as `bash install` are skipped: the built
  `openxet` and this environment's `hf_xet` stand in for them.

Planned for M2's Hub leg, with stock `huggingface_hub` against openxet's
typed-route views (design §6, ADR 0010):
- `untyped_create_is_model`: `hf upload foo/bar` with no `--repo-type`,
  then `repo_info` and `snapshot_download` both work
- `typed_route_view_filters_by_label`
- `wrong_type_route_is_repo_not_found`: `RepositoryNotFoundError` for a
  repository read through a type it isn't labelled with
- `wrong_type_with_read_access_shows_label_hint`: with read access, the
  raised error's text tells how to add the label
- `wrong_type_without_read_access_matches_unknown_repo`: without read
  access, the response matches an unknown repository's: status, headers
  (excluding per-request ids and dates) and body
- `create_collision_answers_409_with_url`: a name taken under another
  label

Client version floor: `hf_xet==1.7.0`. It is the oldest `hf_xet` openxet
supports (ruling M1-R9), and the version this suite pins, so the floor is
the version CI proves; `test_version_floor_recorded` keeps the two equal.

```bash
cargo build
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest   # everything
uv run --frozen --directory compat pytest -m "not agent and not storage_only and not docs" # HF baseline only
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m agent
OPENXET_BIN=$PWD/target/release/openxet uv run --frozen --directory compat pytest -m storage_only
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m docs
```

- `OPENXET_COMPAT_TARGET`: endpoint under test for the HF baseline
  (default `https://huggingface.co`).
- `OPENXET_BIN`: the built CLI. Tests marked `agent`, `storage_only` or
  `docs` fail, not skip, without it. The storage-only leg moves several
  hundred MB through the server; a release build
  (`cargo build --release -p openxet`) runs it faster.

The storage-only leg starts its own `openxet serve` per test, on a free
port with a temporary data directory, and counts requests from the
server's JSON debug log. A test fails if that log holds any token it
sent. In both the storage-only and the agent leg, every child process
gets its environment from an allowlist: no `HF_XET_*`, `HF_HUB_*` or
`OPENXET_*` variable from your shell reaches a client or the server
except those the harness sets itself (`HARNESS_ENV` in `conftest.py`).
Numbers worth watching but not yet
asserted are printed under "measurements (informational)" at the end of
the run.

Client runs use an isolated `HF_HOME` and `HF_XET_CACHE` under pytest's
`tmp_path`; the real `~/.cache/huggingface` is never touched. Downloads run in
a subprocess because `huggingface_hub` reads `HF_HOME` at import.

The Xet path is proven from `hf_xet`'s own log under `$HF_XET_CACHE/logs`
(verified for `hf_xet` 1.7.0): it must record reconstructing the expected Xet
hash. If a client upgrade changes that log, the nightly `latest` leg fails
and the assertion in `conftest.py` is updated.

The client versions are pinned exactly in `pyproject.toml` and `uv.lock`.
`huggingface_hub` 2.2.0 has no `cli` extra: the `hf` CLI ships in the base
package. PyJWT and `cryptography`, which sign CAS tokens as a platform would,
are pinned in the `dev` group. The nightly workflow also runs the latest
releases, and the storage-only leg with the newest `hf-xet` release and
pre-release.

Add a CLI command to `JSON_COMMANDS` in `tests/test_agent_cli.py` whenever you
add one.
