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
This file never turns a target into an available feature. The landing page's launch framing is an owner's ruling
(see availability below), not an availability claim made here.

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
an illustration: the proof strip links each figure to its method, and “How it works” puts a measured, to-scale
bar pair under the illustrative comparison.

Anchor the opener on what the visitor already knows: Git. The H1 (“Git for
large files. Only the changes upload.”) answers “what is it, and why do I
care?”, and its second half is the contrast with Git LFS; the pill carries
the brand and open source, and the subhead names repositories, buckets,
`git push` and `hf upload`. Keep the page generic: models
and datasets are examples beside logs, tables, source and archives
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
| Newcomer | What does it do, in words I know? | Hero, proof strip, “How it works” comparison | Understand the example |
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
**Owner's ruling, 2026-10-25: the landing page is written as if the full
planned product (design §12) has launched.** The owner chose this over
availability badges. Limits that stay: no invented numbers or command output,
no logos, customers or testimonials, and every call to action lands on what
works today. Sections about features without a measured run show commands or
diagrams only. Docs, `llms.txt` run steps and agent-facing material stay
strictly factual about what runs, because agents execute them. Other pages
(waitlist, privacy, about) keep their factual availability.

**Button hierarchy (coordinator ruling, owner may override):** the one filled
button is **Get started**, which lands on the self-hosted quickstart, the path
that works today. **Start on openxet.com** is outlined, in the header and
beside the primary, and lands on the hosted waitlist form until sign-up
launches.

| State | Primary action | Identity / pricing | Technical truth |
|---|---|---|---|
| **Current: waitlist** | Get started → quickstart (filled). Start on openxet.com → `/waitlist/` (outlined, in the header and beside the primary) | No active hosted sign-up, sign-in or checkout; no invented prices | M1 storage-only server/client works; Hub/Git and hosted service remain planned; the landing page is written as launched (ruling above) |
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

Current navigation: **Repositories · Buckets · Self-hosting · Platforms · Agents · Docs · GitHub · Start on openxet.com** (outlined). Below 1088px the links move into the menu.
Keep it short. On mobile, show the brand and waitlist action with a keyboard
accessible disclosure for the remaining links; do not hide essential paths.

| Destination | Job | Search intent | Present implementation |
|---|---|---|---|
| `/` | Explain value, demonstrate reuse, route the visitor | open-source large-file storage; Xet storage | Homepage |
| `/#use-cases` | Set expectations for any file type | what deduplication saves | Measured pairs from S12, zeros included |
| `/#compatibility` | Why not S3, Git LFS or the Hub; which clients and backends work | hf_xet self-hosted server | Fan-out with status pills |
| `/#how-it-works` | Make the mechanism understandable | how file deduplication works | Explanation and example; preserve old `#dedup` links |
| `/#hosting` | Choose who operates the storage | hosted or self-hosted Xet | Hosted and own-infrastructure paths; preserve `#hosted` |
| `/#integrate` | Fit storage into another product | Xet storage API integration | Separate integration path, current API and token references |
| `/#security` | Check each security boundary against its test | openxet security | Four cards naming M1 tests; preserve `#review` |
| `/#agents` | Hand the same task to an agent | openxet agent / JSON CLI | Captured `--json` output and error with hint; preserve `#roadmap` |
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

