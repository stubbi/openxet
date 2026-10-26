# Contributing to OpenXet

Thanks for helping. Humans and AI agents are equally welcome. Agents should
also read [`AGENTS.md`](AGENTS.md), which is the full working agreement;
humans benefit from reading it too.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).
Report security issues privately, following [SECURITY.md](SECURITY.md).

## Response times

We aim to give a first response to every issue and PR within **two business
days**. If you get none, comment again; that is our miss, not yours.

## Dev setup

Install Rust through [rustup](https://rustup.rs); the toolchain is pinned in
`rust-toolchain.toml`. `cargo nextest` needs `cargo install cargo-nextest
--locked` once, and `cargo deny` needs `cargo install cargo-deny --locked`.
The compatibility suite needs [uv](https://docs.astral.sh/uv/), which
installs the pinned Python and HF clients from `compat/uv.lock`.
Run from the repository root. CI runs these same commands
(`.github/workflows/ci.yml`), so green locally means green there, except
for the latency gate: timed, it needs a quiet machine, and a failure on a
busy host, p99 first and p50s too under heavy load, is load, not a
regression. On a pull request CI compares the gate with the base measured
on the same runner (ruling M2-R17,
[`docs/reference/benchmarks.md`](docs/reference/benchmarks.md)). The criterion
benchmarks are for humans, not CI gates: record their results, with the
machine, in [`docs/reference/benchmarks.md`](docs/reference/benchmarks.md).

```bash
cargo build                                                # build the workspace
scripts/fetch-reference-files.sh                           # fetch + verify golden-test fixtures (once)
cargo nextest run --workspace                              # run all tests
cargo nextest run --release -p openxet-protocol -E 'binary(file_hash_streaming)' # incremental file hash, release build (CI gate)
cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)' --no-capture # reconstruction p99 < 50 ms, release build, quiet machine (CI gate, absolute on main)
OPENXET_LATENCY_GATE=measure OPENXET_LATENCY_GATE_BUILD=$(git rev-parse HEAD) cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)' --no-capture # the same, printing the JSON lines without asserting (CI, on a pull request: base, then head)
python3 -I scripts/check-latency-gate.py --base base.json --head head.json --head-build <commit> # a PR's gate run against its base's, ruling M2-R17 (CI gate on a pull request)
python3 -I scripts/test_check_latency_gate.py              # the comparison script's tests (CI gate)
cargo bench -p openxet-cas --bench reconstruction          # reconstruction and URL-signing benchmarks
cargo bench -p openxet-cas --bench xorb_ingest             # xorb ingest on a 64 MiB LZ4 xorb
cargo bench -p openxet-cas --bench commit_throughput      # durable shard and index commits per second on local disk
cargo clippy --workspace --all-targets -- -D warnings      # lint (CI gate)
cargo fmt --check                                          # formatting (CI gate)
cargo deny check                                           # licenses, advisories, bans (CI gate)
scripts/check-client-core-ban.sh                           # the ADR 0006 ban rejects xet-data & co. in a server crate (CI gate)
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps # docs build (CI gate)
fuzz/seed.sh                                               # fill fuzz/corpus and rewrite fuzz/Cargo.lock from Cargo.lock (before fuzzing; commit the lock; needs the reference files)
cargo +nightly fuzz run xorb_upload -- -max_total_time=60  # fuzz a parser 60 s (CI: every target in fuzz/Cargo.toml)
cargo deny --manifest-path fuzz/Cargo.toml check           # the fuzz workspace's own dependencies (CI, with the fuzz job)
uv run --frozen --directory compat pytest tests/test_fixture_redaction.py tests/test_harness.py -m "not pinned" # redaction guard + harness isolation (CI gate)
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m agent        # agent leg (CI gate)
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m storage_only # stock hf_xet against openxet serve (CI gate)
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m docs         # docs/quickstart-storage-only.md runs as written (CI gate)
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest                 # all of it, plus the HF baseline against huggingface.co (nightly)
cargo nextest list --workspace --message-format json > /tmp/tests.json && python3 -I scripts/check-security-tests-map.py --nextest /tmp/tests.json # security test map vs the Rust tests that run (CI gate)
uv run --frozen --directory compat pytest --collect-only -q > /tmp/compat-tests.txt && python3 -I scripts/check-security-tests-map.py --pytest /tmp/compat-tests.txt # the same for the compat tests (CI gate)
scripts/build-site.sh                                      # openxet.com, landing + docs/, into target/site/public; needs mdbook 0.5.4 (CI: docs-site; main deploys it)
mdbook serve site --open                                   # live preview of the docs site while editing docs/
```

Fuzzing needs the nightly toolchain (`rustup toolchain install nightly`) and
`cargo install cargo-fuzz --locked` once. `fuzz/` is its own workspace, and
`fuzz/seed.sh` copies the seeds from `testdata/` and writes the text
targets' seeds; the corpus and any crash artifacts stay untracked. It also
rewrites `fuzz/Cargo.lock` from the workspace's `Cargo.lock`, so fuzzing
runs the dependency versions OpenXet ships; after changing `Cargo.lock`,
run it and commit `fuzz/Cargo.lock` too, or `fuzz-smoke` fails. On macOS the default AddressSanitizer build fails to
link, so add `-s none` (`cargo +nightly fuzz run -s none <target>`); Linux and
CI use the default. A crash input becomes a regression test in the
crashed crate's tests together with its fix. CI's `fuzz-smoke` job runs
each target for 60 s on PRs that touch a crate the fuzz workspace builds
(`crates/protocol/`, `auth/`, `cas/`, `index/` or `storage/`), `fuzz/` or
`Cargo.lock`.

The legs marked `agent`, `storage_only` and `docs` need only the built
`openxet`. The rest of the compatibility suite talks to huggingface.co, and
CI runs it nightly (`.github/workflows/compat-nightly.yml`) with the pinned
and the newest client releases.

## Before you start

- For anything non-trivial, open an issue first so we can agree on the
  approach. Big changes need an RFC (see [GOVERNANCE.md](GOVERNANCE.md)).
- Check history so we don't undo earlier decisions: `ls docs/adr/`,
  `git log --oneline -- <path>`, `git log --grep '<topic>' --oneline`.
- Write the failing test first, then make it pass.

## Sign off your commits (DCO)

Every commit needs a `Signed-off-by` line certifying the
[Developer Certificate of Origin](https://developercertificate.org/). Use
`git commit -s`. If you forgot, fix the last commit with
`git commit --amend -s --no-edit`, or a whole branch with
`git rebase --signoff main`, then force-push. The `DCO` check on PRs fails
without it. Pull requests opened by Dependabot are exempt.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/) with a
component scope, for example `fix(hub): return 501 naming the endpoint for
unimplemented Hub routes`. The scopes and the one-logical-change rule are in
`AGENTS.md`.

## Pull request checklist

The PR template repeats this list.

- [ ] Tests added or updated in the same change, and they fail without it.
- [ ] Docs updated for any user-facing behaviour, config or command.
- [ ] ADR written or updated if you changed a decision.
- [ ] Checked `git log` and `docs/adr/` for prior decisions in this area.
- [ ] All commands above pass locally.
- [ ] Every commit is signed off and follows Conventional Commits.
