# M0 Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lay the foundation that every later milestone stands on:
- a Rust workspace with the project's quality gates
- CI and governance
- golden protocol tests against HF's reference files
- a real-client compatibility harness
- answers to spikes S1–S9, recorded as research notes, ADRs and captured
  fixtures

**Architecture:**
- A Cargo workspace (`crates/*`). It starts with two crates:
  - `openxet`: the single binary, CLI only for now.
  - `openxet-protocol`: our only doorway to HF's `xet-core-structures`
    (ADR 0001).
- A Python/uv compatibility harness in `compat/` that drives pinned real
  clients against a target endpoint. It starts with huggingface.co as the
  baseline target.
- Spikes answer the design's open questions before M1 code depends on them.
  Their throwaway code never enters the repo; their findings, fixtures and
  ADRs do.

**Tech Stack:**
- Rust 1.97.1 (edition 2024), `xet-core-structures` 1.7.0 (exact pin),
  clap 4, serde/serde_json, thiserror
- cargo-nextest, cargo-deny
- Python 3.13+ via uv, pytest, `huggingface_hub` 2.2.0, `hf_xet` 1.7.0
- GitHub Actions

**Spec:**
- `docs/superpowers/specs/2026-10-23-openxet-design.md`
- ADR `docs/adr/0001-build-from-scratch-on-xet-core.md`
- lessons list in `docs/competitive-matrix.md`

**Scope note:** the design spans M0–M7. Each milestone gets its own plan,
written when the previous one lands, because spike results (this plan) and
real code change what the later plans must say. The final task of this plan
writes the M1 plan.

## Global Constraints

- **License:** Apache-2.0. Every crate sets `license = "Apache-2.0"`; NOTICE
  credits `huggingface/xet-core`.
- **Rust toolchain** pinned in `rust-toolchain.toml` to `1.97.1`. The
  workspace `rust-version = "1.97"`; `edition = "2024"`.
- `#![forbid(unsafe_code)]` in every crate root.
- **Workspace lints:**
  - `clippy::all`, `clippy::pedantic` and `rust_2018_idioms` at `warn`.
  - CI runs with `-D warnings`.
  - `unwrap_used` and `expect_used` are denied in non-test code.
  - A root `clippy.toml` sets `allow-unwrap-in-tests = true` and
    `allow-expect-in-tests = true`.
- **Pinning:** `xet-core-structures` uses the exact version `=1.7.0`. Bumps
  are deliberate and run the compatibility suite.
- **Tests never skip silently.** A missing fixture, tool or credential is a
  hard failure with a message saying how to provide it. Opt-out happens only
  via an explicit, documented env var that CI never sets. This is the lesson
  from Shardline's silently-skipping git-xet tests.
- **CLI agent UX from the first command:**
  - Every command supports `--json`.
  - JSON output carries `"schema": "openxet.<command>.v1"`.
  - Exit codes are documented in `docs/reference/exit-codes.md`.
- **Commits** follow Conventional Commits with the scopes in `AGENTS.md` and
  end with the `Co-Authored-By` trailer the session prescribes.
- **Fixtures:**
  - Reference files are fetched from
    `datasets/xet-team/xet-spec-reference-files` at revision
    `c4aa3a3f15b1395fff5ce934784bf6c8f2d62de8` and verified against a
    committed `SHA256SUMS`.
  - Known values: the CSV sha256 is
    `f41255b252f776125f2b657136654e4d4d5d2ccf8ef4db0ec186bd1981b69734`; the
    xorb sha256 is
    `e72f4e05b9c4b96f1c6b2213d93de00ab943585a2f7c049fd127ba72ec1ff413`.
  - Large binaries are never committed.
- **Spike code** lives outside the repo (a scratch directory). Only findings
  (`docs/research/`), fixtures (`compat/fixtures/`) and ADRs (`docs/adr/`)
  are committed.
- **Outward-facing actions need explicit user confirmation in the session:**
  GitHub settings, creating HF orgs or tokens, publishing anything.

## Review Focus

1. **Fixtures absent or corrupted:** a test run without fetched reference
   files must fail with "run `scripts/fetch-reference-files.sh`", never pass
   or skip. Pinned in Task 4.
2. **Reference-file revision drift:** if upstream changes the files, our
   pinned revision and `SHA256SUMS` must catch it. A checksum mismatch fails
   the fetch script with a non-zero exit. Pinned in Task 4.
3. **Agent UX regression:** a `--json` output that isn't valid JSON or lacks
   `schema` must fail CI. Pinned in Task 1 (unit test) and Task 5 (agent leg).
4. **Compat harness pointed at the wrong target:**
   - The harness must print and record the target endpoint and client
     versions in every report. Without that, a "pass" against huggingface.co
     could be mistaken for a pass against openxet.
   - It must refuse to run against a non-default target unless
     `OPENXET_COMPAT_TARGET` is set explicitly.

   Pinned in Task 5.
