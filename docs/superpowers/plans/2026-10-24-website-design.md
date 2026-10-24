# Website design and release plan

> **For agentic workers:** Execute the tasks in order, using
> `superpowers:executing-plans`; independent audit and review work may run
> alongside implementation. The user has authorized implementation and small,
> frequent releases to openxet.com.

**Goal:** Make openxet understandable on first contact, with a documented design
system, an honest hosted waitlist, and reliable paths for humans and agents.

**Architecture:** Keep the marketing site static on GitHub Pages and docs in
mdBook 0.5.4. Use the Sites plugin's managed Worker and D1 database for the
anonymous waitlist only; this is hosted operational tooling, not user identity
or a private fork of the storage server. Keep the service source versioned.

**Tech stack:** Semantic HTML, CSS custom properties, small progressive JavaScript,
Python site generation/checks, managed Worker/D1 for the waitlist.

**Spec:** Root `design.md`, alongside the product design's §7.1, §10.10 and §15.
The product architecture and ADRs retain precedence over presentation.

## Global constraints

- Hosted is the primary future journey; until launch its CTA is **Join the
  waitlist**. Do not activate sign-up/sign-in or invent pricing, customers,
  certifications, release availability or performance numbers.
- Preserve existing URLs, anchors where practical, Apache-2.0 identity, no
  telemetry by default, and generated Markdown/llms.txt.
- A beginner must understand the reuse of unchanged file parts before learning
  protocol vocabulary. Every diagram has a textual equivalent.
- Collect email only for requested launch contact. Never expose the database,
  put addresses in URLs, or report success without a durable write.
- No framework migration for the marketing site. No new identity system.
- Use Conventional Commits, DCO sign-off, PRs, independent review and passing
  required checks. Verify GitHub Pages deployment and live content after merge.

## Review focus

1. An agent mistakes launch copy for today's technical capabilities: availability
   must agree across homepage, docs and llms.txt.
2. A failed or repeated waitlist request lies about success: test database
   failure, validation, duplicate input, request size, origin and throttling.
3. A first-time mobile/keyboard visitor cannot understand or use the page:
   inspect 320/390/768/1440 widths, keyboard focus and no-JavaScript fallback.
4. Generated docs metadata silently breaks nested links or escaping: test
   canonical paths, descriptions with quotes/ampersands and Markdown alternates.
5. A future edit invents a color, phrase or unsupported claim: tie AGENTS.md to
   design.md, document evidence and check rendered output before release.

## Task 1: Research and durable design reference

**Files:** `design.md`, `docs/design.md`,
`docs/research/website-design-2026-10-24.md`, `docs/SUMMARY.md`, `AGENTS.md`.

- [x] Record stakeholder needs, current-state evidence and reference findings.
- [x] Define tokens, typography, layouts, components and their interaction states.
- [x] Define voice, information architecture, SEO intent, agent contracts,
  hosted/waitlist/launch transitions, metrics and change governance.
- [x] Have an independent builder check the design; resolve material gaps.
- [x] Publish root design through a docs include, without maintaining two copies.

## Task 2: First release — documentation foundations

**Files:** `scripts/generate-site-index.py`, focused tests under `site/`,
`site/llms-template.txt`, `docs/README.md`, `scripts/build-site.sh`,
`.github/workflows/pages.yml`; generated-output improvements where appropriate.

**Interface:** Site generation consumes the rendered HTML/Markdown tree and docs
navigation. It produces page-specific escaped metadata, one canonical per page,
Markdown alternates, sitemap entries and llms.txt without changing page paths.

- [x] Write failing tests for canonical/description/Markdown output and observed
  navigation/search accessibility defects, then demonstrate the failure.
- [x] Implement the smallest fixes, preserving mdBook behavior and architecture.
- [x] Run tests, build the site and check internal links.
- [x] Obtain independent review, open a PR, wait for required CI, merge and
  verify the deployment. Record the live baseline and result.

## Task 3: Hosted waitlist service

**Files:** `hosted/waitlist/` service source, tests, migrations and operator README;
`docs/adr/0008-hosted-waitlist.md`; Sites build checkout under ignored `target/`.

**Interface:** `POST /api/waitlist` accepts a bounded JSON body containing email
and an empty bot-trap field; returns versioned JSON after persistent storage.
Errors have `code`, `message`, `hint`. No anonymous list/read API. Browser origins
are restricted to openxet.com and the service's own origin. The public site's
database stays private. A receipt capability supports cancelling a request.

- [x] Record the operational choice and privacy/retention boundary in the ADR.
- [x] Create the Sites project once and persist its opaque identifier.
- [x] Write meaningful failing handler tests, then implement input validation,
  duplicate handling, rate bounds, durable save, cancellation and private storage.
- [x] Build and review the migrations; test with a disposable local database.
- [x] Deploy through the supported Sites workflow, enable public anonymous form
  access as required by the user's public waitlist, and verify one synthetic
  signup and cancellation. No launch emails are sent during this task.

## Task 4: Second release — homepage and working waitlist

**Files:** `website/index.html`, `website/assets/`, `website/waitlist/`,
`website/privacy/`, `website/about/`, `website/404.html`, docs theme styles.

**Interface:** Semantic HTML explains the product and provides working links
without JavaScript. JavaScript enhances the illustrative change/reuse diagram
and waitlist form. The form calls only the deployed service; it retains input
on recoverable error and provides durable-save confirmation and a private
cancellation link. Sign-up/sign-in remain unavailable.

- [x] Implement the documented tokens and homepage content order; create original
  diagrams inspired by the research. Mark examples as illustrative.
- [x] Test interaction math independently; test waitlist success/failure behavior
  where meaningful. Inspect actual page rendering and keyboard behavior.
- [x] Align docs, 404, social metadata/image and privacy/contact information.
- [ ] Run build/link checks and a local accessibility/performance review.
- [ ] Review, PR, CI, merge, verify live page and waitlist; record release result.

## Task 5: Audit and refinement

- [ ] Re-run Squirrel with full coverage; compare like-for-like findings with the
  initial sample, separating tool heuristics from confirmed defects.
- [ ] Fix verified remaining defects in a focused follow-up release if needed.
- [ ] Record measured outcomes, limitations, future launch steps and unresolved
  evidence needs. Do not claim that a scan proves usability, compliance or loyalty.

## Baseline

2026-10-24, repository `4d7c086`, Squirrel 0.0.56, 12-page surface sample:
48/100, 55 errors, 171 warnings. Main errors: mdBook control labels and missing
ARIA target/frame title. Main SEO issues: generic descriptions and no docs
canonicals or social metadata. Report retained in `/tmp/openxet-audit-before.txt`
during work; durable findings belong in the research note.


## PR 58 follow-up: npm dependency metadata in the secret scan

The agent-leg job at `d240abf` failed before running agent tests: the
credential-header detector interpreted the waitlist lockfile's npm dependency
`cookie` version as a Cookie header. The unchanged guard reproduces this locally.

1. Add regressions for the observed v3 lockfile entry and for credentials beside
   it, unknown/header fields, duplicate keys, malformed JSON and suspicious values.
2. Recognize only the exact waitlist lockfile path and
   `packages.<package>.dependencies.cookie` with a numeric release version,
   optionally prefixed by `^` or `~`. Validate JSON with duplicate-key rejection.
3. Exempt only the matching credential-header occurrence at its original source
   offset. Every other detector continues to inspect every original byte.
4. Run the complete redaction guard and harness-isolation tests, independently
   review the change, then rerun CI without weakening the agent or core checks.
