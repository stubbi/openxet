# Compatibility suite

Drives the official, unmodified Hugging Face clients against a target
endpoint. It is the project's most important test asset: it never gets
weaker to make a change pass.

Seven legs:

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
  `bash` and `python` block of `docs/quickstart-storage-only.md` and of
  `docs/quickstart-hub.md`, each in order, as one script in an empty
  directory, and fails if one takes 2 minutes.
  The end-to-end time to first upload, install included (design §5.8), is
  measured per release. Blocks fenced as `bash install` are skipped: the built
  `openxet` and this environment's `hf_xet`, `huggingface_hub` and `hf`
  stand in for them, with the runner's `git`.
- **Hub leg** (`-m hub`, `tests/test_hub.py`): stock `huggingface_hub` and
  the `hf` CLI against a configured `openxet serve` with two users, alice
  and bob, and the rules in `hub_harness.py`: login and `whoami`, uploads
  (regular and Xet files, a 1 GiB file, dedup across repositories),
  downloads, refs, listings, the typed views of a repository and their
  labels (design §6, ADR 0010), and the error shapes stock clients raise.
- **Git leg** (`-m git`, `tests/test_git.py`): `git` over smart HTTP:
  clone, push and pull (protocol v0 and v2), Hub commits and Git pushes on
  one history, two nodes on one data directory racing to push, a node with
  an empty Git cache, and a force-pushed commit no longer fetchable by its
  id.
- **LFS leg** (`-m lfs`, `tests/test_lfs_gitea.py`, `tests/test_lfs_openxet.py`,
  `tests/test_jj_xet.py`):
  repositories on Gitea with their LFS objects on openxet through
  `lfs.url` (ADR 0003): `git-xet` pushes and refreshes its token through
  the capability `href` with no credential, `git-lfs` pulls over `basic`,
  and plain `git-lfs` gets the `git-xet` install hint.
  `test_lfs_openxet.py` pushes a kernel repository's large file to an
  OpenXet-hosted repository with `git xet install`ed (design §12) and reads
  it back with `hf_hub_download` and a fresh clone. `test_jj_xet.py`
  replays jj-xet's expectations of openxet
  (`fixtures/lfs/jj-xet-expectations.json`). Neither needs Gitea.

Client version floor: `hf_xet==1.7.0`. It is the oldest `hf_xet` openxet
supports (ruling M1-R9), and the version this suite pins, so the floor is
the version CI proves; `test_version_floor_recorded` keeps the two equal.

```bash
cargo build
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest   # everything (needs Gitea, below)
uv run --frozen --directory compat pytest -m "not agent and not storage_only and not docs and not hub and not git and not lfs" # HF baseline only
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m agent
OPENXET_BIN=$PWD/target/release/openxet uv run --frozen --directory compat pytest -m storage_only
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m docs
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m hub
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m git
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m lfs   # needs Gitea, below
```

- `OPENXET_COMPAT_TARGET`: endpoint under test for the HF baseline
  (default `https://huggingface.co`).
- `OPENXET_BIN`: the built CLI. Tests marked `agent`, `storage_only`,
  `docs`, `hub`, `git` or `lfs` fail, not skip, without it, and the Gitea
  tests fail without Gitea's variables. The storage-only leg moves several
  hundred MB through the server; a release build
  (`cargo build --release -p openxet`) runs it faster.

Every leg that needs a server starts its own `openxet serve` per test, on
a free port with a temporary data directory, and counts requests from the
server's JSON debug log. A test fails if that log holds any token it
sent. Git runs get a `HOME` of their own and no system configuration
(`GIT_CONFIG_NOSYSTEM`), and take their credentials from a helper
`git_harness.py` writes, which reads them from the environment, so no
token reaches argv, a file, or a credential store outside the test. In
every leg, each child process gets its environment from an allowlist: no
`HF_XET_*`, `HF_HUB_*` or `OPENXET_*` variable from your shell reaches a
client or the server except those the harness sets itself (`HARNESS_ENV`
in `conftest.py`). Numbers worth watching but not yet asserted are printed
under "measurements (informational)" at the end of the run.

### LFS leg

The LFS leg needs, on `PATH`, `git-lfs` 3.7.1 and `git-xet` 0.2.1, the
versions spike S7 observed (`docs/research/S7-git-lfs-xet.md`); the
`compat-lfs` job in `.github/workflows/ci.yml` downloads both and checks
their SHA-256. It also needs Gitea, which `gitea/compose.yaml` runs pinned
by digest (Docker, or colima on macOS):

```bash
eval "$(compat/gitea/seed.sh)"   # starts Gitea on 127.0.0.1:3000 and exports OPENXET_COMPAT_GITEA_*
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m lfs
docker compose -f compat/gitea/compose.yaml down -v
```

Each test creates its own Gitea repository, so one Gitea serves many runs.
`OPENXET_COMPAT_GITEA_PORT` moves it off port 3000.

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
