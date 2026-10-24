# openxet design system

<!-- description: The shared visual language, content rules, components, user journeys and quality standards for the openxet website, documentation and interfaces. -->

Version 1 · 2026-10-24 · Applies to openxet.com, its documentation, hosted
onboarding, and the optional self-service UI. This is the canonical design
reference for humans and coding agents. The product design and accepted ADRs
remain authoritative for capabilities, architecture and security.

This system turns a first visit into understanding, a successful first task,
and a reason to return. Visual polish supports that sequence. Trust is earned
by working examples, honest status, useful feedback and reliable behavior.

## Overview

### The promise and the first understanding

**openxet stores large files and reuses the parts it already has when you upload
a new version.** It is open-source storage based on the Xet protocol. Models,
datasets and build artifacts are examples, not the limits of the product.

Explain in this order:

1. A person changes a large file.
2. Much of the file can remain the same.
3. openxet recognizes those parts, so they do not need uploading again.
4. This is called deduplication. The files people use remain complete files.
5. People can use the hosted service, operate their own installation, or embed
   the storage in another product, subject to the current availability below.

The first **aha** is “I can keep the whole file while uploading less.” It must
be visible above or immediately below the fold, require no account, and work
as a still diagram. A technical explanation is available one click deeper.

### Personality

Calm, capable, open and considerate. Use light mineral surfaces, dark blue ink,
precise type, and one useful illustration. The distinctive element is a pair
of file-version strips: unchanged pieces line up; one new piece has a warm
highlight. It represents the product instead of decorating it.

Keep the existing lowercase name and four-part symbol. Refine its color and
spacing; do not invent a new logo in an unrelated feature change. Do not
borrow Hugging Face's logo, illustrations, customer claims or visual identity.
Credit the Xet protocol and link to upstream explanations.

Three directions were considered: a dark terminal-led page, an infrastructure
blueprint, and a light explanation-led page. Choose the last: it welcomes
less technical readers and gives real product evidence room to breathe.
Terminals and architecture diagrams belong after the explanation.

### Stakeholders and the decisions they make

| Stakeholder | Question the site must answer | Evidence / destination | Next action |
|---|---|---|---|
| Newcomer | What does it do, in words I know? | Hero + file-version diagram | Understand the example |
| Researcher, engineer, data practitioner | Will it fit how I upload and download? | Tool compatibility + tested quickstart | Try a supported workflow |
| Self-hoster / operator | Can I install, maintain and recover it? | Configuration, backends, upgrades, recovery, benchmarks | Run the storage-only quickstart today |
| Platform builder / aqora | Can I fit this into my product and permissions? | APIs, authorization seams, tenancy, optional UI | Read integration and architecture references |
| Security reviewer / data owner | Can I check the boundaries and evidence? | Security tests, private disclosure, design and ADRs | Review the controls |
| Budget owner / procurement | What changes in cost, and what are the obligations? | Measured bytes, cost assumptions; pricing and terms at launch | Evaluate the deployment option |
| Contributor / maintainer | Can I understand and improve it? | Source, governance, roadmap, contribution guide | Find a concrete contribution |
| Agent acting for any of these | What works, how do I call it, how do I recover? | Markdown, llms.txt, schemas, exits, scoped authority | Execute an authorized task |

Human and agent are interaction modes across all roles. Do not force a
“human or agent?” choice at the front door. They receive the same facts and
availability; each has a suitable interface. Buyers and reviewers are often
different people from the person doing the first upload.

### Devices, environments and motivations

Design for the task and situation, not a separate visual brand for every persona.
The same person can move through several of these scenarios in a day.

