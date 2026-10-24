# 0001: Build openxet from scratch on HF's xet-core crates

<!-- description: openxet is a new codebase that takes the Xet format, hashing and chunking from Hugging Face's published xet-core crates, through its own protocol crate. -->

- Status: accepted
- Date: 2026-10-23
- Scope: whole project, `protocol`

## Context

Several open-source Xet servers already exist. They were evaluated on
2026-10-23 with a code audit of each and a hands-on run of the most complete
one, Shardline, against stock clients. Full results are in
`docs/competitive-matrix.md`.

- **Shardline** (~504k LOC, single author):
  - It reimplements the protocol core instead of depending on `xet-core`.
  - It conflicts with design principles: the database is authoritative, Git
    commits are projections, and tokens are admin-minted.
  - Against stock clients it fails for model-sized files with `hf`, for
    `git-xet`, for plain `git-lfs` and for cross-machine dedup.
- **ggoggam/openxet** depends on `xet-core-structures`/`xet-data` and tests
  against the spec's reference files. It shows that this approach works, but
  it is small and inactive.
- The design (§1, §16) requires byte-exact compatibility, and leadership on
  quality and governance that we can't inherit from code we didn't write or
  review.

## Decision

1. Build openxet as a new codebase following the design. We don't fork or
   build on an existing server.
2. Format, hashing and chunking logic comes from Hugging Face's published
   crates (`xet-core-structures`, `xet-data`, pinned exact versions). We
   don't reimplement it.
   - All use of them goes through our `protocol` crate, so upstream changes
     land in one place.
   - Where the upstream crates can't be used server-side, we reimplement only
     that piece. Spike S5 decides which pieces; each one gets its own ADR plus
     golden tests against the reference files.
3. Other projects are reference material only. Their documented client quirks
   and their failures become test cases (`docs/competitive-matrix.md`,
   "Lessons").

## Consequences

- Protocol exactness rests on upstream's own code. Version bumps of
  `xet-core` crates are deliberate, with the compatibility suite run on every
  bump.
- More up-front work than forking. We offset it by mining other projects'
  quirk lists.
- We depend on `xet-core`'s release cadence and API stability. The
  `protocol` crate isolates that.
- The client and server use different parts of upstream. The client core
  (`xet-data`, `xet-runtime`, `xet-client`) lives only in `openxet-client`
  (ADR 0006); the server keeps to `xet-core-structures`.
- License hygiene: the crates are Apache-2.0, and our NOTICE credits them.
- The `protocol` crate may compose upstream's public primitives with
  openxet's own policy checks, for example validating each decompressed
  chunk length and the chunk limits of an uploaded xorb before calling
  upstream's serializer. It must not reimplement format logic: parsing,
  serialization, compression and hashing stay upstream's code.

## Alternatives considered

- **Build on Shardline or contribute upstream.** Rejected:
  - The Hub/LFS layers stock clients rely on would need rewriting.
  - The protocol core is a reimplementation.
  - The architecture conflicts with our principles.
  - Bus factor 1 with no external review history.
- **Fork Shardline.** Rejected: we would inherit ~504k lines we didn't write
  and couldn't hold to our quality bar. Removing the out-of-scope frontends
  alone is a large project.
- **Reimplement the protocol ourselves.** Rejected: it maximises the risk of
  silent incompatibility (§2 anti-goals), and other projects show how easily
  it goes wrong.
