# openxet design system

<!-- description: The shared visual language, content rules, components, user journeys and quality standards for the openxet website, documentation and interfaces. -->

Version 3 · 2026-10-25 · Applies to openxet.com, its documentation, hosted
onboarding, and the optional self-service UI. This is the canonical design
reference for humans and coding agents. The product design and accepted ADRs
remain authoritative for capabilities, architecture and security.

The purpose is to help someone understand openxet, make a sound choice, finish
a useful task and confidently return. Visual polish supports that sequence.
Working examples, honest status, useful feedback and reliable behavior earn
trust; a landing page alone cannot create loyalty.

**How to use this reference:** the principles and shared task contracts are
durable. The release blueprint, identity, tokens and example layouts are the
current versioned expression of them. Follow those adopted choices consistently
until an intentional revision replaces them; no existing page, logo or layout
is protected from a better-supported design. Preserve working URLs or plan
their migration. Current reference docs and verified behavior establish what
works; the product design and ADRs establish approved targets and architecture.
This file never turns a target into an available feature.

<span id="overview"></span>

## Durable principles

### The promise and the first understanding

**openxet stores large files and reuses the parts it already has when you upload
a new version.** It is open-source storage based on the Xet protocol. Models,
datasets and build artifacts are examples, not the limits of the product.

Explain in this order:

1. A person changes a large file.
2. Much of the file can remain the same.
3. openxet recognizes those parts, so they do not need uploading again.
4. This is called deduplication. The files people use remain complete files.
5. People choose who operates their storage. Integrating it into a product is
   a separate choice, subject to the current availability below.

The first **aha** is “I can keep the whole file while uploading less.” It must
be visible above or immediately below the fold, require no account, and work
as a still diagram. Show both the complete file and the smaller set to upload;
color alone does not explain what crosses the network. A technical explanation
is available one click deeper. Where a measurement exists, show it instead of
an illustration: the hero's upload trace is a real, to-scale result.

Anchor the opener on what the visitor already knows. Name the category (an
open-source Xet storage server) and the stock client that works with it in
the first glance; teach the mechanism after that, for those who need it
([S14](docs/research/S14-landing-messaging.md)).

The differentiated long-term proposition is Xet storage with a choice of
operator and an inspectable, shared open-source implementation. Explain that
choice after the basic benefit. Do not imply that deduplication is unique to
openxet or that an unavailable deployment option works today.

Prioritize understanding before persuasion, useful completion before clicks,
and evidence before reassurance. Give a beginner plain words and an expert a
short route to exact contracts. Both should reach the same truth.

### Stakeholders and the decisions they make

| Stakeholder | Question the site must answer | Evidence / destination | Next action |
|---|---|---|---|
| Newcomer | What does it do, in words I know? | Hero + measured upload trace; “How it works” example | Understand the example |
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

## Current release blueprint

This section describes the present website and the transition to hosting.
Change it when availability or evidence changes; it is not the permanent
information architecture of every openxet interface.

### Availability and launch states

The user requested a page designed for the final hosted product, with sign-up
disabled and a real waitlist now. Express the intended product coherently,
then state availability next to the action. Do not make every section a roadmap.
While hosting is a waitlist, the page's primary action is the path that works
today (the self-hosted quickstart), so no visitor's first click is a dead end;
the waitlist stays in the header and beside it. Swap them when hosted sign-up
launches.

| State | Primary action | Identity / pricing | Technical truth |
|---|---|---|---|
| **Current: waitlist** | Header: Join the waitlist → `/waitlist/`. Hero and final section: Run it locally → quickstart (primary), Join the waitlist (secondary) | No active hosted sign-up, sign-in or checkout; no invented prices | M1 storage-only server/client works; Hub/Git and hosted service remain planned |
| Hosted launch | Start free → `https://app.openxet.com/signup` | External identity provider; published limits, pricing and terms | Enable only after the real onboarding and stock-client tests pass |
| Returning hosted user | Sign in → `https://app.openxet.com/signin` | Same external identity and granted scopes | Return to the user's selected workspace |
| Service problem | Status and recovery action | Preserve inputs and authentication context safely | Never imply a successful write before acknowledgement |