| Setting | Likely motivation / constraint | Required response |
|---|---|---|
| Phone, arriving from a shared link | Understand quickly; cannot comfortably install a server | Short explanation, readable diagram, easy waitlist, a durable quickstart link; no terminal interaction required |
| Tablet or touch laptop | Evaluate options, read examples, show a colleague | Touch-sized controls, responsive comparisons, no hover-only information or drag-only interactions |
| Desktop browser beside a terminal | Try the product with minimal context switching | Exact commands, prerequisites, copy without prompts, expected output, links to recovery steps |
| Notebook / research workstation | Move a dataset or checkpoint without changing a research workflow | Version-specific Python/client examples and honest large-file behavior; keep examples useful outside ML too |
| Slow mobile connection or unreliable Wi-Fi | Decide whether to trust the product before investing time | Small static page, fonts that fall back, useful content before JS, retained form input, explicit retry state |
| Corporate proxy, VPN or restricted script/CDN policy | Evaluate or deploy within existing constraints | Self-hosted assets, native HTML forms, no third-party font/widget dependency; document service endpoint requirements |
| Keyboard, screen reader, low vision or reduced motion | Complete the same task independently | Semantic order, labels, text equivalents, zoom/reflow, strong focus, reduced motion and error summaries; no separate degraded mode |
| Shared workstation, screen share or public demo | Show the product without leaking private resources | Examples contain no secrets; hide/reveal credentials deliberately in future app; no private receipt in analytics or screenshots |
| Operator on a private network or air-gapped system | Install, inspect and recover without access to hosted tools | Downloadable/versioned docs and native CLI; distinguish internet prerequisites from offline operation; waitlist is optional |
| Platform product team | Assess how storage fits existing naming, identity and UI | Explain supported seams and permission contracts; the platform's own brand need not inherit openxet's marketing layout |
| Budget/security review circulated internally | Build a defensible recommendation, often without running code | Stable deep links, printable/readable evidence, dated benchmarks, explicit current/planned status and named maintainers |
| Agent in terminal, notebook or CI | Execute an authorized task unattended | Current Markdown, schemas, stable errors/exits, explicit inputs and versions; no interactive-only prerequisite or assumed browser login |
| Browser-controlling agent | Navigate the same public experience a person uses | Semantic control names, labels, status text and ordinary links; no canvas-only content, misleading disabled actions or hidden agent instructions |
| Human delegates then returns later | Preserve intent and review the result | Share goal/resource/scope/expected result without secrets; future app attributes actor, shows pending approval and provides a durable result |

Use the right **next step for the context** without fingerprinting visitors:
all useful paths remain visible and user-chosen. Do not infer skill, disability,
budget or role from device size. A mobile practitioner can still read every
technical reference; a desktop beginner still sees the plain explanation.

When a task must continue elsewhere, offer a stable link or credential-free
instruction block. Never put tokens, email addresses, private repository names
or cancellation receipts in share URLs, page titles, telemetry or screenshot
fixtures. Do not silently copy, launch a terminal, download executable code,
subscribe an address or authorize an agent on page load.

For a signed-out/stale session, preserve only non-sensitive intent and return
the visitor after external sign-in. For cross-device setup, obtain credentials
again through the approved identity flow. A public marketing page never needs
to know the visitor's identity to explain the product.

### Availability and launch states

The user requested a page designed for the final hosted product, with sign-up
disabled and a real waitlist now. Express the intended product coherently,
then state availability next to the action. Do not make every section a roadmap.

| State | Primary action | Identity / pricing | Technical truth |
|---|---|---|---|
| **Current: waitlist** | Join the waitlist → `/waitlist/` | No active hosted sign-up, sign-in or checkout; no invented prices | M1 storage-only server/client works; Hub/Git and hosted service remain planned |
| Hosted launch | Start free → `https://app.openxet.com/signup` | External identity provider; published limits, pricing and terms | Enable only after the real onboarding and stock-client tests pass |
| Returning hosted user | Sign in → `https://app.openxet.com/signin` | Same external identity and granted scopes | Return to the user's selected workspace |
| Service problem | Status and recovery action | Preserve inputs and authentication context safely | Never imply a successful write before acknowledgement |

The app subdomain and path names are a **design decision for future routing**,
not an assertion that DNS or endpoints exist. No live link goes to these routes
before they work. The header reserves a secondary sign-in position at desktop;
in waitlist mode replace it with a useful GitHub/docs link instead of a dead
button. Future `/signup` and `/signin` aliases on the marketing origin should
redirect only after launch. Pricing appears in navigation only when publishable.

### Information architecture

Current navigation: **How it works · Hosting · Docs · GitHub · Join the waitlist**.
Keep it short. On mobile, show the brand and waitlist action with a keyboard
accessible disclosure for the remaining links; do not hide essential paths.

