# Governance

openxet is meant to be trustworthy beyond any one person. This document says
who decides what, how, and what is true today. The bar behind it is design
§16 (`docs/superpowers/specs/2026-10-23-openxet-design.md`).

## Current status

openxet has **one maintainer**, [@stubbi](https://github.com/stubbi). That is
a bus-factor risk, and we treat it as one:

- We will recruit additional maintainers, and aim to have at least two
  before 1.0.
- Once a second maintainer exists, the `main` branch protection rises from
  zero to **one approving human review**, and this document is updated in
  the same change.

## Roles

| Role | Who | Can |
|---|---|---|
| Contributor | Anyone | Open issues and PRs, review, join discussions. Every commit carries a DCO sign-off. |
| Committer | Contributors with a sustained record in an area, nominated by a maintainer | Approve and merge PRs in that area. |
| Maintainer | Committers trusted with the whole project | Merge anywhere, cut releases, change governance, enforce the [Code of Conduct](CODE_OF_CONDUCT.md), handle security reports. |

Today there are no committers other than the maintainer.

## Path to maintainer

1. Contribute: merged PRs, reviews and issue triage over several months.
2. A maintainer nominates you as committer for an area after you show good
   judgement there. You must be someone who has read and follows
   [`AGENTS.md`](AGENTS.md).
3. After a sustained record across areas, a maintainer proposes you as
   maintainer in a public issue. Existing maintainers decide by consensus
   and the discussion stays open for at least seven days.

A maintainer is **inactive** after six months with no merged PR, review or
public governance comment in the project. An inactive maintainer may step
down to committer by their own choice or by agreement of the other
maintainers, and doesn't count toward the active maintainers below.

## How decisions are made

- **Lazy consensus.** A proposal in an issue or PR passes if nobody with
  standing objects within a reasonable time (at least three business days
  for non-trivial changes). Objections must be technical and specific.
- **ADRs** (`docs/adr/`) record every significant architecture decision. A
  change that reverses or bends one supersedes it in the same PR.
- **RFCs** (`docs/rfcs/`) are for big changes: new subsystems, protocol or
  API surface, or anything affecting compatibility or governance. Open a PR
  adding the RFC; it is accepted by consensus among maintainers after at
  least seven days of public discussion.
- **Tie-breaks.** If consensus fails, the maintainers decide by majority.
  While there is one maintainer, that maintainer decides.

## Review rule

Every change lands through a pull request with at least one independent
review: a human, or an agent reviewer plus maintainer sign-off.

What branch protection enforces on `main` today:

- A pull request is required; no direct pushes.
- Required checks must pass: `rust (ubuntu-latest)`, `rust (macos-latest)`,
  `deny`, `compat (agent leg)`, `compat (storage-only)` and `DCO`.
- The nightly `compat-nightly` workflow (the newest HF client releases and
  `hf-xet` pre-release) is not a PR check: upstream can move without any
  change of ours. A failure opens or updates an issue.
- `fuzz-smoke` (a 60 s run of each parser fuzz target) is deliberately not
  required: it needs the nightly toolchain, so a nightly change can fail it
  without any change of ours, and a time-boxed run is not a proof. A failure
  is still treated as a bug to fix before merging.
- History is linear (squash merges only), and every review conversation
  must be resolved before merging.
- **Zero required approving reviews**, and administrators are not bound by
  these rules (`enforce_admins: false`). With one human maintainer, a
  required approval would only block the project or be self-approval.

So today independent review is done by agent reviewers, with the maintainer
signing off, and that part is a convention, not something GitHub enforces.
We say so plainly rather than overstate the guarantee.

## Changing this document

By PR, under the same decision process. Changes to roles or the review rule
need the approval of every active maintainer while there are three or fewer.
Above three, they need two-thirds of the active maintainers; a maintainer
who hasn't answered 14 days after being asked in the PR counts as
consenting.
