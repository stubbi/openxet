# Website design research

<!-- description: Research, stakeholder decisions, source references and verification evidence for the openxet website and shared design system. -->

Date: 2026-10-24. Baseline repository: `4d7c086`. This records observed sources,
design judgments and testable hypotheses separately. The canonical decisions
are in [the design system](../design.md); product scope remains in the
[product design](../superpowers/specs/2026-10-23-openxet-design.md).

## Brief and constraints

The user asked for thorough research, a complete `design.md`, a clear and
appealing website for humans and agents, SEO and non-generic copy, frequent
small releases, and an early moment of understanding. The final hosted product
should lead the page, but sign-up/sign-in stay disabled and a real waitlist
accepts requests. The user authorized setting up a lightweight service/database.

Existing tracked files were clean; unrelated `.claude/`, `.entire/` and
`Claude outputs/` material is outside this work. Website history reviewed:
`c0ca960` (landing page) and `15acbf3` (mdBook, Markdown and llms.txt). The
existing static site and mdBook pipeline are retained. ADR 0001 and the
competitive matrix establish protocol exactness and reproducible evidence;
the site must not substitute marketing claims for that evidence.

## Reference register

All external pages below were accessed on 2026-10-24. Observations are about
the inspected pages, not proof of their conversion performance. No reference
site's audience, customer claims, benchmarks or certification is ours.

