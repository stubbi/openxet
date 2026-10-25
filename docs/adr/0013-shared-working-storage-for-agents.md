# 0013: Prioritize shared working storage for agents

<!-- description: Proposed product direction: preserve Xet compatibility, deliver measured client workflows earlier, and add artifact handoffs and durable workspace restoration above the existing storage primitives. -->

- Status: proposed
- Date: 2026-10-25
- Scope: `client`, `transfer`, `auth`, `gc`, `hub`, `docs`, `hosted`

## Context

The owner asks for a concrete vision and roadmap for OpenXet when agents
operate computers and internet services. The use cases include selective
data access, fast mounts, disk pressure from many local workspaces, and
handoffs between agents. The approved design already provides a strong
storage foundation, but concentrates much of the client experience in M6
and does not specify portable workspace capture and restoration.

The [vision and roadmap](../superpowers/specs/2026-10-25-openxet-agent-storage-vision.md)
compares the accepted scope with the source at main commit
`3f5add9206bf8a5d1df3884cd6380c45060d1221`. It separates implemented
code, planned features, product hypotheses and proposed performance gates.

## Proposed decision

1. Make continuity of work across agents, tools and machines an additional
   product goal. Preserve the self-hostable Xet server and all protocol,
   identity, storage-truth, security and hosted/core parity commitments.
2. Keep repositories and buckets as the two storage primitives (ADR 0010).
   Artifacts and workspace snapshots are versioned manifests referencing
   ordinary content. Keep the Xet wire format and `xet://` contract unchanged.
3. Build workload measurement first alongside completion of M2. Deliver a
   minimal artifact handoff and a reusable reader/cache before expanding
   into workspace management. Bring these selected M6 capabilities and
   preview packaging forward; measure task latency, physical allocation,
   transfers and restoration as well as protocol performance.
4. Keep active writes on native local filesystems initially. Use existing
   package stores, build caches and filesystem cloning where appropriate.
   A general writable remote filesystem needs evidence of a benefit over
   those baselines before implementation is planned.
5. Require a synchronous durable snapshot root, retention ownership,
   verifiable restoration and a local-state recheck before local workspace
   release. The existing Git ref recovery RPO is not sufficient alone.
   Root/pin persistence and recovery take a further design before P3 code.
6. Add packaged workflow previews before all M7 channels are complete.
   Keep implementation moving while obtaining user evidence. Review the
   final 1.0 cut explicitly; no existing feature is silently removed from
   the accepted 1.0 scope.
7. Apply the [feasibility audit](../research/S15-agent-storage-feasibility.md)
   and its F1 to F6 gates. Keep Mac mounts version/backend-specific, scope cache
   bounds to coordinated storage, and restrict initial automatic local
   release to managed workspaces with writer exclusion. A future decision
   must reconcile strict delegated egress caps with direct downloads and
   the approved tenant read-budget policy before those caps are promised.

## Consequences

- If accepted, update design §1, §1a, §7.1, §10.8 and §12, AGENTS and
  the relevant milestone plans together before following the new sequence.
  Until then, the current design remains authoritative.
- This bends the existing M6/M7 ordering and the M0 to M7 release-cut decision.
  It does not discard ongoing Hub/Git work, shorten the 28-day minimum GC
  grace, change bucket last-writer-wins semantics or approve the S3 frontend.
- Workspace capture must preserve Git state and selected unfinished work,
  not merely copy a linked worktree's `.git` path. It is a file-state
  feature; process memory and running environments remain separate.
- Artifact publication initially uses existing fenced Git commits and
  repository authorization. Narrower sharing and conditional publication
  over buckets require explicit contracts before they are advertised.
- Production backends, recovery, quotas, delegated identity, audit and
  export remain prerequisites for their corresponding production promises.
  A client prototype does not authorize a public multi-tenant launch.
- Proposed thresholds and external design-partner goals are evaluation
  criteria, not measured savings, a demand forecast or hosted pricing.

## Alternatives considered

- Complete the current sequence without change. This preserves the plan
  but tests the workspace and handoff opportunity late.
- Build a writable remote filesystem first. This takes on small-file,
  metadata and platform complexity before measuring the actual bottleneck.
- Build another messaging protocol or an agent runtime. Existing systems
  can carry task messages and artifact references; OpenXet can serve them
  through a focused storage interface.
