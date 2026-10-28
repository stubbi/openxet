# 0014: Design for agents as the primary daily user, and humans as the ones who set limits

<!-- description: Day to day the agent is OpenXet's user; the human sets agents loose and sets their limits, and keeps every guarantee of design section 7.1: readable output, safe defaults, limits, audit and approvals. -->

- Status: accepted
- Date: 2026-10-28
- Scope: `client`, `hub`, `auth`, `web`, `docs`, `compat`
- Amends: design §7.1

## Context

Design §7.1 builds OpenXet for people and AI agents equally: "each gets
the best possible experience, and neither is a degraded mode of the
other". It does not say whose path comes first when one workflow has to
be designed around one of them.

The agent workspaces design (approved by the owner on 2026-10-27, §1)
gives the roles in the owner's words: the human "sets agents loose and sets
limits". Day to day, the agent is the user. The human's surface is
control: budgets, permissions, approvals, audit and revocation. The human
does not browse files. Its §5 makes "a degraded human" an anti-goal, and
its §10 asks for an ADR that amends §7.1 and keeps every human guarantee.

The evidence for the priority is the workload: the disk problem the
workspaces design starts from comes from agents running in parallel
without a human at each step (418 worktrees in one repository, §1), and
nobody knew which copies were safe to delete.

## Decision

1. **Agents are the primary daily user.** Each daily workflow (create a
   workspace, read, write, upload, download, snapshot, hand off) is
   designed first as an agent runs it: non-interactive, `--json`,
   structured errors with a hint, stable exit codes, and an MCP tool. An
   agent completes it within its limits with no human present: no prompt
   that blocks, no browser step, no manual cleanup afterwards.
2. **The human sets agents loose and sets limits.** The human's surface
   is control: delegated tokens and their scopes, expiry, quotas, disk
   and workspace budgets, approval gates, the audit log and revocation.
   That surface gets the same care as the agent's daily path, in the CLI
   and in the web UI.
3. **Limits bind agents, not the other way round.** An agent can never
   widen its own limits, approve its own gated action or extend its own
   token. Only a human, or a credential the human holds, can.
4. **Every human guarantee of §7.1 stays.** Readable output, progress
   and helpful prompts when a human runs a command; safe defaults with
   confirmation before destructive actions; the web UI and docs that
   teach; delegated, attenuated tokens; attribution of every action;
   approval gates; and handoff between human and agent. A human who runs
   any daily workflow directly gets the experience §7.1 describes.
5. **When the two pull apart in a daily workflow, the agent's path
   decides the design**, unless it would weaken a human guarantee in
   item 4 or a limit in item 3. Those hold whatever the agent's
   convenience.

## Consequences

- Design §7.1 points here. Its sentence "neither is a degraded mode of
  the other" stays true: the human is not degraded, the human's main job
  changes.
- Docs lead each daily workflow with the agent's invocation and its JSON
  output, and keep a human walkthrough beside it. Guides for the control
  surface (tokens, budgets, approvals, audit) lead with the human.
- The compatibility suite's agent leg remains the gate for item 1: it
  fails on any prompt, unparseable output or unstable schema. Each new
  command gets an entry in `compat/tests/test_agent_cli.py`.
- The web UI's first priority is the control surface. File browsing in
  design §10.10 stays in scope but is not where human effort goes first.
- Item 3 is a security property: tests that an agent token can't raise
  its own limits or approve its own action belong in the security test
  map with the M3 and M6 work that adds those limits and gates.

## Alternatives considered

- **Keep §7.1's equal framing unchanged.** It gives no rule for the
  workflows where the two pull apart, so each one would be argued again.
  The owner's direction already answers it.
- **Agents only, with a minimal human surface.** It would drop guarantees
  §7.1 makes (readable output, confirmation before destructive actions,
  docs that teach) and make the human the degraded mode the workspaces
  design rules out.
