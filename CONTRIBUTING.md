# Contributing to openxet

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
(`.github/workflows/ci.yml`), so green locally means green there.

```bash
cargo build                                                # build the workspace
scripts/fetch-reference-files.sh                           # fetch + verify golden-test fixtures (once)
cargo nextest run --workspace                              # run all tests
cargo nextest run --release -p openxet-protocol -E 'binary(file_hash_streaming)' # incremental file hash, release build (CI gate)
cargo clippy --workspace --all-targets -- -D warnings      # lint (CI gate)
cargo fmt --check                                          # formatting (CI gate)
cargo deny check                                           # licenses, advisories, bans
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps # docs build (CI gate)
uv run --frozen --directory compat pytest tests/test_fixture_redaction.py tests/test_harness.py -m "not pinned" # redaction guard + harness isolation (CI gate)
OPENXET_BIN=$PWD/target/debug/openxet uv run --frozen --directory compat pytest -m agent # agent leg (CI gate)
```

The full compatibility suite (`pytest` without `-m agent`) talks to
huggingface.co, and CI runs it nightly.

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