5. **Secrets in fixtures:** captured HTTP fixtures (Task 7, Task 10) must have
   `Authorization`, `X-Xet-Access-Token`, cookies and signed URL query
   strings redacted. A test scans `compat/fixtures/` for token-like patterns
   (`hf_`, `xet_`, `Bearer `, `Signature=`, `X-Amz-Signature`) and fails on
   any hit. Pinned in Task 7.

---

### Task 1: Cargo workspace and the `openxet` binary with human and agent output

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `.gitignore`,
  `crates/openxet/Cargo.toml`, `crates/openxet/src/main.rs`,
  `crates/openxet/src/cli.rs`, `crates/openxet/src/output.rs`,
  `crates/openxet/tests/cli_version.rs`, `docs/reference/exit-codes.md`
- Modify: `AGENTS.md` (add a **Commands** section; replace the line that
  promises it)

**Interfaces:**
- Produces:
  - `openxet version [--json]`
  - `output::Output` (`Human` | `Json`) with `fn emit<T: Serialize + HumanDisplay>(&self, value: &T) -> std::io::Result<()>`
  - `trait HumanDisplay { fn human(&self) -> String; }`
  - `ExitCode` constants in `crates/openxet/src/cli.rs`: `SUCCESS = 0`,
    `FAILURE = 1`, `USAGE = 2`

  Later tasks and milestones add commands through clap subcommands in
  `cli.rs`.

- [ ] **Step 1: Write the failing tests** in `crates/openxet/tests/cli_version.rs`
  (use `assert_cmd` + `predicates` as dev-dependencies)

```rust
#[test]
fn version_human_names_the_binary_and_semver() {
    Command::cargo_bin("openxet").unwrap().arg("version")
        .assert().success()
        .stdout(predicates::str::starts_with(format!("openxet {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn version_json_is_versioned_schema() {
    let out = Command::cargo_bin("openxet").unwrap().args(["version", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "openxet.version.v1");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["git_commit"].is_string());
    assert!(v["protocol_spec"].as_str().unwrap().starts_with("1.1"));
}

#[test]
fn unknown_command_exits_with_usage_code_and_json_error_when_asked() {
    let out = Command::cargo_bin("openxet").unwrap().args(["nope", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["schema"], "openxet.error.v1");
    assert_eq!(v["code"], "usage");
    assert!(v["hint"].as_str().unwrap().contains("openxet --help"));
}
```

- [ ] **Step 2: Run them and confirm they fail**
  Run: `cargo nextest run -p openxet`.
  Expected: build failure (no `openxet` binary).

- [ ] **Step 3: Create the workspace and implement**
  - Workspace `Cargo.toml`:
    - `members = ["crates/*"]`, `resolver = "3"`
    - `[workspace.package]` (edition, rust-version, license, repository
      `https://github.com/stubbi/openxet`)
    - `[workspace.lints]` per Global Constraints
  - `git_commit` comes from a `build.rs` that reads `git rev-parse --short
    HEAD`, falling back to `"unknown"` outside a git checkout.
  - `protocol_spec` is the constant `"1.1.0"` (Xet spec version, design §3).
  - Usage errors with `--json` anywhere in argv print the `openxet.error.v1`
    object to stderr: `{schema, code, message, hint}`.
  - `docs/reference/exit-codes.md` documents 0/1/2.

- [ ] **Step 4: Run the tests and confirm they pass**
  Run: `cargo nextest run -p openxet && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`.
  Expected: all pass, with no warnings.

- [ ] **Step 5: Add a `## Commands` section to `AGENTS.md`**
  List the exact commands:
  - `cargo build`
  - `cargo nextest run --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo fmt --check`
  - `cargo deny check`

  Replace the "The workspace scaffold adds a **Commands** section…" line.

- [ ] **Step 6: Commit**
  `feat(server): add workspace and openxet version command with JSON output`

---

### Task 2: CI pipeline and branch protection

**Files:**
- Create: `.github/workflows/ci.yml`, `deny.toml`,
  `.github/dependabot.yml`
- Modify: `AGENTS.md` (Commands: mention that CI runs the same commands)

**Interfaces:**
- Consumes: Task 1 commands.
- Produces: required status check names `ci / rust (ubuntu-latest)`,
  `ci / rust (macos-latest)`, `ci / deny`, which later tasks add jobs
  alongside.