| Destination | Job | Search intent | Present implementation |
|---|---|---|---|
| `/` | Explain value, demonstrate reuse, route the visitor | open-source large-file storage; Xet storage | Homepage |
| `/#how-it-works` | Make the mechanism understandable | how file deduplication works | Explanation and example; preserve old `#dedup` links |
| `/#hosting` | Explain hosted, self-hosted and embedded choices | hosted or self-hosted Xet | Honest availability beside each option; preserve `#hosted` |
| `/waitlist/` | Save a request for hosted launch contact | openxet early access | One-field form + privacy + confirmed receipt |
| `/docs/` | Accomplish a real task | openxet documentation | mdBook, current commands and compatibility |
| `/about/` | Establish who builds it and how to participate | openxet project / contact | Maintainer, source, governance, feedback and disclosure links |
| `/privacy/` | Explain website/waitlist data handling | openxet privacy | Factual current practices, no speculative certifications |
| `/llms.txt` | Find authoritative machine-readable material | Agent discovery | Generated from the same docs navigation |

Future dedicated `/self-host/`, `/integrate/`, `/security/`, `/pricing/` and
`/blog/` pages are justified by distinct content and visitor tasks, not keyword
counts. Until that content exists, link directly to useful docs. Do not publish
empty landing pages. Blog posts need an author, date, original example, technical
review and related next step; use the editorial backlog in the research note.

### Homepage sequence and working copy

| Section | Question answered | Content / behavior |
|---|---|---|
| Hero | What is this? | **Large files. Lighter uploads.** Supporting text names storage and explains reuse. Hosted waitlist status sits beside CTA. |
| File-version illustration | Why should I care? | Show a previously stored file and its edited version; distinguish reused and new parts with labels as well as color. |
| Explanation | How does it work? | Three real steps: split into parts, recognize existing parts, upload new parts. Explain “chunks” after “parts.” |
| Deployment choices | Where does my data live? | Hosted service (waitlist), own infrastructure (quickstart), own product (integration docs). No fake cloud availability. |
| Familiar tools and agents | How will I use it? | Separate available `openxet` / `hf_xet` flows from planned Hub/Git/MCP. Link exact current references. |
| Evidence | Why trust it? | Apache-2.0 source, reference-file checks, real-client tests, measured benchmarks and public decisions. |
| Practical questions | Is this right for me? | Workloads, variable savings, independence from Hugging Face, release status and pricing availability. |
| Final action | What now? | Repeat the waitlist action; offer a direct self-host quickstart. |

Desktop composition:

```text
openxet          How it works  Hosting  Docs  GitHub     [Join the waitlist]

Large files.                    Previous file  [A][B][C][D][E][F][G][H]
Lighter uploads.                Updated file   [A][B][+][D][E][F][G][H]
Plain explanation.              7 parts reused · 1 new part to upload
[Join the waitlist] [See how]    Illustrative example · change a part
Hosted service coming soon.

                 Same file to work with. Less to send.
                 explanation → hosting → tools → evidence → FAQ → CTA
```

On mobile: headline → explanation → actions/status → illustration → remaining
sections. Avoid forcing a large artwork above the reason to use the product.

## Colors

Use semantic CSS variables, never ad hoc colors in a component. These are the
normative values. Functional colors carry stable meanings; new/changed data is
amber, interaction is blue, success is green, failure is red.

```yaml
colors:
  canvas: "#F5F7FB"
  surface: "#FFFFFF"
  ink: "#142238"
  muted: "#526079"
  primary: "#304BCE"
  primary-hover: "#253DAE"
  primary-soft: "#E8EDFF"
  border: "#D3DBEA"
  control-border: "#77859C"
  new-data: "#F5CF6B"
  success: "#176843"
  danger: "#A12532"
  dark-canvas: "#101A2B"
  dark-surface: "#17243A"
  dark-ink: "#EDF2FB"
  dark-muted: "#B2BCD0"
  dark-primary: "#A5B7FF"
  dark-primary-hover: "#C0CCFF"
  dark-primary-soft: "#26365A"
  dark-border: "#394963"
  dark-control-border: "#8394AF"
  dark-success: "#8DD8B0"
  dark-danger: "#FFADB8"
```