The app subdomain and path names are a **design decision for future routing**,
not an assertion that DNS or endpoints exist. No live link goes to these routes
before they work. The header reserves a secondary sign-in position at desktop;
in waitlist mode replace it with a useful GitHub/docs link instead of a dead
button. Future `/signup` and `/signin` aliases on the marketing origin should
redirect only after launch. Pricing appears in navigation only when publishable.

### Claims and their evidence

These are the claims most likely to confuse a visitor, reviewed 2026-10-24.
They are pointers to authoritative material, not a second capability registry.
Before changing public copy, check its source and tested version. Update HTML,
Markdown and structured data together; link measurements rather than copying
numbers that will drift. A missing implementation or test stays **planned**.

| Claim | Status | Source / evidence to check |
|---|---|---|
| Storage-only server on local disk and SQLite; source build required | Available preview, M1 | [Quickstart](docs/quickstart-storage-only.md), [configuration](docs/reference/config.md), [changelog](docs/changelog.md) |
| Reuse already stored file data; `hf_xet` 1.7.0+ and `openxet` upload/download | Available in the documented storage workflow | [Executable quickstart](docs/quickstart-storage-only.md), [CLI byte-counter definitions](docs/reference/cli.md#openxet-upload) |
| Scoped credentials, signed transfer URLs and versioned CLI errors | Implemented boundaries; do not imply a security certification | [Configuration](docs/reference/config.md), [security test map](docs/reference/security-tests.md), [exit codes](docs/reference/exit-codes.md) |
| Hosted service, Hub/Git/LFS, external identity and MCP | Planned; no active hosted signup | [Product design](docs/superpowers/specs/2026-10-23-openxet-design.md), checked against current reference docs |
| Hosted uses the shared open-source implementation | Architectural commitment for launch | [Product design §15](docs/superpowers/specs/2026-10-23-openxet-design.md#15-hosted-openxetcom-decided-by-the-user-we-must-have-the-perfect-hosted-experience) |

### Resource addresses: selected future contract

The founder chose **`xet://`** for future openxet resource addresses;
[ADR 0009](docs/adr/0009-xet-resource-uris.md) records the decision. The authority
will name the actual deployment host and optional port. Hosted and self-hosted
installations use the same scheme; copied addresses must not silently select a
default server. HTTPS stays the canonical browser/share link. No access token,
password or signed capability belongs in a resource URI.

The name is selected; the full grammar, legacy compatibility and implementation
remain pending. M2-URI specifies the contract and parser; M6-URI delivers clients,
adapters and UI/docs integration, as tracked in the
[milestone handoff](docs/superpowers/plans/2026-10-24-resource-uri-milestones.md).
Once supported, offer distinct **Copy resource URI** and **Copy web link**
actions with the same task/resource context for humans and agents. Until then,
use released commands and ordinary HTTPS links in website examples. Do not
claim stock clients accept the scheme or that openxet owns the Xet protocol.

### Information architecture

Current navigation: **Compatibility · How it works · Hosting · Docs · GitHub · Join the waitlist**.
Keep it short. On mobile, show the brand and waitlist action with a keyboard
accessible disclosure for the remaining links; do not hide essential paths.

| Destination | Job | Search intent | Present implementation |
|---|---|---|---|
| `/` | Explain value, demonstrate reuse, route the visitor | open-source large-file storage; Xet storage | Homepage |
| `/#compatibility` | Say which stock clients work today and which are next | hf_xet self-hosted server | Per-tool status table |
| `/#how-it-works` | Make the mechanism understandable | how file deduplication works | Explanation and example; preserve old `#dedup` links |
| `/#hosting` | Choose who operates the storage | hosted or self-hosted Xet | Hosted and own-infrastructure paths; preserve `#hosted` |
| `/#integrate` | Fit storage into another product | Xet storage API integration | Separate integration path, current API and token references |
| `/#review` | Judge maturity and operating boundaries | openxet benchmarks / security | Current stage, four measured figures with methods, security tests, source |
| `/#agents` | Hand the same task to an agent | openxet agent / JSON CLI | Real quickstart output, error contract, Markdown handoff; preserve `#roadmap` |
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

Each section has one job and answers the question a visitor has at that
scroll depth ([S14](docs/research/S14-landing-messaging.md) has the visitor
model and the research behind the order).

| Section | Question answered | Content / behavior |
|---|---|---|
| Hero | What is this? Is it for me? | Category **Open-source Xet storage server**. H1 **Upload only what changed. Keep it where you choose.** Lead names Hugging Face’s `hf_xet`, unmodified. Run it locally (primary), Join the hosted waitlist (secondary); one status line. |
| Upload trace (hero visual) | Does it really work, and is the saving real? | Stock `hf_xet` → `openxet serve`; to-scale bars of one measured re-upload (100 MiB file, 1 MiB edit, 1.14 MB sent, downloaded back whole); exact bytes and method linked. |
| Compatibility | Does it work with my tools? | Per-tool table: Available or Planned, in text; CI runs the released client. |
| How it works | Why is the second upload small? | Three steps beside the interactive eight-part example. Explain “chunks” in plain words; savings vary. |
| Numbers you can rerun | Is it trustworthy and maintained? | Four measured figures, each with machine/version context and a link to its method; security tests, crash safety, source. |
| Your infrastructure, or ours | Self-host or hosted? Lock-in? | Self-host card first and featured (available); hosted card (coming soon); same open-source code. Integration row below. |
| Same task, by hand or by an agent | Can my agent do this? | Real quickstart output, `--json`, error contract, Markdown handoff. |
| Before you decide | What’s the catch? | Hugging Face affiliation, `hf upload` status, measured savings range (including ~0%), leaving, pricing. |
| Final invitation | What now? | Run it locally and Join the waitlist side by side. |

Desktop composition:

```text
openxet   Compatibility  How it works  Hosting  Docs  GitHub  [Join the waitlist]

Open-source Xet storage server       CLIENT hf_xet  - - -[+]- - ->  SERVER openxet serve
Upload only what changed.            Edited file         [=========|=========] 100 MiB
Keep it where you choose.            Sent to the server  [|                  ] 1.14 MB
Lead naming hf_xet, unmodified.      Downloaded back     [=========|=========] 100 MiB
[Run it locally] [Join the waitlist] Measured … bars to scale. How it was measured

  compatibility → how it works → numbers → hosting / integration
  → agents → questions → final invitation
```

Every two-column layout (hero, how it works, hosting, agents, questions)
splits on the same centre line; section intros are one column on the
content's left edge. On mobile: headline → lead → stacked full-width
actions → trace → remaining sections.

<span id="personality"></span>

## Visual identity: version 2

Calm, capable, open and considerate. The chosen light mineral surfaces and
dark blue ink make explanatory content readable without requiring terminal
fluency. Manrope supplies a compact display voice; IBM Plex Sans and Mono
connect readable prose with exact file data. This is a reasoned direction,
not evidence that one palette converts better than another.

The signature is a visual grammar: aligned labels mean existing content, an
amber plus-marked part means new content, a transfer tray isolates what moves,
and the complete strip remains the file the person works with. Reuse this
grammar in relevant diagrams and future transfer results, never as decoration
on unrelated screens. Real results show actual bytes, not toy part counts.

The lowercase wordmark and four-part symbol are retained because they are
compact and connect to the composition of a file. They may be replaced when
a candidate improves recognition, small-size legibility and product fit.
Compare candidates on those criteria, not novelty or attachment to old work.
Do not borrow Hugging Face's visual identity; credit its Xet protocol.

A dark terminal-led direction and a dense infrastructure blueprint would
privilege experienced operators at the first encounter. Keep their useful
technical detail deeper in the experience. Validate the chosen direction with
the comprehension and navigation experiments below; revise it if it fails.

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

Use a two-column hero above 960px, a single column below it. Compare the two
hosting options side by side only when both fit comfortably; collapse by
768px. At 320px, no horizontal page scrolling, clipped controls or tiny text.
Code blocks may scroll horizontally within a labeled region.

Use alignment to establish hierarchy. Group content about the same decision;
do not put every sentence in a card. Numbers signify a real process, not a
decorative layout. Keep sticky navigation small, and use scroll margins so it
does not cover headings or focused controls.

## Elevation & Depth

Default surfaces have no shadow. Use a 1px separator for ordinary groupings.
The hero's upload trace can use `0 16px 48px rgb(20 34 56 / 7%)`; this is the
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

<span id="components"></span>

## Shared interaction contracts

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

### Measured upload trace

The hero's visual shows one real measurement, never an illustration. A route
row names the stock client and the server; three bars (edited file, sent,
downloaded back) are drawn to scale from the recorded bytes, with the changed
region amber at its true position and the sent bar an empty dashed track
holding only the amber sliver. A caption gives the exact byte count, the
client version and cache state, says “bars to scale”, and links the method.
Update the numbers only from a recorded run, and change HTML, the social
image and the caption together. A 4px minimum width keeps a sliver visible;
nothing else is rounded up. Static; no motion.

### File-version demonstration

Shown in “How it works”, not the hero. The current example uses eight labeled parts: enough to show substantial reuse,
few enough to compare on a phone. This count is an illustration choice, not a
brand requirement or protocol fact. The initial state shows seven reused parts
and one new part, with that new part alone in a **New data to upload** tray.
“Small edit”, “Same file” and “All new” buttons confirm the idea; interaction
is optional. The same-file tray says there is no new file data; the all-new
tray contains every part. Keep the complete updated file visible in all states.

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

- Hosting choices use the same order: name, who operates it, data location,
  availability, action. Integration is a separate dimension, not a hosting tier.
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

### A task that humans and agents can share

A recipe is the unit of handoff, not a bare command or a link to an enormous
design document. Maintain one canonical task page and generate its HTML and
Markdown from the same source. Offer the relevant Markdown link next to the
human instructions; `llms.txt` helps find it but does not replace it.

| Recipe field | What the reader or agent needs |
|---|---|
| Goal and completion | One observable outcome and how to verify it; distinguish an attempted action from a durable result |
| Context and prerequisites | Supported versions, installation, resource, endpoint, working directory and inputs; record the build used when reporting a result |
| Authority | Required scope and where an authorized person obtains a credential; never embed the credential in the shared recipe |
| Action | Exact commands or requests, expected output/schema, whether retries are safe and which steps change state |
| Recovery and limits | Stable error/exit reference, likely corrective action, cancellation and partial-result behavior; no invented `--dry-run` or approval feature |
| Next task and cleanup | A useful next outcome, files left behind and explicit cleanup boundaries; never silently delete existing data |

**Current example:** the [storage-only quickstart](docs/quickstart-storage-only.md)
builds and starts a server, uploads a generated test file with `hf_xet`, downloads
and compares it, then uploads it with `openxet` to demonstrate reuse. It includes
the prerequisites, endpoint setup, version floor and server shutdown. Share that
page as Markdown with the goal “upload the test file, download it and verify
the contents”; authorize access to credentials separately. The example's
executable steps run in CI. It needs no hosted account or future Hub API.

Design a second success into the path: first establish a verified round trip,
then observe actual reuse on the repeated upload. A revised-file experiment
is the next candidate recipe; publish its commands and expected observations
only once tested. Do not convert illustrative percentages into expected output.

On a phone, a useful result may be saving the public recipe for a workstation.
A reviewer should be able to share the specific evidence page. After a waitlist
request, offer the current quickstart or project progress without subscribing
the person to more messages. After a successful task, offer verification or a
related next task; after failure, offer recovery and a reproducible issue report.
Collect only the context needed to help, with credentials and private data removed.

<span id="future-hosted-onboarding-and-humanagent-collaboration"></span>

### Future hosted onboarding and delegated actions

At launch: sign in with an existing external identity → choose workspace/repo
and region when necessary → receive a narrowly scoped credential through the
proper secret channel → run the chosen upload → see a durable success and
actual transferred/stored bytes. Progressive disclosure keeps advanced storage
settings out of the first upload. BYO bucket needs its own permission review.

The same task contract applies to UI, CLI and agents. The future app should
preserve non-sensitive intent across external sign-in and show a durable result
the person can inspect later. “Copy command” copies an authorized instruction,
never a credential. Returning users go to their task or workspace, not back
through marketing onboarding.

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
Headings name the question, decision or task; reserve expressive copy for the
main promise rather than turning every section into a slogan.

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
| Understand the benefit | A beginner points to what uploads, knows the file stays complete, and predicts the same-file and all-new-file cases | Research target; not yet tested |
| Find the right path | Hosted, self-hosting and integration participants find a next action within 30 seconds without treating integration as a third hosting model | Research target; not yet tested |
| Reliable waitlist | Validation, failure, duplicate, cancellation and durable-save checks pass | Automated and live checks per release |
| First hosted success | Landing → completed real `hf upload` under 60s on the documented test rig | Future launch gate; include setup and network assumptions |
| Trust | Capability claims link evidence; no knowingly broken path or hidden availability | Editorial/release review |
| Shared task | A human and an authorized agent independently follow the same recipe and can explain its result and recovery path | Current quickstart has executable CI coverage; handoff usability still needs observation |
| Return value | Users verify a download, then observe reuse on a repeated upload; they can resume from a saved recipe and report a problem that receives a useful response | Requires actual users; not inferable from clicks |

Do not optimize waitlist conversion at the cost of understanding or consent.
Do not equate a scanner score with accessibility, trust or love. Use manual
research until optional, approved measurement exists. Publish small changes,
record their reason and evidence, and stop an iteration when remaining ideas
need user research rather than pretending no further improvement is possible.

### Ownership, maintenance and intentional change

1. Before a website/docs/UI change, read this file and the relevant product
   references. Check the distinction between durable requirements, current
   expression and future capabilities; reuse the adopted tokens and contracts.
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
- 2026-10-24, v2: reconsidered the website without preserving its old layout
  or logo by default. Independent critique found that two full file rows did
  not show the smaller transfer, and integration was misclassified as hosting.
  Added a transfer tray, separated those decisions, made trust links answer
  operating questions, and specified shared task recipes and a second success.
  Kept the palette/type direction for readability and semantic consistency;
  comprehension and recognition remain hypotheses requiring participant tests.
- 2026-10-24, resource-address contract: the founder selected `xet://` for
  future resource addresses (ADR 0009). M2-URI and M6-URI schedule the grammar,
  compatibility checks and client/UI delivery. HTTPS links and current examples
  stay in place until support ships; no visual identity or runtime changes here.
- 2026-10-25, v3 messaging (iteration 1): the owner found the opener,
  order and hero visual weak and the alignment off. A teardown of 19
  landing pages and a visitor model ([S14](docs/research/S14-landing-messaging.md))
  showed the page answered “what is dedup?” before “what is this, and does
  it work with my tools?”, and asked for a waitlist before any proof.
  Changed: opener anchored on Xet and the stock client; hero visual is now
  a measured, to-scale upload trace (the toy example moved to “How it
  works”); compatibility and measured numbers come before the hosting
  choice; the working quickstart is the primary action while hosting is a
  waitlist; one-column section intros and a shared centre line fix the
  misaligned grid edges. Tokens, type and palette unchanged. Evidence:
  screenshots at 375, 768, 1024, 1440 and 1920 px in light and dark were
  reviewed before and after; attach them to the PR. Comprehension still
  needs participant tests.
- Future changes append date, reason, impacted tokens/components, evidence and
  migration scope here. Avoid duplicating the token definitions in prose elsewhere.

Research and source details: [website design research](docs/research/website-design-2026-10-24.md).
Implementation sequence: [website release plan](docs/superpowers/plans/2026-10-24-website-design.md).