| Source | Observed pattern | openxet decision |
|---|---|---|
| [Hugging Face Storage](https://huggingface.co/storage) | Clear product category, concrete actions, alternating explanation/visual sections, an eight-part upload comparison, and a dedicated agent section. Desktop page visually inspected in Chrome. | Learn from explanation beside proof. Put simple reuse before a terminal. Use original graphics and our own availability; do not copy its pricing, logos or claims. |
| [From Files to Chunks](https://huggingface.co/blog/from-files-to-chunks), 2024-11-20 | Walks from whole files to repeated pieces, then changes one piece. Its two-version visual makes overlap tangible. | Two versions of one file are the first example. A still diagram must teach the idea; interaction confirms it. |
| [From Chunks to Blocks](https://huggingface.co/blog/from-chunks-to-blocks), 2025-02-12 | Explains aggregation and shows a heatmap of shared blocks across model variants. The image answers a more technical question than the first example. | Reserve cross-file sharing and aggregation for deeper docs. Never imply one network request or object per displayed piece. |
| [Xet is on the Hub](https://huggingface.co/blog/xet-on-the-hub), 2025-03-18 | Connects the technology to small edits and describes rollout experience, with a direct early-access path. | Keep an obvious action while explaining the product. A launch story should include actual experience and limitations. |
| [Xet deduplication documentation](https://huggingface.co/docs/hub/main/xet/deduplication) | Explains content-defined boundaries and reuse. | Introduce “parts” before “chunks”; clarify that real chunk sizes vary and savings depend on data. The live code/spec, not an old illustration, decides protocol details. |
| [getdesign.md](https://getdesign.md/) and public [Replicate](https://getdesign.md/replicate/design-md), [Supabase](https://getdesign.md/supabase/design-md), [IBM](https://getdesign.md/ibm/design-md) previews | The catalog presents reusable design references with visual choices and rationale; its public previews describe contrasting developer-site directions. | Document values and why they exist. Catalog pages were reviewed; complete downloadable brand analyses were not imported or represented as our research. |
| [Google Labs DESIGN.md format](https://github.com/google-labs-code/design.md/blob/main/docs/spec.md), alpha | Separates structured tokens from explanatory Markdown and organizes visual guidance into predictable sections. | Use the same useful separation, with interleaved token blocks. Our file also specifies content, tasks, availability and governance; no automated format-conformance claim. |
| [Carbon content foundations](https://www.carbondesignsystem.com/building-blocks/foundations/content) | Clear, consistent, purposeful interface language. | Stable verbs and labels, useful empty/error states, concrete prose. This is a judgment about suitability, not copied brand voice. |
| [WCAG 2.2](https://www.w3.org/TR/WCAG22/), Recommendation 2024-12-12 | Normative accessibility requirements, including contrast, reflow, focus and target-size criteria. | Target AA; use a stricter 44px product target for controls. Combine automation with keyboard, zoom and assistive-technology checks. |
| [Web Vitals](https://web.dev/articles/vitals) | LCP, INP and CLS distinguish load, responsiveness and stability; field percentiles matter. | Set performance budgets and report lab/field evidence separately. No tracker added by default. |
| [Google helpful content](https://developers.google.com/search/docs/fundamentals/creating-helpful-content) | Prioritizes original, useful, reliable content for people. | Research-intent pages with examples and evidence. No filler articles or near-duplicate keyword pages. |
| Google [titles](https://developers.google.com/search/docs/appearance/title-link), [snippets](https://developers.google.com/search/docs/appearance/snippet), [canonicals](https://developers.google.com/search/docs/crawling-indexing/consolidate-duplicate-urls), [sitemaps](https://developers.google.com/search/docs/crawling-indexing/sitemaps/build-sitemap) and [AI features](https://developers.google.com/search/docs/appearance/ai-features) | Distinctive metadata, consistent canonical signals and crawlable content; no special AI text file requirement for Search. Reviewed in the independent SEO pass. | Fix generated docs metadata and sitemap coverage. Keep llms.txt for agent use without claiming a ranking benefit. |

The lessons above are intentionally short paraphrases. Reference visuals are
inspiration, not transferred assets. Our diagram, icon refinement and social
artwork are original. Any future direct asset reuse must record its source,
license and attribution first. Benchmark results from upstream remain upstream
results and never appear as openxet measurements.

## What the current site makes difficult

Source review of `website/index.html` found a protocol-heavy first paragraph,
GitHub stars as the primary action, a future `hf upload` terminal with
illustrative numbers, and deeper capability statements that look current.
This asks a beginner to understand storage internals before seeing the benefit.
It also invites a technical visitor to overestimate present compatibility.

The existing site already has useful foundations: static semantic HTML, a
skip link, theme support, a canonical homepage, social metadata, a generated
sitemap and Markdown docs. Keep these strengths rather than migrate frameworks.

**Squirrel baseline:** version 0.0.56, surface coverage, 12 pages, 2026-10-24.
48/100 overall; 55 errors and 171 warnings. Category scores: accessibility 84,
core SEO 72, performance 89, content 90, crawlability 97. This is a sampled
tool score, not a usability score or proof of regulatory compliance.

Confirmed/high-value findings to address:

- Documentation shares a global description and lacks canonicals/social tags.
- mdBook's search/sidebar markup raises missing labels, a missing ARIA target
  and an untitled table-of-contents iframe in the static output.
- New marketing pages would be absent from the current sitemap generator.
- The website lacks a focused project/contact page and a factual waitlist
  privacy explanation.
- The 404 uses protocol jargon and needs a narrow-screen layout check.

Findings needing judgment: titles shorter than the scanner's preferred length,
technical-word density, fixed sidebar iframe layout hints, repeated theme
assets, and headers controlled by GitHub Pages. Do not pad accurate titles,
rename protocol terminology, fabricate legal claims or migrate hosting merely
to satisfy a score. Full coverage follows the first release; compare equal
coverage when claiming a score improvement.

## The early understanding experiment

Hypothesis: a new visitor can explain “unchanged parts do not need uploading
again” within ten seconds of seeing the hero. Use eight labeled toy parts,
seven reused and one new. New data is both marked and colored. Show the
complete updated file and the smaller set to upload together. This prevents
the misconception that the user must manage partial files.

Improvement over a dense heatmap: lower initial decoding effort, adjacent
plain-language result, no reliance on hover, no animation required, and a
keyboard-accessible way to explore “same file” and “all new” cases. The latter
also explains when there are no dedup savings. Label it an illustration; do
not promise a universal percentage. Real edits can alter neighboring chunks.

Proposed research: recruit five people unfamiliar with Xet; show the first
viewport for ten seconds; ask what it does, what remains on their computer,
and what happens if every part changes. Record verbatim misunderstandings.
This research has not been conducted. Internal review and a scan cannot stand
in for it.

**Design critique, 2026-10-24:** after the user removed any requirement to
preserve the old website, an independent review found two comprehension gaps
in the first proposal: two complete file strips did not isolate the smaller
transfer, and a platform integration path was incorrectly presented as a third
hosting choice. Design v2 adds an explicit transfer tray and separates hosting
from integration. It also replaces vague task headings, directs trust questions
to operating evidence, and specifies shared human/agent recipes and a second
successful task. The visual direction is retained for its explanatory purpose,
not because it already exists. These are review-based improvements; participant
testing is still needed to establish understanding, recognition and preference.

## Stakeholder and SEO implications

The [design system](../design.md) maps eight stakeholder perspectives to
questions, proof and actions. The homepage sequences shared questions; task
paths provide depth. Avoid eight audience tabs or a large solution menu.

Search intents are qualitative hypotheses, without volume/ranking data:
large-file storage and Xet on the homepage; deduplication explanation in a
learning page; self-hosting in the quickstart; precise commands in references;
trust through named maintainers, source, tests and reproducible results.
No Search Console property or analytics account was connected for this review.

## Editorial backlog

Publish when the underlying example is tested, not on a quota:

| Article | Reader outcome | Original asset / proof required |
|---|---|---|
| What happens when you change a large file? | Understand reuse without protocol knowledge | Our file-version diagram; same/new/mixed cases |
| Run your first Xet storage server | Complete an upload and download locally | Existing CI-tested quickstart, explicit supported client version |
| Measure the bytes your next upload sends | Evaluate the benefit for their own data | Reproducible script, workload, hardware, versions and raw results |
| Where your files live when you run openxet | Understand storage and recovery responsibility | Current architecture; label future backends and recovery tools |
| Give an agent access to one repository | Delegate a useful task without sharing broad authority | Only publish the walkthrough when the corresponding token flow is implemented and tested |

Each article has an author, publication/revision date, references and a next
task. Do not reuse upstream numbers as our own, create generic “AI storage”
filler, or publish unfinished third-party security information.

## Verification and release record

The [implementation plan](../superpowers/plans/2026-10-24-website-design.md)
tracks completion. Append release commit/PR, deployment result, audit coverage,
browser sizes and observed failures as they are verified. Distinguish passing
checks from checks not run. User research and production field performance
remain unmeasured until actually collected.

### First public release

PR [#56](https://github.com/stubbi/openxet/pull/56), merge commit `150e632`,
deployed successfully through Pages on 2026-10-24. The public design page's
canonical URL and content were fetched after deployment. Required CI passed,
including Linux/macOS Rust and both compatibility legs.

The first full audit used Squirrel 0.0.133 with `--coverage full --max-pages 100
--refresh`, with `NO_TELEMETRY=1`. Its baseline was 49/F with 116 failed rules;
after this release it was 73/C with three failed rules. The three failures
refer to the same 24 unnamed task checkboxes in ADR 0004. More pages became
discoverable (65 fetched resources, 33 HTML pages evaluated), so these scores
are directional rather than a fixed-cohort experiment. The remaining errors
and repeated table names were reproduced and fixed with item-specific names
and authored captions in the next release candidate.

### Homepage and waitlist release candidate

Independent reviews covered design coherence, copy, accessibility and waitlist
data handling. Changes include the explicit upload tray, separate hosting and
integration choices, pending email locking, private-link copying, actionable
validation/retry states and shorter mobile form content.

The standalone waitlist was deployed using Sites; a real native browser POST
confirmed persistence before reporting success. The deployed HTTP smoke checks
passed signup, cancellation, origin rejection, no public reads and response
headers. Synthetic addresses were withdrawn. Frontend tests cover malformed
success, network failure, invalid email, throttling and removal-link validation.

Chrome rendering checks confirmed no horizontal page overflow at 320, 390, 768
and 1440 CSS pixels on the homepage and waitlist. The diagram's unchanged-file
state, mobile menu/Escape and form failure with retained input were exercised.
These are browser checks, not a completed assistive-technology or user study.
Cross-browser and physical-device testing, field Core Web Vitals, and first-time
participant comprehension remain follow-up evidence needs.

## Published homepage and waitlist verification — 2026-10-24

<!-- release-verification: 270ccd84bb9740ae5346fb059b0e256ff4df6e61 -->

PR [#58](https://github.com/stubbi/openxet/pull/58) merged as
[`270ccd84bb97`](https://github.com/stubbi/openxet/commit/270ccd84bb9740ae5346fb059b0e256ff4df6e61). The operator verified the successful
[GitHub Pages deployment](https://github.com/stubbi/openxet/actions/runs/40125379486) and checked the public website and waitlist
before recording this release. The waitlist is deployed independently: its exact
Sites v3 source, deployment and production HTTP signup/withdrawal evidence are in
[the operating guide](../hosted-waitlist.md#deployed-revision).

Recorded candidate checks passed: **20 Python site tests, 11 JavaScript behavior
tests and 31 service tests**, the site build and local links/fragments, plus the
waitlist TypeScript check and production build. The pre-merge redaction guard and
harness-isolation run passed **580 tests**. These are recorded test runs, not a
claim that this documentation update reran them.

The measured homepage HTML, CSS and JavaScript total **10.87 KiB gzip**; JavaScript
accounts for **1.19 KiB gzip**. The three self-hosted font files total **73.11 KiB**.
These are local artifact sizes and gzip estimates, not measured network transfer,
field loading or responsiveness metrics.

Chrome checks covered 320, 390, 768 and 1440 CSS pixels without horizontal page
overflow on the homepage and waitlist, and exercised the diagram, mobile menu and
Escape, form validation and retained input after failure. On 2026-10-24, a real
browser checked the then-public, pre-release docs search: sequentially typing
`upload` produced 30 results including the CLI reference and storage quickstart.
Opening the quickstart result reached `#4-upload-and-download-with-openxet` with
the heading highlighted. This is a search interaction observation, not a
screen-reader evaluation or a post-release search test.

ADR 0009 and the URI milestone handoff record a future `xet://` contract: M2
specification/shared parser, M6 client integration and M7 published support
evidence. No URI parser, SDK adapter or operating-system handler ships in this
website release. HTTPS remains the canonical browser link.

### Final public automated audit

The final public audit used Squirrel **0.0.133** at `2026-10-24T19:29:00.581Z`.
The report records full coverage, a 100-page cap, no cap reached,
and no replayed pages. Overall score: **83/B**.

| Reported measure | Result |
| --- | ---: |
| Crawled resources | 76 |
| HTML pages evaluated | 40 |
| Passed summary checks | 4523 |
| Warning summary checks | 235 |
| Failed summary checks | 0 |
| Total summary checks (passed + warnings + failed) | 4758 |
| Errors summed from reported score groups | 0 |
| Listed issue rules | 27 |
| Listed failing rules | 0 |
| Listed warning rules | 27 |
| Listed error-severity rules | 0 |

Summary counts and listed rule counts measure different things. Shared markup can
trigger several rules; these totals are not counts of independent product defects.

The XML lists no failed or error-severity issue rules. Warning findings still
need interpretation; this result does not establish usability or accessibility compliance.

The same Squirrel version previously reported 49/F and 116 failed summary
checks before the documentation release, then 73/C and three failed checks
after PR #56. Discovery and page coverage changed, so score changes are
directional; individual rule findings provide the stronger comparison.

Audit input: `openxet-audit-release2.txt`; SHA-256 `6e64acd82f1a78091d71ed8c903e9ff4d96638626ee98283956eb36efa1e8f7e`.

### Evidence still needed

- Participant testing of first-time comprehension, successful setup and return use.
- Cross-browser and physical-device checks.
- Assistive-technology evaluation beyond the recorded browser and automated checks.
- Production field Core Web Vitals.

The release demonstrates the checks above. It does not establish perfection,
usability for every visitor, accessibility compliance, conversion gains or loyalty.


### Live browser signup and homepage check

After PR #58 deployed, Chrome on `https://openxet.com/waitlist/` rejected `a@b`
with an actionable email message and `aria-invalid=true`. A synthetic
`example.com` request reached “Your request is saved”; the private-link copy
control reported success. The synthetic request was then withdrawn through the
API without logging its receipt. The deployed HTTP smoke test also passed again
and removed its own synthetic request. No launch email was sent.

The published homepage was visually inspected in its automatic dark appearance.
The “All new” control showed zero reused parts and eight new parts, and “Small
edit” restored seven reused parts and one new part. These checks establish the
observed interactions, not comprehensive operating-system or browser coverage.

### Warning triage

All **27 listed warning rules** in the final Squirrel report were reviewed against
the generated site and source. The 235 warning checks are not 235 independent
defects. No additional functional release blocker was confirmed. The missing
`security.txt` discovery route is addressed by the separate follow-up; its public
availability must be checked after deployment.

- **Canonical aliases:** `crawl/sitemap-coverage`, `core/title-unique`,
  `content/duplicate-title` and `content/duplicate-description` refer to `/docs/`
  and `/docs/index.html`. Both identify `/docs/` as canonical; the sitemap contains
  that preferred URL. Keep the working alias and canonical-only sitemap rather
  than inventing different content or indexing both. This follows
  [Google's canonical sitemap guidance](https://developers.google.com/search/docs/crawling-indexing/sitemaps/build-sitemap).
- **Copy thresholds:** `core/meta-title`, `core/meta-description`,
  `content/keyword-stuffing`, `content/word-count` and
  `content/title-pattern-outlier` mostly reflect concise task pages, repeated
  technical vocabulary and different marketing/documentation title formats.
  Do not pad the waitlist or replace precise protocol terms to satisfy counts.
  Next iteration: remove the redundant `openxet docs - openxet docs` title and
  improve individual descriptions where they fail to explain the page, without
  treating character limits as a writing goal.
- **Agent discovery:** `ax/agents-md` and `ax/markdown-response` do not account
  for the existing `llms.txt`, per-document Markdown files, alternate links and
  visible Markdown links. There is no content-negotiated Markdown response at
  the homepage. Publishing repository editing instructions at the site root
  would not improve this reader contract; avoid a duplicate, drifting manual.
- **Documentation performance:** `perf/inp-hints`, `perf/dom-size`,
  `perf/render-blocking`, `perf/critical-request-chains` and `perf/unminified-js`
  identify real mdBook assets and long reference documents, not measured field
  failures. The report's resource attribution also lists docs scripts against
  marketing pages that do not load them. Profile representative docs on a slower
  device before changing script order, styles or document structure; preserve
  working search, sidebar, keyboard navigation and code copying. The small
  marketing modules do not justify adding a build dependency merely to remove
  an unminified-source warning.
- **Security controls:** `security/csp`, `security/hsts` and
  `security/x-frame-options` identify absent static-origin response controls.
  Keep these as explicit hosting-hardening work for the next iteration, with
  tested header control and HTTPS behavior; they are not proof of an exploited
  vulnerability or a blocker for this public waitlist release. A meta CSP cannot
  supply `frame-ancestors` protection ([CSP specification](https://www.w3.org/TR/CSP/#directive-frame-ancestors)).
  The separate service already sends CSP on native form responses.
  `security/form-captcha` does not mean abuse controls are absent: the service
  validates bounded input, checks origin, uses a honeypot and enforces persistent
  client/global rate limits. Reconsider challenges if abuse warrants their cost.
  `security/security-txt` is the concrete discovery fix noted above.
- **Navigation:** `links/no-contextual-inbound` identifies pages reachable mainly
  through the book navigation; add relevant task-to-reference links when they
  help a reader. `a11y/identical-links-same-purpose` identifies repeated `config`
  labels for different sections of the same reference. The surrounding sentence,
  list or table cell supplies purpose, as permitted by
  [WCAG's link-context criterion](https://www.w3.org/WAI/WCAG22/Understanding/link-purpose-in-context.html).
  Next iteration: use specific labels such as “memory configuration” to improve
  scanning and assistive-technology link lists. These are navigation improvements,
  not a confirmed conformance failure from this rule alone.
- **Asset and URL conventions:** `social/asset-divergence` compares two
  byte-identical favicon files under different build URLs and the docs/marketing
  theme colors. Align the docs browser theme color with its actual surface in a
  later polish pass; do not replace correct assets to match a path majority.
  `url/lowercase` concerns existing research filenames. Preserve working URLs;
  any future naming migration must retain redirects or aliases.
- **Contact and privacy discovery:** `eeat/contact-page` misses the existing
  project contact routes on About and Privacy. `eeat/privacy-policy` correctly
  observes that docs pages lack a direct Privacy link. Add a concise shared docs
  link in the next navigation iteration so readers can find the notice without
  returning to marketing pages. Neither finding justifies duplicating contact
  pages or adding claims unsupported by the project's actual practices.

These dispositions complete this warning review, not the outstanding participant,
cross-browser, physical-device, assistive-technology or field-performance work.