The page is mainly canvas/surface and ink. Blue identifies actions and selected
content. Amber occupies only the changed file pieces and their legend; never
use it as unreadable text on white. Primary buttons use white text in light
mode and dark ink on the lighter blue in dark mode. Use control-border for an
input boundary that must be perceivable; the quieter border is for separators.

Default to system theme, preserve a deliberately chosen docs theme, and check
both. Never rely on red/green, opacity alone, or hover to express state. Forced
colors must preserve control boundaries and the changed-part labels.

## Typography

| Role | Family | Size / line height | Weight / tracking |
|---|---|---|---|
| Wordmark | Manrope | 24px / 1.1 | 750 / -0.04em |
| Hero | Manrope | clamp(44px, 5.4vw, 76px) / 1.04 | 650 / -0.045em |
| Section heading | Manrope | clamp(30px, 3.5vw, 44px) / 1.15 | 650 / -0.035em |
| Component heading | Manrope | 22px / 1.25 | 650 / -0.02em |
| Lead | IBM Plex Sans | clamp(18px, 1.6vw, 21px) / 1.55 | 400 / normal |
| Body | IBM Plex Sans | 17px / 1.65 | 400 / normal |
| Controls / navigation | IBM Plex Sans | 15–16px / 1.35 | 500–600 / normal |
| Captions / metadata | IBM Plex Sans | 14px / 1.5 | 400–500 / normal |
| Code / file data | IBM Plex Mono | 13–14px / 1.6 | 400 / normal |

Use rem units in implementation, assuming a 16px browser default; never prevent
zoom. Body text is at least 16px on mobile. Prose width is 60–68 characters;
hero copy 42–48 characters. Headings use sentence case. Reserve uppercase for
short diagram labels; do not turn every section label into an eyebrow.

Self-host Latin WOFF2 subsets from the official font distribution with license
files, `font-display: swap`, and system fallbacks. Only preload a font used
above the fold. Do not send visitors to a font CDN. Fall back to system sans
and monospace without hiding text or breaking layout. Use tabular numbers for
metrics, never monospace for long paragraphs.

## Layout

```yaml
spacing:
  xs: 4px
  sm: 8px
  md: 12px
  base: 16px
  lg: 24px
  xl: 32px
  2xl: 48px
  3xl: 64px
  4xl: 96px
  5xl: 128px
layout:
  page-max: 1200px
  reading-max: 680px
  form-max: 480px
  mobile-gutter: 20px
  desktop-gutter: 32px
  section-gap: "clamp(64px, 8vw, 112px)"
```

Use a two-column hero above 960px, a single column below it. Use a three-column
deployment choice group only when all choices fit comfortably; collapse by
768px. At 320px, no horizontal page scrolling, clipped controls or tiny text.
Code blocks may scroll horizontally within a labeled region.

Use alignment to establish hierarchy. Group content about the same decision;
do not put every sentence in a card. Numbers signify a real process, not a
decorative layout. Keep sticky navigation small, and use scroll margins so it
does not cover headings or focused controls.

## Elevation & Depth

Default surfaces have no shadow. Use a 1px separator for ordinary groupings.
The hero's file workspace can use `0 16px 48px rgb(20 34 56 / 7%)`; this is the
single elevated object. Dialogs, when needed in the future app, use
`0 16px 48px rgb(20 34 56 / 18%)`. No glow, glass blur, background particles,
floating decorative panels or scroll parallax. Do not apply hover elevation
to something that is not clickable.

## Shapes

```yaml
rounded:
  small: 4px
  control: 8px
  panel: 16px
  large-panel: 24px
```

File parts use 4–6px corners. Controls use 8px corners. Rounded pills are only
for short status labels. Use a consistent 20px icon box, 1.75px stroke and
round joins. Icons accompany meaningful text unless their accessible name is
unambiguous. Inline SVG and CSS diagrams are preferred over decorative raster
images. Provide a 1200×630 social image matching the current page and status.

## Components

### Buttons and links

