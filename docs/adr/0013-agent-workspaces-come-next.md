# 0013: Build agent workspaces after M2, before the rest of the milestones

<!-- description: Agent workspaces (design of 2026-10-27) come right after M2 and ahead of M3 to M8; steps 1 to 3 need no server, and M3's backends, OIDC and issued tokens interleave before the hosted and multi-tenant steps. -->

- Status: accepted
- Date: 2026-10-28
- Scope: `client`, `storage`, `auth`, `gc`, `docs`
- Supersedes: the proposed ADR 0013 of
  [PR #103](https://github.com/stubbi/openxet/pull/103), "Prioritize
  shared working storage for agents", which never merged. This ADR takes
  its number.

## Context

Design §12 orders the work M0 to M8 and builds M0 to M7 straight to
`1.0`. After M2, the next milestone would be M3: production backends,
OIDC and the webhook authorizer, issued tokens and rate limits.

The owner's own machine shows a need that can't wait for M6's client.
Coding agents work in parallel, each in a Git worktree with its own build
outputs. On 2026-10-25 six parallel builds filled a 460 GB disk and
stalled every agent; on 2026-10-26 one repository held 418 agent worktrees
in 87 GB ([agent workspaces design](../superpowers/specs/2026-10-27-agent-workspaces-design.md)
§1). The owner approved that design on 2026-10-27 and decided "finish M2
first, then agent-first" (§3). M2 completed on 2026-10-27.

Codex proposed a direction for the same problem in PR #103: a vision
document, a draft of this ADR and the feasibility audit
[S15](../research/S15-agent-storage-feasibility.md). The approved design
builds on all three and supersedes the vision where they differ (§11).
What this ADR carries over from Codex's draft, each checked against the
approved design:
- Repositories and buckets stay the only storage primitives (ADR 0010);
  workspaces, snapshots and forks are built from them. The Xet wire
  format and the `xet://` contract do not change.
- Measure before building, and keep active writes on native local disk
  until a measurement shows a mount meets the bar (design §6.3).
- Release no local work without a durable snapshot first (design §6.1 and
  §6.2a; S15 gate F5).
- No hard egress cap is promised while signed URLs can be replayed; that
  waits for a mediated download path (design §6.6; S15 "Hard byte caps
  and direct downloads are different contracts").
- No M3 to M7 deliverable leaves the `1.0` scope.

S15's feature register and its gates F1 to F6 still apply: each names
the proof a workspace feature needs before it is promised.

## Decision

1. **Agent workspaces come next.** The roadmap of the agent workspaces
   design (§9) starts after M2 and ahead of the remaining milestones of
   design §12.
2. **Steps 1 to 3 come before M3.** Measure (step 1), the local store and
   workspace provider (step 2) and the mount (step 3) run on one machine
   against a local store, or a local or self-hosted `openxet serve`. They
   need no production backend, OIDC or issued tokens, so nothing in M3
   blocks them.
3. **M3 interleaves before the hosted and multi-tenant promises of steps
   4 to 6.** Durable snapshots and handoffs (step 4), bucket snapshots and
   forks (step 5) and limits at scale (step 6) can be built and tested
   against a self-hosted server first. Their promises for openxet.com and
   for many tenants wait on M3's production backends, OIDC and issued
   tokens. Step 5 also needs M5's bucket primitive and its own ADR first;
   step 6 builds on M3's delegated tokens and M4's audit log.
4. **Nothing is dropped.** Every deliverable of M3 to M8 stays in scope.
   Each workspace plan in `docs/superpowers/plans/` says which milestone
   work it runs before or after.
5. **Each step ends with its gate green**, as the agent workspaces design
   §2 and §9 define them: step 2's flat-disk gate, step 3's build bar.

## Consequences

- M3 to M8 start later than design §12's order implied. The release cut is
  unchanged: `1.0` still needs all of M0 to M7.
- Step 3's mount brings part of M6's `mount` forward. M6 keeps the rest of
  its client scope (`sync`, `login`, `setup git`, `doctor`, Python
  bindings).
- Workspace commands follow design §7.1 like every command: `--json`,
  stable exit codes, structured errors with a hint, `--dry-run`. Their MCP
  tools wait for `openxet mcp` (M6) and are tracked in an issue, not
  skipped.
- The workspace provider is client code, so the client-core ban (ADR 0006,
  `scripts/check-client-core-ban.sh`) keeps applying to server crates.
- `AGENTS.md` "Current state", design §12 and the agent workspaces design
  §9 state this order, and change together if it changes.

## Alternatives considered

- **M3 next, as design §12 ordered.** It finishes the server for hosted
  and multi-tenant use first, but leaves the measured disk problem until
  after M6, while steps 1 to 3 need none of M3.
- **Codex's draft (PR #103) as written.** It brings artifact handoffs and
  a reusable reader forward first and treats workspace management as a
  later phase. The approved design starts from the disk problem the owner
  measured, which handoffs don't solve, and keeps the draft's safeguards
  listed above.
- **A writable remote filesystem first.** It takes on small-file,
  metadata and platform risk before measuring whether native disk with
  copy-on-write clones already meets the bar (S15; design §6.3).