- [ ] **Step 1: Write `ci.yml`**
  - Triggers: `push` to `main`, `pull_request`.
  - Job `rust`: matrix `ubuntu-latest`, `macos-latest`. It runs
    `dtolnay/rust-toolchain` reading `rust-toolchain.toml`,
    `Swatinem/rust-cache`, `taiki-e/install-action` for nextest, then fmt
    check, clippy `-D warnings`, `nextest run --workspace`, and `cargo doc
    --no-deps` with `RUSTDOCFLAGS=-D warnings`.
  - Job `deny`: `EmbarkStudios/cargo-deny-action`.
  - Pin every action to a full commit SHA, with the version in a trailing
    comment.
- [ ] **Step 2: Write `deny.toml`**
  - Licenses allow-list: Apache-2.0, MIT, BSD-2/3-Clause, ISC,
    Unicode-3.0, Zlib, MPL-2.0.
  - Advisories: deny.
  - Bans: `multiple-versions = "warn"`.
  - Sources: crates.io only.

  Run `cargo deny check` locally. Expected: `advisories ok, bans ok,
  licenses ok, sources ok`.
- [ ] **Step 3: Add `dependabot.yml`** for `cargo`, `github-actions` and
  `pip` (`/compat`), weekly.
- [ ] **Step 4: Push on a branch, open a PR, and confirm all jobs pass**
  Run: `gh pr checks --watch`. Expected: every check green.
- [ ] **Step 5: Ask the user to confirm, then enable branch protection on
  `main`** via `gh api -X PUT repos/stubbi/openxet/branches/main/protection`:
  - required status checks from the Produces list, `strict: true`
  - require a pull request
  - dismiss stale reviews
  - no force pushes or deletions
  - `enforce_admins: false` until a second maintainer exists, so the sole
    maintainer isn't locked out; record that in `GOVERNANCE.md`, Task 3

  Verify with `gh api repos/stubbi/openxet/branches/main/protection --jq .required_status_checks.contexts`.
- [ ] **Step 6: Merge the PR**
  Commit message: `ci: add CI pipeline, cargo-deny and dependabot`

---

### Task 3: Governance, community and security documents

**Files:**
- Create: `GOVERNANCE.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`,
  `SECURITY.md`, `NOTICE`, `.github/ISSUE_TEMPLATE/bug.yml`,
  `.github/ISSUE_TEMPLATE/feature.yml`, `.github/ISSUE_TEMPLATE/config.yml`,
  `.github/pull_request_template.md`, `.github/workflows/dco.yml`
- Modify: `README.md` (links section)

**Interfaces:**
- Produces: the DCO requirement (all later commits carry `Signed-off-by`;
  `git commit -s`), and the review rule later tasks follow.

- [ ] **Step 1: Write `GOVERNANCE.md`** (design §16):
  - roles: maintainer, committer, contributor
  - how decisions are made: lazy consensus, ADRs for architecture, RFCs in
    `docs/rfcs/` for big changes
  - the path to maintainer
  - the review rule: every change lands via PR with at least one independent
    review (human, or agent reviewer plus maintainer sign-off)
  - the current single-maintainer status and the commitment to recruit
    maintainers before 1.0
- [ ] **Step 2: Write `CONTRIBUTING.md`:**
  - dev setup using exactly the `AGENTS.md` Commands
  - the DCO (`git commit -s`), Conventional Commits and the PR checklist
    (tests, docs, ADR)
  - a pointer to `AGENTS.md` for agents
  - response-time commitment: first response within two business days
- [ ] **Step 3: `CODE_OF_CONDUCT.md`:** the Contributor Covenant 2.1,
  verbatim from https://www.contributor-covenant.org/version/2/1/code_of_conduct/,
  with contact `conduct@openxet.com`.
  **`SECURITY.md`:**
  - private reporting via GitHub Security Advisories, plus
    `security@openxet.com`
  - acknowledge within 2 business days, fix-or-plan within 14 days
  - supported versions table: "pre-1.0: latest `main` only"

  **Ask the user** whether the two `@openxet.com` mailboxes exist or should
  be created; until confirmed, use GitHub-only channels.
- [ ] **Step 4: Write `NOTICE`.** It says the work is licensed Apache-2.0 and
  includes a dependency notice for `xet-core` (Copyright Hugging Face,
  Apache-2.0).
- [ ] **Step 5: Add issue forms and the PR template.**
  - The bug form asks for: `openxet version --json` output, client and
    version, target (self-hosted or openxet.com), human or agent caller.
  - The PR template is a checklist: tests, docs, ADR if a decision changed,
    `git log` checked for prior decisions.
- [ ] **Step 6: Add `dco.yml`,** a DCO check on PRs. Pin the action by SHA.
  Verify on a PR whose commit lacks `Signed-off-by`: the check fails.
- [ ] **Step 7: Commit (signed off) and merge via PR**
  Commit message: `docs: add governance, contributing, conduct and security policies`

---

### Task 4: `openxet-protocol` with golden tests against HF's reference files