Primary action: blue fill, at least 44px high (48px on principal forms), 16px
horizontal padding minimum, semibold label. Secondary: surface with visible
border. Text links inside prose are underlined; navigation links may use an
underline on hover/focus. An arrow is optional and decorative.

Every action has default, hover, focus-visible, pressed, pending, disabled and
error behavior where relevant. Focus uses a 3px primary outline and 3px offset,
with a contrasting gap. Pending buttons retain their width and say what is
happening. Never disable a form just because the user has not entered data;
validate on submit. Disable only during an in-flight duplicate action.

Use `<a>` to navigate and `<button>` to act. No nested interactive card links.
One primary action per decision. “Join the waitlist” consistently names the
current hosted action; never alternate with “Get started” or “Start free.”

### File-version demonstration

Use eight labeled parts in an **illustrative** file. The initial view already
shows seven reused parts and one new part. A small “Change another part” action
can change the example; do not require it to reveal the idea. If offering
scenarios, name them “Small edit”, “Same file” and “All new” with real buttons.

Keep equal toy parts explicitly illustrative. Real chunks vary in length;
file formats, edits, compression, client caches and metadata affect transfer.
Do not claim a 1-byte edit always transfers one byte, imply every training
checkpoint mostly repeats, or present a toy percentage as a benchmark.
Use “parts reused” and “new parts to upload” rather than a fake live speed.

New parts carry a plus mark or “new” label and a distinct border as well as
amber fill. Unchanged parts keep matching labels in both rows. Announce a
short result in a polite live region after user interaction. No automatic
loop, blinking cursor or moving text. Static HTML explains the default state
when scripts fail. Reduced motion removes transitions.

### Cards, tables, code and proof

- Deployment cards use the same order: name, who operates it, data location,
  availability, action. Compare meaningful choices, not invented tiers.
- Compatibility tables say **Available**, **Planned**, or **Not supported** in
  text. A green check never substitutes for a version or qualification.
- Code examples are copyable text with a language label, prerequisites and
  expected result. Copy only commands, never prompts or example output. State
  the tested version; use the current quickstart as source. Never include a
  real credential in markup, analytics or a copied agent handoff.
- Benchmarks link the workload, date, machine, version and reproducible command.
  Distinguish logical file bytes, new bytes transferred and physical stored
  bytes. Compression and dedup savings are different measurements. Money needs
  an actual price basis; show no invented savings in euros/dollars.
- FAQ uses native `<details>/<summary>` or an equivalently tested disclosure.
  Content remains in the HTML and accessible to crawlers. Never hide the only
  explanation of availability or a material limitation in a closed FAQ.

### Waitlist and consent

Use a dedicated page and a short homepage invitation, not a blocking modal.
The form has one visible **Email address** label; `type=email`,
`autocomplete=email`, `inputmode=email`, no company/name/password requirement.
The action itself requests launch contact; no prechecked newsletter consent,
referral competition, fake queue position or urgency.

Supporting copy: “We’ll email you when hosted openxet is ready to try.” Link
the current privacy explanation. Distinguish an unverified expression of
interest from a verified address or account. Do not send unrelated marketing.

States:

| State | Visible behavior |
|---|---|
| Empty / editing | Explain purpose; submit enabled; no premature error |
| Invalid | Inline error tied to field; keep value; focus the relevant field |
| Saving | “Saving…”; prevent accidental duplicate submission; announce status |
| Saved | “Your request is saved.” Explain launch contact and expose a private removal link |
| Repeated request | Same externally observable success; no disclosure of membership |
| Network/server failure | Keep email; explain retry; never display success |
| Rate limited | Explain waiting and retry; honor server retry time |
| Script unavailable | Standard form POST reaches the service and returns a useful page |
| Removal | GET is a confirmation page; explicit POST removes the request using its secret receipt |

No public list of addresses, no email in a URL, no secret in an agent prompt,
no secret in source. Database administration stays behind owner access. Do not
silently add a CRM, newsletter vendor or tracking script. The service and its
database are operational tooling, not openxet-owned user identities.

The maintainer owns retention: review the list at least quarterly, remove
withdrawn requests immediately through the receipt flow, and delete remaining
waitlist data within 90 days after the waitlist closes. Do not migrate addresses
into a newsletter or account database without a separate choice. This is an
operator procedure, not an automatic expiry guarantee; document the closure
date and completed deletion in the protected operations record. The same rule
applies to exports. Short-lived rate counters are purged by the service.

