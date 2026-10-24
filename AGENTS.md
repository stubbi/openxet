# AGENTS.md

Guidance for anyone, human or agent, changing this repository. Read it fully
before your first change. When something here is wrong or missing, fixing
it is part of your change.

## What openxet is

openxet is the open-source server and client for the Xet protocol, Hugging
Face's chunk-deduplicated storage for large, versioned files (models and
datasets are examples, not the definition). The official
tools (`hf`, `huggingface_hub`, `hf_xet`, `git-xet`, `git-lfs`,
`huggingface.js`) work against it unmodified. It runs on storage and identity
systems people already own, and as a hosted service at **openxet.com**.

The design is the source of truth for scope and decisions:
`docs/superpowers/specs/2026-10-23-openxet-design.md`. Architecture decisions
made since are in `docs/adr/`.

For the website, docs presentation and self-service UI, also read root
[`design.md`](design.md) before changing copy, navigation, styling or interaction.
It defines the shared language, visual tokens, component states and human/agent
experience. Reuse these choices. An intentional change to the language or visual
identity updates `design.md` in the same PR, with a reason and visual evidence.
Keep capability claims consistent with the current release; the hosted waitlist
is not an active hosted storage service.

## Who we build for

Every change should make at least one of these people's day better and none
of them worse:

- **Self-hosters:** teams that can't or won't put data on huggingface.co.
  They want one command to a working setup and nothing to babysit.
- **Platforms** (aqora first): they embed openxet's storage under their own
  product, naming and permissions.
- **openxet.com users:** they want the same thing without running anything.
  The hosted experience must be perfect: landing page to working `hf upload`
  in under 60 seconds, no dead ends, and visible dedup savings. openxet.com
  runs the released open-source build; there is no private fork. When the
  hosted service needs something, it is built into the core, behind the
  same seams self-hosters use.

Each of them may be a **human, an AI agent, or both working together**. Each
deserves the best possible experience; neither is a degraded mode of the
other (design §7.1):
- **Humans** get readable output, safe defaults and docs that teach.
- **Agents** get:
  - `--json` with versioned schemas
  - stable exit codes
  - structured errors with a hint for the next action
  - non-interactive modes, idempotency and `--dry-run`
  - OpenAPI, an MCP server, and `llms.txt`
- **Together** they get:
  - delegated tokens that are narrower than the human's own
  - audit entries that attribute each action
  - approval gates for destructive agent actions

Every new command, endpoint or error must work well for both. The agent leg
of the compatibility suite enforces this.

Think from their side. Do the extra round for them (a clearer error, a
faster path, a doc example that actually runs) rather than what is
convenient for us. Failure looks like this, even if the code works:
slow, hard to set up, or users' feedback going nowhere.

## The bar: best in class on every dimension

No other project should come remotely close, in protocol correctness,
performance, code quality, architecture, security, docs, governance,
community, release engineering, UX, or hosted operations. An ambition only
counts if it is measured: design §16 lists the bar and the proof for each
dimension, and `docs/competitive-matrix.md` tracks where we stand. When a
change would lower any of those measures, it isn't done.

## Non-negotiables

1. **Protocol exactness.** Chunking, hashes, xorbs, shards and API behaviour
   match the Xet spec and `xet-core` byte for byte. Data written by openxet is
   readable by stock clients and huggingface.co tooling, and vice versa. Any
   divergence is a release blocker.
2. **The bucket is the source of truth.** The database is a rebuildable
   index. Losing the database must never lose data.
3. **Nothing is deleted outside garbage collection,** which is explicit,
   logged, and has a grace period.
4. **openxet never owns users.** No passwords, profiles or sign-up database.
   Identity comes from the operator's systems. Our access tokens and SSH keys
   are always bound to an external identity.
5. **Stateless nodes.** Any node can serve any request; scale by adding
   nodes.
6. **No telemetry** unless the operator explicitly opts in.

## Security requirements

Design §5.9 lists them, each with the test that enforces it. Every change
keeps them true, and a change that weakens one is a release blocker:
- No unauthenticated reads when auth is on. Every data URL needs a
  credential or is a signed, range-bound URL.
- Credentials are repo-scoped on every route and signed URL. Repo creation
  is authorized per namespace.
- Caches and proxies are keyed by the caller's credential and fail closed.
- Dedup answers are HMAC-keyed and never expose raw chunk hashes.
- Uploaded xorbs and shards are verified before anything is registered,
  and verification errors never reveal server-derived values.
- Credentials are never echoed or logged.
- No silent no-op success; acknowledged writes survive a crash.
- Git reads check reachability, and every ref writer is fenced.

Security issues found in any third party (including Hugging Face's
clients) go privately to their maintainers; public text never describes an
unfixed third-party security weakness or hints at one (R21/R26).

## How we work

**Plan before building.** Non-trivial work starts from the design and a
written plan in `docs/superpowers/plans/`. If the work changes a decision,
write an ADR in `docs/adr/` first.

**Check history before changing anything.** We don't silently undo
directions already taken.

