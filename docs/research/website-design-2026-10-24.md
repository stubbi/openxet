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