### Future hosted onboarding and human/agent collaboration

At launch: sign in with an existing external identity → choose workspace/repo
and region when necessary → receive a narrowly scoped credential through the
proper secret channel → run the chosen upload → see a durable success and
actual transferred/stored bytes. Progressive disclosure keeps advanced storage
settings out of the first upload. BYO bucket needs its own permission review.

The same task can begin in UI, CLI or agent. Offer “Copy command” and “Read as
Markdown” beside relevant actions. A handoff includes goal, endpoint, resource,
scope, version, prerequisites and expected result, with a reference to where a
credential can be obtained. It never includes a credential itself.

For delegated actions, the future app shows who acted, on whose behalf, the
affected resource and the result. Pending approval is a visible/pollable state,
not a vague failure. Destructive actions show the exact effect and applicable
grace period. Cancellation, expiry, revocation and partial failure must leave
the user with a next step. These are product requirements, not features to
pretend the waitlist already implements.

### Loading, empty, error and recovery states

Use the same content order everywhere: what happened → what is affected → what
to do. Keep useful content visible during refresh. Empty screens invite a real
first task. Never show a spinner without a status or an endless retry. Use
inline errors for fields, page-level errors for unavailable services, and
short-lived notices only for nonessential confirmation. Do not rely on toasts
for errors or cancellation receipts.

Examples: “Enter an email address, such as you@example.com.” “We couldn’t save
your request. Your address is still here; try again.” “This token has expired.
Create a new token for this repository.” Never echo a secret or server-derived
hash. The 404 says “Page not found” and links home and to documentation.

## Do's and Don'ts

### Voice and vocabulary

Write as a knowledgeable person helping someone do the work. Be precise and
warm without cheerleading. Use concrete nouns, active verbs and varied sentence
length. Read copy aloud. Explain a term once, then use it consistently.

| Prefer | Avoid / reason |
|---|---|
| “Upload only the parts that changed.” | “Unlock seamless next-generation data workflows.” |
| “Run openxet on your own infrastructure.” | “Achieve complete data sovereignty.” without concrete controls |
| “Hosted service coming soon.” | An active-looking “Start free” button before signup exists |
| “See what works today.” | “100% compatible” without a supported-version matrix |
| “We’ll email you when it’s ready to try.” | “Join the revolution”, fake scarcity or invented social proof |
| “New data”, then “new chunks” | CAS, xorbs and shards in the first paragraph |
| “Stored bytes” / “uploaded bytes” | An unlabeled “saved” number that mixes cost, time and space |
| “People and agents can use the same workflows.” | Treating agents as a separate product with different truth |

Avoid “effortlessly”, “blazing fast”, “best-in-class”, “enterprise-grade”,
“seamless”, “revolutionary” and “perfect” in public claims. Show evidence instead.
Do not make every heading a three-word slogan or every paragraph a symmetrical
contrast. No jokes in errors, billing, permission requests or data-loss states.
No fabricated quotes, badges, customer logos, customer counts or testimonials.

Editorial review has seven passes: clarity, voice, relevance, proof,
specificity, emotional honesty and friction. Emotional honesty means relief
from repeated uploads and confidence in control, not manufactured fear.

### Accessibility and performance

Target WCAG 2.2 AA. Check keyboard order, visible/unobscured focus, landmarks,
labels, name/role/value, contrast and reflow manually as well as automatically.
Our 44px target size is a product standard, stricter than the 24px WCAG 2.2 AA
minimum with its exceptions. Normal text needs 4.5:1 contrast; large text 3:1;
required control boundaries and graphics 3:1. Status is never color-only.

Test 320, 390, 768 and 1440 CSS-pixel widths; 200% zoom and 400% reflow;
keyboard only; screen-reader landmarks/forms; both color schemes; reduced
motion; failed fonts; disabled scripts; slow network; and long translated-like
labels. Do not announce every diagram cell separately. A paragraph describes
the graphic, and interaction announces only the changed result.