Hugging Face Storage's rhythm, generically: a centred hero that fits the
first screen at 1440×900 with its proof strip, seven framed two-column rows,
a slim security band and three tiles
([S14](docs/research/S14-landing-messaging.md#iteration-3-the-release-candidate)).

| Section | Job | Content |
|---|---|---|
| Hero | What is it, why care, is it real | Pill **OpenXet · open source · Apache 2.0**; H1 **Git for large files. Only the changes upload.**; subhead naming repositories, buckets, `git push`, `hf upload`, the Git LFS contrast, self-host or openxet.com; Get started (filled), Start on openxet.com (outlined); proof strip of four measured figures as the bottom edge. No hero visual |
| Repositories (blue) | Do my tools work? | Commands only: `git clone`, `git push`, `hf upload`, `openxet label add` against an OpenXet endpoint |
| Xet (amber) | Why is it better than LFS or S3? | Toggle above whole-file 8 / 8 against Xet 1 / 8, then the measured pair to scale (100 MiB against 1.09 MiB) naming its CI test |
| Use cases (green) | Will it help my files? | S12's measured pairs, zeros included |
| Buckets (lilac) | Where do checkpoints and logs go? | `hf buckets create`, `hf sync`, and a key list with labels and no sizes |
| Self-hosting (slate) | Does it fit our stack and rules? | Identity, stateless nodes, storage backends as a stack diagram |
| Platforms (peach) | Can I embed it under my brand? | Four-step flow: token, upload, authorizer, metering |
| Agents (blue) | Can my agent use it safely? | A captured `openxet.error.v1` with its hint, then the corrected command |
| Security band | Is it safe? | Four cells, each linking its named test |
| Get started | Next action per persona | Run it yourself · OpenXet Cloud · Build it into your platform |

Row grammar: left cell has a pill tag, a benefit H2 (sentence case, with a
full stop), at most two sentences and three check bullets; right cell is a
panel in the row's hue with one visual. Both cells share one top padding, so
every pill sits the same distance below its hairline. Rows share the frame's
rails (the same 75 rem edge as the header and footer) and stack below 960px
with the visual after the copy. **One measurement everywhere:** the landing
page, social image and `llms.txt` quote the edit upload only from
`site/measurements.json`; `site/test_measurements.py` and the compat test it
names keep them equal.

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

The wordmark is **OpenXet** (owner's ruling, 2026-10-25): the brand is
written OpenXet everywhere it names the product, and lowercase `openxet`
stays only for identifiers (the command, crates, `OPENXET_*`, paths, schema
ids, the openxet.com domain and URLs). With two capitals, the wordmark's
tracking opens from -0.04em to -0.03em so “nX” does not crowd. The four-part
symbol is retained because it is compact and connects to the composition of a
file. They may be replaced when
a candidate improves recognition, small-size legibility and product fit.
Compare candidates on those criteria, not novelty or attachment to old work.
Do not borrow Hugging Face's visual identity; credit its Xet protocol.

A dark terminal-led direction and a dense infrastructure blueprint would
privilege experienced operators at the first encounter. Light terminal cards
in the page's own tokens are allowed below the hero copy and inside sections,
as evidence of a real run; never as the page's identity or above the headline. Validate the chosen direction with
the comprehension and navigation experiments below; revise it if it fails.

## Colors

Use semantic CSS variables, never ad hoc colors in a component. These are the
normative values. Functional colors carry stable meanings; new/changed data is
amber, interaction is blue, success is green, failure is red.

Row panels take one hue each, light and dark, so sections keep an identity at
scroll speed; elevation in dark mode runs frame < panel < card.

```yaml
panels:
  card: "#FFFFFF"
  blue: "#EEF2FF"
  amber: "#FFF8E6"
  green: "#EDF8F2"
  lilac: "#F4F0FD"
  slate: "#F0F3F8"
  peach: "#FFF2EB"
  dark-card: "#263A5E"
  dark-blue: "#213357"
  dark-amber: "#2E3349"
  dark-green: "#1F3647"
  dark-lilac: "#2A3158"
  dark-slate: "#223249"
  dark-peach: "#30334A"
```

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
| Wordmark | Manrope | 24px / 1.1 | 750 / -0.03em |
| Hero | Manrope | clamp(44px, 5.4vw, 76px) / 1.04 | 650 / -0.045em |
| Section heading | Manrope | clamp(30px, 3.5vw, 44px) / 1.15 | 650 / -0.035em |
| Component heading | Manrope | 22px / 1.25 | 650 / -0.02em |
| Lead | IBM Plex Sans | clamp(18px, 1.6vw, 21px) / 1.55 | 400 / normal |
| Body | IBM Plex Sans | 17px / 1.65 | 400 / normal |
| Controls / navigation | IBM Plex Sans | 15 to 16px / 1.35 | 500 to 600 / normal |
| Captions / metadata | IBM Plex Sans | 14px / 1.5 | 400 to 500 / normal |
| Code / file data | IBM Plex Mono | 13 to 14px / 1.6 | 400 / normal |

Use rem units in implementation, assuming a 16px browser default; never prevent
zoom. Body text is at least 16px on mobile. Prose width is 60 to 68 characters;
hero copy 42 to 48 characters. Headings use sentence case. Reserve uppercase for
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
The landing page has no elevated object; panels and cards separate by hue
and border. A future hero visual may use `0 16px 48px rgb(20 34 56 / 7%)` as
the single elevated object. Dialogs, when needed in the future app, use
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

File parts use 4 to 6px corners. Controls use 8px corners. Rounded pills are only
for short status labels and section tags. Use a consistent 20px icon box, 1.75px stroke and
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
One primary action per decision, and at most one filled button in any
screenful: the header action is outlined. On the landing page “Start on
openxet.com” names the hosted action and “Get started” the self-hosted
quickstart; the waitlist page itself keeps “Join the waitlist”.

### Terminal card

A light card in the page's tokens: a bar with three muted dots and a label,
the transcript in IBM Plex Mono, and a caption. Output is **captured from a
real run, never composed**; a card for a feature without a measured run shows
commands only and says so. Mark what was edited (comments added, tables omitted,
hashes shortened), state the date, build and machine, and link how to
re-capture it ([S14](docs/research/S14-landing-messaging.md#re-capturing-the-terminal-transcripts)).
Prompts and comments are muted and not selectable; one key result may be
highlighted in primary. Lines wrap rather than hide a result on narrow
screens. Static; no typing animation.

### Proof strip

Four measured figures in equal cells forming the bottom edge of the hero, in
the first screen at 1440×900, each a link to its method. Figure, label and link sit on a three-row grid so links align
across cells. Use one unit system per figure and never repeat a figure
elsewhere in the hero.

### File-version demonstration

Shown in “How it works”. A whole-file upload (S3 PUT, Git LFS) sends 8 / 8
parts; the Xet upload beside it sends only the changed ones. Eight parts are
enough to show substantial reuse and few enough to compare on a phone; the
count is an illustration choice, not a protocol fact. “Small edit”, “Same
file” and “All new” buttons change the Xet row; the whole-file row stays
8 / 8 by design. Under it, one measured bar pair, to scale, makes the real
size of the saving visible.

Keep equal toy parts explicitly illustrative. Real chunks vary in length;
file formats, edits, compression, client caches and metadata affect transfer.
Do not claim a 1-byte edit always transfers one byte, imply every training
checkpoint mostly repeats, or present a toy percentage as a benchmark.
Use “parts reused” and “new parts to upload” rather than a fake live speed.

Sent parts carry a plus mark and a dashed border as well as amber fill. Announce a
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

No em dashes or en dashes in any public copy: the site, docs, help text,
errors, commit messages and PR text (owner ruling, 2026-10-25). Use a full
stop, a comma, a colon or parentheses; write ranges with "to". Never stand a
hyphen in for a dash. Copy must not read as machine-written: avoid stock
words ("robust", "leverage", "unlock", "empower", "elevate", "delve", "in
today's world"), "not just X but Y" turns, padded lists of three and the same
sentence shape in every section. `scripts/check-copy.py` checks the dashes and
a short phrase list in CI.

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
- 2026-10-25, v3 iteration 2: the owner asked to be closer to Hugging Face
  Storage's page, generically. Adopted its structure: a centred hero, one
  framed two-column grid with a pill, H2, two sentences, three checks and one
  visual per row, and light terminal cards. Retired the hero trace (it failed
  the three-second test) for a captured terminal card plus a proof strip.
  Added use cases (S12 pairs, zeros included), a compatibility fan-out, a
  security section and a three-door closing. Button hierarchy: one filled
  button per screen; the header waitlist is outlined. Pills now also serve as
  section tags. Tokens, type and palette unchanged. Evidence: screenshots at
  375, 768, 1024, 1440 and 1920 px in light and dark were reviewed; attach
  them to the PR.
- 2026-10-25, v3 iteration 3 (release candidate): owner's rulings: the
  brand is OpenXet; the landing page is written as launched (limits above);
  the Git analogy as opener. Hero fits the first screen with its proof strip
  and has no visual; rows reshaped to Repositories, Xet, Use cases, Buckets,
  Self-hosting, Platforms, Agents, a security band and tiles; per-row panel
  hues and a dark elevation fix; rows top-aligned; one measurement backed by
  `site/measurements.json`. Wordmark tracking -0.03em. Screenshots at 375 to
  1920 px, light and dark, reviewed; attach them to the PR.
- Future changes append date, reason, impacted tokens/components, evidence and
  migration scope here. Avoid duplicating the token definitions in prose elsewhere.

Research and source details: [website design research](docs/research/website-design-2026-10-24.md).
Implementation sequence: [website release plan](docs/superpowers/plans/2026-10-24-website-design.md).
