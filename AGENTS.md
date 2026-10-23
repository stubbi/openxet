# AGENTS.md

Guidance for anyone, human or agent, changing this repository. Read it fully
before your first change. When something here is wrong or missing, fixing
it is part of your change.

## What openxet is

openxet is the open-source server and client for the Xet protocol, Hugging
Face's chunk-deduplicated storage for large, versioned files. The official
tools (`hf`, `huggingface_hub`, `hf_xet`, `git-xet`, `git-lfs`,
`huggingface.js`) work against it unmodified. It runs on storage and identity
systems people already own, and as a hosted service at **openxet.com**.

The design is the source of truth for scope and decisions:
`docs/superpowers/specs/2026-10-23-openxet-design.md`. Architecture decisions
made since are in `docs/adr/`.

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
in CI. A doc that drifts from behaviour is a bug.

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
`ssh`, `auth`, `gc`, `web`, `client`, `server`, `hosted`, `ci`, `docs`,
`adr`, `deps`.

One logical change per commit. Every commit builds and passes tests.

## Current state

M0 foundation in progress, following
`docs/superpowers/plans/2026-10-23-m0-foundation.md`. The design is approved,
and ADR 0001 decides to build from scratch on `xet-core`'s crates (after
evaluating existing Xet servers; see `docs/competitive-matrix.md`).

## Commands

Run from the repository root. The toolchain is pinned in
`rust-toolchain.toml`; `cargo nextest` needs `cargo install cargo-nextest
--locked` once. CI runs these same commands (`.github/workflows/ci.yml`), so
green locally means green there. Keep this list exact and current; benchmarks
are added when they exist.

```bash
cargo build                                                # build the workspace
scripts/fetch-reference-files.sh                           # fetch + verify golden-test fixtures (once)
cargo nextest run --workspace                              # run all tests
cargo clippy --workspace --all-targets -- -D warnings      # lint (CI gate)
cargo fmt --check                                          # formatting (CI gate)
cargo deny check                                           # licenses, advisories, bans
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps # docs build (CI gate)
OPENXET_BIN=$PWD/target/debug/openxet uv run --directory compat pytest # compatibility suite (needs uv; CI gate: -m agent)
```

Exit codes and JSON output schemas are documented in `docs/reference/`.