Budgets for marketing pages: no framework runtime; initial HTML+CSS+JS under
80 KiB compressed; self-hosted fonts under 120 KiB total; interactive JavaScript
under 12 KiB compressed; no third-party request before the visitor submits.
Measure rather than assume. Aim for LCP ≤2.5s, INP ≤200ms and CLS ≤0.1 at the
75th percentile where field data exists. Lab tests do not prove field results.
No telemetry without explicit operator opt-in; a waitlist submit is functional
processing, not permission to track browsing.

### SEO and discovery

Write for the person with the question. Each page has a distinct task, an
accurate title/description, one main heading, a canonical URL and descriptive
links. HTML contains the content without requiring JS. Titles should be concise
and distinctive, not padded to satisfy a scanner's preferred character count.

Use the actual product category early: open-source storage for large files,
Xet, models/datasets as examples. Do not target unrelated queries or publish
near-duplicate pages for synonyms. Original benchmarks and explainers earn
their own pages. Search-demand hypotheses are not measured volumes.

Metadata, HTML, Markdown and structured data must agree. Use only schema types
and facts supported by visible content. Do not add fabricated ratings, prices,
offers or FAQ rich-result promises. Keep all indexable pages in the sitemap;
exclude 404, private app screens, confirmation/removal pages and inactive auth
routes. Avoid blocking a page in robots.txt when a crawler must see its noindex.

`llms.txt` and Markdown support agents; they are not a promised SEO ranking
technique. Link current CLI/HTTP error contracts and availability. Do not invent
OpenAPI or MCP URLs before those are published. Preserve old anchors and URLs
or provide deliberate redirects when reorganizing content.

### Measuring understanding, conversion and loyalty

| Outcome | Proposed test / definition | Evidence status |
|---|---|---|
| Understand the product | After 10 seconds, 4/5 first-time participants can describe file-part reuse without prompting | Research target; not yet tested |
| Understand the benefit | A beginner can predict the unchanged-file and all-new-file examples | Research target; not yet tested |
| Find the right path | Hosted, self-hosting and integration participants find the right next action within 30 seconds | Research target; not yet tested |
| Reliable waitlist | Validation, failure, duplicate, cancellation and durable-save checks pass | Automated and live checks per release |
| First hosted success | Landing → completed real `hf upload` under 60s on the documented test rig | Future launch gate; include setup and network assumptions |
| Trust | Capability claims link evidence; no knowingly broken path or hidden availability | Editorial/release review |
| Return value | Users complete a second useful task and choose to return; feedback issues are closed with an explanation | Requires actual users; not inferable from clicks |

Do not optimize waitlist conversion at the cost of understanding or consent.
Do not equate a scanner score with accessibility, trust or love. Use manual
research until optional, approved measurement exists. Publish small changes,
record their reason and evidence, and stop an iteration when remaining ideas
need user research rather than pretending no further improvement is possible.

### Ownership, maintenance and intentional change

1. Before a website/docs/UI change, read this file and the relevant product
   references. Reuse tokens, words, component states and routes.
2. Add new components to this document when they introduce a reusable behavior.
   A one-off illustration does not justify a new abstraction or entire library.
3. Any intentional change to palette, typography, voice or navigation updates
   this file in the same PR, with rationale and before/after screenshots.
4. Change architecture through an ADR first. Presentation changes normally need
   a design-log entry, not an ADR. Keep facts and implementation status current.
5. Run the build, link/metadata checks, relevant behavior tests and visual review.
   Review the human and agent versions together. Require independent review.
6. Publish through the repository's PR and Pages flow. Confirm live content and
   functional actions, then record the deployment and audit result.

**Design log:**

- 2026-10-24, v1: established the explanation-led direction, file-version
  signature, blue/mineral palette, hosted-first waitlist state, shared
  human/agent facts and measurable quality criteria. User explicitly requested
  documentation, research, early understanding and small public releases.
- Future changes append date, reason, impacted tokens/components, evidence and
  migration scope here. Avoid duplicating the token definitions in prose elsewhere.

Research and source details: [website design research](docs/research/website-design-2026-10-24.md).
Implementation sequence: [website release plan](docs/superpowers/plans/2026-10-24-website-design.md).