```bash
ls docs/adr/                       # decisions in force
git log --oneline -- <path>        # how this area evolved
git log --grep '<topic>' --oneline # related work packages
```

If your change reverses or bends an earlier decision, say so in the commit
message and update or supersede the ADR.

**Verify, don't assume.**
- Protocol behaviour is checked against the spec
  (https://huggingface.co/docs/xet), `xet-core`, `huggingface_hub` source and
  the reference files (`xet-team/xet-spec-reference-files` on the Hub).
- Client behaviour is checked by running the real client.
- Before writing a claim into code comments, docs or commit messages,
  confirm it. Note what you verified and against which version.
- Before building something, check prior art (other Xet servers exist;
  see the design §14 and `docs/adr/`).

**Test-driven.** Write the failing test first, see it fail, make it pass.
Tests must earn their place:
- Property tests for hashing and format math.
- Golden vectors from the reference files.
- Fuzzing for every parser of untrusted bytes.
- The compatibility suite against real clients. This is the most important
  test; it never gets weaker to make a change pass.

When behaviour changes, the tests change in the same commit. Delete tests
that no longer test anything real.

**Leave code better than you found it.** Improve what you touch: naming,
clarity, a missing test. Put unrelated cleanups in their own commit. Don't
start drive-by rewrites without a plan.

**Docs are part of the change.** User-facing behaviour, config, commands and
operator procedures are documented in the same change. Examples in docs run
in CI. A doc that drifts from behaviour is a bug. A new page under `docs/`
goes into `docs/SUMMARY.md`, the docs site's navigation.

## Quality bar: no slop

- No speculative abstractions, options or extension points that nothing uses
  yet. YAGNI, unless the design names the seam.
- No comments that narrate the code. Comment why, invariants and protocol
  references (spec section and URL), never what.
- No dead code, commented-out code, `TODO` without an issue link, or
  placeholder text.
- Errors are actionable: what happened, and what the user should do next.
  Never leak secrets in errors or logs.
- `unsafe` is forbidden unless an ADR justifies it.
- Match the surrounding code's style and idioms. Consistency beats personal
  preference.
- Small, focused files with one responsibility each. Prefer clarity over
  cleverness.
- Performance claims come with a benchmark. Speed regressions fail CI.

## Commits

Use Conventional Commits with a component scope, so `git log` reads per work
package:

```
feat(cas): verify shard verification entries against stored chunk hashes
fix(hub): return 501 naming the endpoint for unimplemented Hub routes
docs(adr): 0003 use xet-core-structures for xorb and shard parsing
```

Scopes: `protocol`, `cas`, `index`, `storage`, `transfer`, `hub`, `git`,
`ssh`, `auth`, `gc`, `web`, `client`, `server`, `hosted`, `site`, `compat`,
`ci`, `docs`, `adr`, `deps`. `web` is the self-service web UI; `site` is the
openxet.com landing page in `website/`; `compat` is the compatibility suite
in `compat/`.

One logical change per commit. Every commit builds and passes tests.

## Current state

M1 complete: the storage-only CAS (`openxet serve`, `upload`, `download`;
`docs/quickstart-storage-only.md`), with the compatibility suite's agent,
storage-only and docs legs and the reconstruction latency gate in CI
(`docs/reference/benchmarks.md`), and every CAS route in
`docs/reference/routes.md`. M2 next: the Hub API, Git and LFS, per
`docs/superpowers/plans/` once its plan is written. The design is approved,
and ADR 0001 decides to build from scratch on `xet-core`'s crates (after
evaluating existing Xet servers; see `docs/competitive-matrix.md`).

## Commands

Run from the repository root. The toolchain is pinned in
`rust-toolchain.toml`; `cargo nextest` needs `cargo install cargo-nextest
--locked` once. CI runs these same commands (`.github/workflows/ci.yml`), so
green locally means green there, except for the latency gate: timed, it
needs a quiet machine, and a p99 failure with steady p50s on a busy host is
load, not a regression. Keep this list exact and current. The
criterion benchmarks are for humans, not CI gates: record their results, with
the machine, in `docs/reference/benchmarks.md`.

```bash
cargo build                                                # build the workspace
scripts/fetch-reference-files.sh                           # fetch + verify golden-test fixtures (once)
cargo nextest run --workspace                              # run all tests
cargo nextest run --release -p openxet-protocol -E 'binary(file_hash_streaming)' # incremental file hash, release build (CI gate)
cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)' --no-capture # reconstruction p99 < 50 ms, release build, quiet machine (CI gate)
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
scripts/build-site.sh                                      # openxet.com, landing + docs/, into target/site/public; needs mdbook 0.5.4, Python 3 and Node.js 22+ (CI: docs-site; main deploys it)
mdbook serve site --open                                   # live preview of the docs site while editing docs/
npm test --prefix hosted/waitlist                           # waitlist handler/store tests; Node 26.7.0, no dependency install (CI: hosted waitlist)
```

Exit codes and JSON output schemas are documented in `docs/reference/`.