**Files:**
- Create:
  - `crates/protocol/Cargo.toml` (package `openxet-protocol`)
  - `crates/protocol/src/lib.rs`, `crates/protocol/src/hash.rs`,
    `crates/protocol/src/xorb.rs`, `crates/protocol/src/shard.rs`
  - `crates/protocol/tests/reference_files.rs`
  - `scripts/fetch-reference-files.sh`, `testdata/xet-spec-reference/SHA256SUMS`
  - `docs/research/S5-xet-core-structures-server-side.md`
- Modify: `.gitignore` (ignore `testdata/xet-spec-reference/*` except
  `SHA256SUMS`), `.github/workflows/ci.yml` (run the fetch script before
  tests; cache by `SHA256SUMS` hash), `NOTICE` if new upstream crates appear

**Interfaces:**
- Produces, in the `openxet_protocol` crate. These are the only protocol
  entry points later milestones may use; anything else from
  `xet-core-structures` is added here first.
  - `pub use xet_core_structures::merklehash::MerkleHash;`
  - `pub fn hash_to_hex(h: &MerkleHash) -> String` (spec "Converting Hashes
    to Strings")
  - `pub fn hash_from_hex(s: &str) -> Result<MerkleHash, ProtocolError>`
  - `pub struct XorbChunks { pub hashes: Vec<(MerkleHash, u64)> }`
  - `pub fn read_xorb_chunks(r: impl Read) -> Result<XorbChunks, ProtocolError>`
    (deserialize and hash every chunk)
  - `pub fn xorb_hash(chunks: &XorbChunks) -> MerkleHash`
  - `pub fn file_hash(chunks: &[(MerkleHash, u64)]) -> MerkleHash`
  - `pub fn range_hash(chunk_hashes: &[MerkleHash]) -> MerkleHash`
  - `pub enum ProtocolError` (thiserror). Variants: `InvalidHex`,
    `MalformedXorb`, `MalformedShard`.

- [ ] **Step 1: Write `scripts/fetch-reference-files.sh`**
  - Uses `curl -fL` from
    `https://huggingface.co/datasets/xet-team/xet-spec-reference-files/resolve/c4aa3a3f15b1395fff5ce934784bf6c8f2d62de8/<file>`
    into `testdata/xet-spec-reference/`. Files: every file listed in the
    dataset except `.gitattributes`.
  - Then it runs `shasum -a 256 -c SHA256SUMS`, exiting non-zero on any
    mismatch.
  - Generate `SHA256SUMS` from the first successful download, and confirm
    the CSV and xorb lines equal the Global Constraints values before
    committing it.

- [ ] **Step 2: Write the failing golden tests** in `tests/reference_files.rs`.
  A helper `fixture(name) -> PathBuf` panics with `"missing reference file
  {name}: run scripts/fetch-reference-files.sh"` if absent.

```rust
#[test] fn hex_round_trip_matches_spec_example() {
    // spec api page: bytes 0..32 render as this string
    let bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
    let h = MerkleHash::from(bytes);
    assert_eq!(hash_to_hex(&h), "07060504030201000f0e0d0c0b0a0908171615141312111f1e1d1c1b1a1918");
    assert_eq!(hash_from_hex(&hash_to_hex(&h)).unwrap(), h);
}
#[test] fn reference_xorb_hash_matches_its_name() {
    let chunks = read_xorb_chunks(File::open(fixture("eea25d6e…7632.xorb")).unwrap()).unwrap();
    assert_eq!(hash_to_hex(&xorb_hash(&chunks)), "eea25d6ee393ccae385820daed127b96ef0ea034dfb7cf6da3a950ce334b7632");
}
#[test] fn reference_xorb_chunks_match_chunks_listing() { /* every (hash,len) equals the `.xorb.chunks` file, in order */ }
#[test] fn reference_file_hash_matches() {
    // file_hash over the xorb's chunks == contents of `.csv.xet-file-hash`
    // == "118a53328412787fee04011dcf82fdc4acf3a4a1eddec341c910d30a306aaf97"
}
#[test] fn reference_range_hash_matches() { /* range_hash(all chunk hashes) == `.xorb.range-hash` contents */ }
#[test] fn single_chunk_files_hash_to_their_names() { /* compute_data_hash of each `<hash>.chunk` equals <hash> */ }
#[test] fn missing_fixture_fails_loudly() {
    let r = std::panic::catch_unwind(|| fixture("does-not-exist"));
    assert!(r.is_err());
}
```

  Use the full file names, not the shortened forms above.
  - Parse the `.chunks` listing format: inspect the file and document its
    format in a comment, with the spec link.

- [ ] **Step 3: Run the tests and confirm they fail**
  Run: `scripts/fetch-reference-files.sh && cargo nextest run -p openxet-protocol`.
  Expected: compile errors (functions not defined).

- [ ] **Step 4: Implement `crates/protocol`** as thin wrappers over
  `xet-core-structures =1.7.0`:
  - `merklehash::{MerkleHash::hex, from_hex, xorb_hash, file_hash, compute_data_hash}`
  - `xorb_object::deserialize_chunks`. The returned `chunk_byte_indices`
    start with a leading `0` (upstream doc comment).
  - `metadata_shard::chunk_verification::range_hash_from_chunks`

  No reimplementation. If an upstream function isn't public, stop and
  record it in S5 (Step 6) instead of copying code.

- [ ] **Step 5: Run the tests and confirm they pass**
  Same command. Expected: 7 passed.
  Then run `cargo tree -p openxet-protocol -e normal --depth 1`.

- [ ] **Step 6: Write `docs/research/S5-xet-core-structures-server-side.md`**
  Answer:
  1. What `xet-core-structures` pulls in (notably `xet-runtime`): binary-size
     and compile-time deltas, measured.
  2. Whether shard parsing and serialization for the upload path (including
     footer-less shards) and dedup-response building (shard **with footer**,
     HMAC-keyed hashes, competitive-matrix lesson 6) are reachable through
     the public API. List each needed function and whether it is public.
  3. Async and runtime assumptions that conflict with a tokio server.
  4. Semver behaviour: how often 1.x has broken API (checked against
     upstream's CHANGELOG and `api_changes/`).

  End with a recommendation. If anything must be reimplemented, propose ADR
  0002.

- [ ] **Step 7: Commit (signed off), via PR**
  Commit message: `feat(protocol): add openxet-protocol with reference-file golden tests`

---

### Task 5: Compatibility harness with an HF baseline and the agent leg

**Files:**
- Create:
  - `compat/pyproject.toml`, `compat/uv.lock`, `compat/conftest.py`
  - `compat/tests/test_baseline_download.py`, `compat/tests/test_agent_cli.py`
  - `compat/README.md`
  - `.github/workflows/compat-nightly.yml`
- Modify: `.github/workflows/ci.yml` (add job `compat (agent leg)`),
  `AGENTS.md` (Commands: `uv run --project compat pytest`)

**Interfaces:**
- Produces:
  - pytest fixtures `target` (`Target(endpoint: str, name: str)`),
    `hf_env` (a dict of env vars with an isolated `HF_HOME` under
    `tmp_path`), and `openxet_bin` (path to the built binary)
  - A report header (`pytest_report_header`) printing the target and the
    exact `huggingface_hub`, `hf_xet`, `git`, `git-lfs` and `git-xet`
    versions.

  Every later milestone adds tests here.

- [ ] **Step 1: Set up the harness**
  - `compat/pyproject.toml` pins `huggingface_hub[cli]==2.2.0`,
    `hf_xet==1.7.0`, `pytest` and `httpx`.
  - `conftest.py`:
    - Reads `OPENXET_COMPAT_TARGET` (default `https://huggingface.co`, name
      `huggingface.co`).
    - Never touches the real `~/.cache/huggingface`.
    - Fails if `OPENXET_BIN` isn't set or doesn't exist for tests marked
      `agent`.
- [ ] **Step 2: Write the failing tests**

```python
def test_xet_download_of_reference_csv_matches_sha256(target, hf_env):
    path = hf_hub_download("xet-team/xet-spec-reference-files",
        "Electric_Vehicle_Population_Data_20250917.csv", repo_type="dataset",
        revision="c4aa3a3f15b1395fff5ce934784bf6c8f2d62de8", endpoint=target.endpoint)
    assert sha256(path) == "f41255b252f776125f2b657136654e4d4d5d2ccf8ef4db0ec186bd1981b69734"

def test_resolve_exposes_xet_hash(target):
    r = httpx.head(f"{target.endpoint}/datasets/xet-team/xet-spec-reference-files/resolve/"
                   "c4aa3a3f15b1395fff5ce934784bf6c8f2d62de8/Electric_Vehicle_Population_Data_20250917.csv")
    assert r.headers["x-xet-hash"] == "118a53328412787fee04011dcf82fdc4acf3a4a1eddec341c910d30a306aaf97"

@pytest.mark.agent
def test_every_command_json_is_parseable_with_schema(openxet_bin):
    for args in (["version", "--json"],):
        out = subprocess.run([openxet_bin, *args], capture_output=True, check=True, stdin=subprocess.DEVNULL, timeout=30)
        assert json.loads(out.stdout)["schema"].startswith("openxet.")

@pytest.mark.agent
def test_never_prompts_without_tty(openxet_bin):
    out = subprocess.run([openxet_bin, "version"], capture_output=True, stdin=subprocess.DEVNULL, timeout=30)
    assert out.returncode == 0
```

  The command list is a module-level constant that every later CLI task must
  extend, so the agent leg grows with the CLI.
- [ ] **Step 3: Run and confirm they fail.**
  Run: `cd compat && uv run pytest -v`.
  Expected: failures (no fixtures or helpers).
- [ ] **Step 4: Implement `conftest.py`** and the `sha256` helper.
  Confirm the download really took the Xet path: assert that `hf_xet` wrote
  to `HF_XET_CACHE` (competitive-matrix lesson 1).
- [ ] **Step 5: Run and confirm they pass**
  Run: `cargo build && OPENXET_BIN=../target/debug/openxet uv run pytest -v`.
  Expected: 4 passed, with a header showing target `huggingface.co` and the
  versions.
- [ ] **Step 6: Add the CI jobs**
  - In `ci.yml`, job `compat (agent leg)` builds and runs `pytest -m agent`.
  - `compat-nightly.yml` runs the full suite against huggingface.co daily at
    04:00 UTC, plus a matrix leg with the *latest* `huggingface_hub`/`hf_xet`
    to catch upstream drift (design §8). It opens an issue on failure.
- [ ] **Step 7: Commit via PR**
  Commit message: `test(ci): add real-client compatibility harness and agent leg`

---

### Task 6: Spike S1 + S2 (direct presigned downloads, plain-HTTP endpoints)

**Question:**
- Does stock `hf_xet` 1.7.0 download correctly when `/v2/reconstructions`
  returns native S3 presigned URLs with **one range per fetch entry** and no
  `X-Xet-Signed-Range`?
- What does it do with `http://` CAS and Hub endpoints?

**Method:**
- In a scratch dir, write a ~150-line Python stub server (FastAPI). It
  serves `xet-read-token`, `resolve` HEAD (with `X-Xet-Hash`) and
  `/v2/reconstructions/<file>` for the reference file.
- The reference xorb is uploaded to MinIO (`docker run minio/minio`), and the
  stub presigns GET URLs with boto3.
- Drive it with `hf_hub_download` via the Task 5 harness, with
  `OPENXET_COMPAT_TARGET` pointing at the stub.
- Vary:
  - (a) one range per entry, presigned
  - (b) multiple ranges per entry, presigned (expected to fail on S3: record
    the error verbatim)
  - (c) `http` vs `https` for both endpoints
  - (d) URL expiry shorter than the download

**Output:**
- `docs/research/S1-S2-direct-downloads.md`, with versions, results per
  variant and verbatim errors.
- ADR `0002`, "Serve v2 reconstruction with presigned single-range URLs
  where the backend allows", or the alternative the evidence supports.
- Any `hf_xet` request/response captured as redacted fixtures in
  `compat/fixtures/s1/`.

**Done when:** each variant has an observed result, the ADR is accepted, and
the commit is merged via PR:
`docs(adr): 0002 direct download strategy from S1/S2`

---

### Task 7: Spike S3 (Hub API shapes captured from huggingface.co) and fixture redaction guard

**Prerequisite:** **ask the user** for an HF account or org dedicated to
testing (suggested: an `openxet-compat` org) and a write token. Store the
token only in the local env and as a GitHub Actions secret
`HF_COMPAT_TOKEN`, after the user confirms.

**Files:**
- Create: `compat/fixtures/hub/*.json`, `compat/tests/test_fixture_redaction.py`,
  `docs/research/S3-hub-api-shapes.md`

**Method:**
- Run `mitmdump` with a capture script (scratch) as an HTTPS proxy, trusting
  its CA only inside an isolated `HF_HOME` and venv.
- Drive the full §6 workflow list against the test org with
  `huggingface_hub` 2.2.0:
  - `hf auth login` / whoami
  - create a repo
  - `upload_folder` with a small and a 200 MB file
  - `create_commit` with a delete
  - branch/tag create/delete
  - `list_repo_tree`, `get_paths_info`, `list_repo_refs`,
    `list_repo_commits`, `model_info`
  - `hf_hub_download` and `snapshot_download`
  - a `bucket` repo round trip if the account has access to it
- Save each endpoint's request and response as JSON. Redact:
  `Authorization`, cookies, `X-Xet-Access-Token`, signed URL query strings,
  account and org names (replace with `openxet-compat`). Keep everything else
  byte-exact.

**Steps:**
- [ ] **Step 1: Write the failing redaction guard**
  `test_fixture_redaction.py` walks `compat/fixtures/` and fails if any file
  matches `hf_[A-Za-z0-9]{20,}`, `xet_[A-Za-z0-9]`, `Bearer [^R]`,
  `Signature=`, `X-Amz-Signature` or `Key-Pair-Id=`, outside the literal
  placeholder `REDACTED`. Seed one unredacted temporary fixture and check that
  it fails, then delete it.
- [ ] **Step 2: Capture, redact, and confirm the guard passes**
  Run: `uv run --project compat pytest compat/tests/test_fixture_redaction.py`.
  Expected: pass.
- [ ] **Step 3: Write `S3-hub-api-shapes.md`.**
  - One section per endpoint: method and path, the request fields the client
    sends, the response fields the client **reads**. Derive the second
    list by grepping `huggingface_hub` 2.2.0 source for each field, with
    `file:line`.
  - Status codes and error bodies the client handles.
  - Explicit notes for the competitive-matrix lessons 1–3, 7 and 12.
- [ ] **Step 4: Commit via PR**
  Commit message: `docs(hub): capture Hub API shapes from huggingface.co (S3)`

---

### Task 8: Spike S4 (DuckDB, polars and fsspec against a custom endpoint)

**Question:** can DuckDB `hf://`, polars `hf://` and `HfFileSystem` target a
non-huggingface.co endpoint? If not, what is the clean HTTPS route?

**Method:**
- Read the DuckDB httpfs source for the hf:// URL construction: the
  repository and file path at a pinned commit.
- Read the polars and `huggingface_hub` `HfFileSystem` endpoint handling.
- Test each against the Task 6 stub serving the reference CSV converted to
  Parquet.

**Output:**
- `docs/research/S4-analytics-clients.md`: one row per client: supports a
  custom endpoint (yes/no, how), with evidence (source `file:line` or a
  command output), and the fallback URL form.
- If DuckDB hard-codes the host, open a focused upstream issue or PR draft,
  and **ask the user** before submitting it.

**Done when:** every row has evidence and the note is merged via PR:
`docs(research): S4 analytics client endpoint support`

---

### Task 9: Spike S6 (shard upload verification rules) as executable tests

**Files:**
- Modify: `crates/protocol/src/shard.rs`, `crates/protocol/tests/reference_files.rs`
- Create: `docs/research/S6-shard-verification.md`

**Interfaces:**
- Produces:
  - `pub struct ShardFileEntry { pub file_hash: MerkleHash, pub segments: Vec<Segment>, pub sha256: Option<[u8;32]> }`
  - `pub struct Segment { pub xorb: MerkleHash, pub chunk_start: u32, pub chunk_end: u32, pub unpacked_bytes: u32, pub verification: Option<MerkleHash> }`
  - `pub fn read_upload_shard(r: impl Read) -> Result<UploadShard, ProtocolError>`,
    which accepts footer-less shards
  - `pub fn verify_segment(seg: &Segment, xorb_chunk_hashes: &[MerkleHash]) -> Result<(), VerifyError>`
  - `pub struct UploadShard { pub files: Vec<ShardFileEntry>, pub xorbs: Vec<MerkleHash> }`
  - `pub enum VerifyError { RangeHashMismatch, LengthMismatch, OutOfBounds, MissingVerification, FileHashMismatch }`
    (thiserror; each message names the xorb, the chunk range and the
    expected vs. actual value)

  M1's shard ingest builds on these.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn verification_shard_without_footer_parses_and_names_the_reference_file() {
    // `.shard.verification-no-footer` → one file entry, file_hash = 118a5332…af97, segments all in xorb eea25d6e…7632
}
#[test] fn every_reference_segment_verifies_against_the_reference_xorb() { /* verify_segment(seg, chunks) is Ok for all */ }
#[test] fn tampered_verification_hash_is_rejected() { /* flip one byte of seg.verification → Err(VerifyError::RangeHashMismatch) */ }
#[test] fn tampered_unpacked_bytes_is_rejected() { /* unpacked_bytes + 1 → Err(VerifyError::LengthMismatch) */ }
#[test] fn segment_out_of_xorb_bounds_is_rejected() { /* chunk_end > len → Err(VerifyError::OutOfBounds) */ }
#[test] fn file_hash_recomputes_from_segments() { /* file_hash(chunks referenced by segments) == entry.file_hash */ }
#[test] fn shard_without_verification_entries_is_rejected_for_upload() { /* the base `.shard` (no verification) → Err(VerifyError::MissingVerification) */ }
```

- [ ] **Step 2: Run and confirm they fail.** `cargo nextest run -p openxet-protocol`.
  Expected: compile errors.
- [ ] **Step 3: Implement** on top of `xet-core-structures` shard types,
  following S5's findings. The rules come from the spec "shard" and "upload
  protocol" pages: cite the section URL in a doc comment on
  `verify_segment`.
- [ ] **Step 4: Run and confirm they pass.** Expected: all pass.
- [ ] **Step 5: Write `S6-shard-verification.md`.**
  - Each server-side rule, its spec citation and the test enforcing it.
  - Gaps where the spec is silent, with the behaviour we chose.
  - The performance implication: verification needs stored per-xorb chunk
    hashes, never a re-download of xorbs. That's the Shardline lesson; feed
    it into M1's index design.
- [ ] **Step 6: Commit via PR**
  Commit message: `feat(protocol): verify upload shard segments against xorb chunk hashes`

---

### Task 10: Spike S7 (`git-lfs` / `git-xet` handshake, credentials, SSH)

**Prerequisite:** the Task 7 test org and token.

**Questions:**
1. The exact LFS batch request and response for upload and download against
   huggingface.co, with `git-xet` 0.2.1 and without it.
2. How `git-lfs` 3.5.1 and 3.7.x pick credentials when `lfs.url` is on
   another host than the Git remote, over HTTPS and SSH. Confirm the design
   §10.2 gap end to end against a local Gitea (`docker run gitea/gitea`) with
   `lfs.url` pointing at the Task 6 stub. Does `git-xet` send the Gitea
   credential to the stub?
3. Does a signed capability `href` with no `Authorization` header satisfy
   `git-xet`'s token refresh?
4. `git-lfs-authenticate` response format, and whether `git-lfs-transfer`
   (pure SSH) can negotiate the `xet` custom transfer.

**Method:** capture with `GIT_TRACE=1 GIT_CURL_VERBOSE=1 GIT_TRANSFER_TRACE=1`
and mitmproxy. Use an isolated HOME and repo-local config only.

**Output:**
- `docs/research/S7-git-lfs-xet.md`, with redacted fixtures in
  `compat/fixtures/lfs/` (guarded by Task 7's test).
- A draft upstream issue text for `huggingface/xet-core` about credential
  sourcing (design §10.2). **Ask the user** before filing it.
- ADR `0003` for the credential strategy.

**Done when:** all four questions are answered with evidence. Merge via PR:
`docs(adr): 0003 git-xet credential strategy from S7`

---

### Task 11: Spike S8 (HF pull-through mirror)

**Questions:**
- Which endpoints and headers a mirror must proxy or replicate for
  `hf_hub_download` / `snapshot_download` and Xet reconstruction to work
  transparently.
- Whether Xet reconstruction via a read token for a public repo can be
  proxied and cached (TTL, URL expiry).
- How guilt/xet-server's `xet-proxyd` does it (read its source at a pinned
  commit; note what to reuse conceptually).
- What HF's terms of service and content licences permit for caching and
  serving HF content to third parties, including on openxet.com. Quote the
  relevant clauses with links. Flag anything needing a lawyer.

**Output:** `docs/research/S8-hf-mirror.md`, with an explicit legal-risk
section for the user.

**Done when:** each question has an answer with a source. Merge via PR:
`docs(research): S8 HF pull-through mirror`

---

### Task 12: Spike S9 (Git hosting engine)

**Question:** in-process gitoxide (`gix` 0.90) or the `git` binary
(`git upload-pack` / `receive-pack` on a cached bare repo) for smart HTTP
and SSH, protocol v2, with packs stored in object storage and nodes kept
stateless?

**Method:** scratch prototypes of each, serving clone, fetch and push of:
- a 10k-commit repo (e.g. a mirror of `huggingface/transformers` history
  trimmed to 10k commits)
- a repo with LFS pointers

Measure:
- clone time, cold and warm
- push latency
- server CPU and memory
- correctness under concurrent pushes to the same branch (compare-and-swap)

Also list which server-side capabilities gix supports today (receive-pack,
protocol v2 serve), with source and doc links.

**Output:**
- `docs/research/S9-git-engine.md`, with a numbers table.
- ADR `0004`, "Git hosting engine".

**Done when:** the measurements exist for both options and the ADR is
accepted. Merge via PR:
`docs(adr): 0004 git hosting engine from S9`

---

### Task 13: Close M0: reconcile the design and write the M1 plan

**Files:**
- Modify: `docs/superpowers/specs/2026-10-23-openxet-design.md` (§3 verified
  facts, §11 spikes: mark each answered with links; adjust any section a
  spike changed), `docs/competitive-matrix.md` (add new lessons),
  `AGENTS.md` (Current state)
- Create: `docs/superpowers/plans/<date>-m1-cas-core.md`

- [ ] **Step 1: Update the design.** For every spike result that changes the
  design, edit the section and reference the ADR. A change that alters a
  user-made decision goes to the user before merging.
- [ ] **Step 2: Write the M1 plan** (CAS core on SQLite + local disk + static
  tokens, plus the client core as test driver), using the writing-plans skill
  with this plan's Interfaces as its starting point.
- [ ] **Step 3: Self-check.** `git log --oneline` shows one Conventional
  Commit per task, and CI is green on `main`.
- [ ] **Step 4: Commit via PR**
  Commit message: `docs: close M0, reconcile design with spike results, add M1 plan`
