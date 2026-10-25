# S14: Landing page messaging, order and hero

<!-- description: Teardown of 19 comparable landing pages, a visitor model by scroll depth, and the resulting section order, opener and hero visual for openxet.com. -->

Date: 2026-10-25. Baseline: `website/index.html` at `22e0908`. This is
iteration 1 of several; an independent critic reviews it next. The
canonical decisions land in [the design system](../design.md); this page
keeps the evidence and the reasoning behind them.

The owner's brief: the design language stays, but the messaging, the
order of arguments, the opener, the hero visual and the visual alignment
need work. Which assumptions does a visitor have at which point, where
does the page answer each, and what is each section for?

## What was wrong with the page

Read top to bottom, the previous page said:

1. **Large files. Lighter uploads.** True of every dedup product,
   including huggingface.co itself. Nothing said openxet is a *server*,
   that it is *Xet*, or that Hugging Face's own client works with it.
2. An eight-part toy diagram, labelled illustrative, as the hero. It
   taught dedup, which Hugging Face's own Storage page already teaches,
   and showed nothing openxet does.
3. Three steps of dedup.
4. Hosting choices, with the waitlist (a service that does not exist yet)
   as the featured card and the primary action.
5. Only in the fifth section: what works today, and that `hf_xet` works.
6. Evidence as three links with no numbers, though the repository has
   measured numbers with methods.

So the differentiator (an open-source Xet server that stock clients talk
to) and the proof (measured bytes) both arrived after the visitor had
been asked to join a waitlist. The page answered "what is dedup?" first,
a question most arriving visitors do not have.

## Research: 19 landing pages

Accessed 2026-10-25 with WebFetch, and `curl` where pages render
client-side. XetHub and Oxen through the Wayback Machine (captures
2024-02-08 and 2026-08-26). Supabase, Neon and Turso serve WebFetch a
separate agent-oriented Markdown page; the human HTML was checked with
`curl`. Long subheads were cut by the fetch tool where marked "…".
Observations describe the pages, not their conversion.

| Site | Opener (H1 / subhead) | Order and argument | Proof, and where | CTA, self-host vs hosted | Hero visual | Lesson for openxet |
|---|---|---|---|---|---|---|
| [HF Storage](https://huggingface.co/storage) | "Storage Buckets" / "Store models, datasets, and artifacts with simple per-TB pricing." | Logos → fit for ML → dedup ("only changed bytes") → pricing vs S3 → data independent of compute → CDN → agents → security | Logos in hero; a terminal printing dedup savings in section 2; 8/8 vs 1/8 chunk diagram | "Create a Bucket"; hosted only | Text + logo strip; first strong visual is the dedup terminal | The terminal that prints savings and the before/after chunk diagram; no unmeasured "trusted by" |
| [git-lfs.com](https://git-lfs.com) | "An open source Git extension for versioning large files" | Download → how it works → 3 commands → open source → features | Version number and runnable commands only | "Download v3.8.1" | Pointer diagram | Saying what it is plus "open source" in the H1 is enough for a tool |
| [dvc.org](https://dvc.org) | "Data Version Control" / "Manage data the way code is managed…" | Webinar → lakeFS upsell → DVC → audiences → stars → logos | Stars and logos far below | "Try lakeFS", "Book a Demo" | None | Never let a hosted upsell take over the open-source hero |
| [lakefs.io](https://lakefs.io) | "The Control Plane for AI-Ready Data" | Logos → whitepaper → four values → case study → quotes | Stars in header; quotes with numbers | "Book a Demo" | Lottie animation | A buzzword H1 says nothing about the mechanism |
| [oxen.ai](https://web.archive.org/web/20260826/https://www.oxen.ai/) | "From first prompt to production." | Logos → organize → collaborate → train → scale (benchmark) → "Your infrastructure or ours" | Logos; "20x faster than git-lfs" with "View benchmarks" mid-page | "Get started"; self-host only in a late "Open Source" item | Model carousel | "Your infrastructure or ours" frames the choice in four words |
| [min.io](https://min.io) | No H1; title "Data and Memory Foundation for Enterprise AI" | News → workloads → logos → stats → architecture | Unsourced numbers mid-page; open-source stars last | "Learn more" | None | Stats without method, and an open-source story moved to the footer, cost trust |
| [tigrisdata.com](https://tigrisdata.com) (agent view) | "Storage for Any Cloud" / "S3-compatible object storage for any cloud…" | What it is → get started → MCP → resources | "over 90%" of the S3 API; install code | Text links; hosted only | None served | An agent path (MCP, llms.txt, env vars) beside the human pitch |
| [Cloudflare R2](https://www.cloudflare.com/developer-platform/products/r2/) | "R2" / "Build stateful applications…" | Features → benefits (S3-compatible, progressive migration) → use cases | Brand, zero egress | "Get Started"; hosted only | None | "Existing tools work without rewrites" as a headline benefit |
| [XetHub](https://web.archive.org/web/20240208/https://xethub.com/) | "Versioning built for ML Teams" (animated) / "Bring your ML development into focus…" | Terminal → logos → audiences → ecosystem → quote → blog | The hero is a real `git-xet` push of Mistral-7B | "Explore projects"; hosted only | Real terminal session | The native visual for Xet is a real transfer; its own demo printed "0.0% reduction", so show one where dedup fires |
| [forgejo.org](https://forgejo.org) | "Forgejo" / "Beyond coding. We forge." | Values → six feature cards → get involved | Almost none | "Get Forgejo", "Try now"; one hero sentence splits self-host and Codeberg | Mascot | A slogan H1 explains nothing |
| [about.gitea.com](https://about.gitea.com) | "Private, Fast, Reliable DevOps Platform" | **Choose how you run it** (2nd) → counters → logos → … → pricing toggle | Counters under hero | "Get started", "Start your 30-day free trial" | Deployment illustration, then app | Put the deployment choice early; "reuses your GitHub Actions" as compatibility proof |
| [tailscale.com](https://tailscale.com) | "The best secure connectivity platform for the AI era" | Use cases → proof bar → easy install → tweets | Named outcome numbers under hero | "Start connecting devices"; open source only in footer | Text + tiles | Named, linked outcome numbers |
| [supabase.com](https://supabase.com) | "Build in a weekend / Scale to millions" / "Start your project with a Postgres database…" | Products → logos → dashboard → … → **"Open source from day one… never locked in"** | Stars in nav; logos early | "Start your project"; self-host as a lock-in guarantee near the end | Product tiles | Anchor on what people know ("Postgres"); open source as the no-lock-in promise |
| [neon.com](https://neon.com) | "The backend for apps and agents, built to scale on Lakebase Postgres." | Primitives → coding agents → scale → architecture → pricing | Corporate backing; live counters | "Get started"; closing CTA is a copyable `npx neon init` | Client-rendered | A command as the CTA; a Markdown homepage for agents |
| [turso.tech](https://turso.tech) | "Millions of Databases. One Architecture." / "Built on SQLite…" | Logos → problem → properties → code → stories → **OSS "Install locally" beside "Try Cloud free"** | Logos under hero; numbered stories | Equal cards near the end | None verifiable | Problem → properties → code → proof → choice is the infrastructure narrative |
| [posthog.com](https://posthog.com) | "Your product's context layer" | Setup (terminal `npx`) → labelled "Illustrative demo" → agents → pricing → why | "500,000+ teams" in hero | "Get started – free"; no self-host text found | Scripted demo, labelled illustrative | Honest labels and voice build affinity |
| [zed.dev](https://zed.dev) | "Your last next editor" / "Zed is a minimal code editor crafted for speed and collaboration with humans and AI." | Mockups → named quotes → features → AI → founders' letter | Famous developer quotes under hero | "Download now" **beside "Clone source"** | Product UI | The source CTA as a peer of the product CTA |
| [uv](https://docs.astral.sh/uv/) | "uv" / "An extremely fast Python package and project manager, written in Rust." | **Benchmark chart with method** → highlights ("replace pip…") → install → "the pip interface: drop-in" | Benchmark first, captioned with its workload | The install command | Benchmark chart | A measured claim with its method, first; then the commands you already know |
| [ollama.com](https://ollama.com) | "Ollama is the most popular way to build with open models." / "…locally or in the cloud." | Logos → three usage stats | Stars in nav; usage numbers | "Get started" | None | Local vs cloud as one phrase; a superlative needs numbers we don't have |

### Patterns that hold for an open-source server with a hosted option

1. **Anchor the opener on something the visitor already knows.** Postgres
   (Supabase, Neon), SQLite (Turso), S3 (Tigris, R2), pip (uv), Git
   (git-lfs). Abstract slogans (Forgejo, lakeFS, MinIO) explain nothing.
   For openxet the anchor is Xet and Hugging Face's client.
2. **The first proof is measured and carries its method.** uv opens with
   a captioned benchmark; HF Storage and XetHub show a real transfer.
   Without customers to name, a reproducible measurement is our proof.
3. **Compatibility is shown as the tools people already run,** not
   claimed as a percentage (uv's pip interface, Gitea's Actions, R2's S3
   APIs). Our version: which stock client works today, which is next.
4. **The working open-source path is the primary action when the hosted
   one is not live.** No reviewed page made a pre-launch waitlist its
   main hero action. DVC shows what an upsell in the hero costs.
5. **Self-host vs hosted is a short, equal choice** ("Your
   infrastructure or ours", Turso's paired cards), framed as one
   journey, and open source doubles as the no-lock-in promise (Supabase).
6. **Agents get a peer path,** not a footnote (Tigris, Neon, HF Storage).
7. **Honest labels build trust** (PostHog's "Illustrative demo", Zed's
   scoped beta banner, uv's captioned workload).

## Visitor model

Who arrives, from where, and what they bring.

| Visitor | Typical entry | Knows | First doubt |
|---|---|---|---|
| Self-hoster, ML or data team | Search ("self-host xet", "huggingface private storage"), HN, GitHub | HF tools, maybe Xet; cannot or will not put data on huggingface.co | Is this a real server my tools talk to, or a toy? |
| Platform engineer (aqora-like) | GitHub, a referral, the docs | Storage, auth, tenancy | Can it sit under my product's names and permissions? |
| ML engineer, curious | HN, a Hugging Face context (blog, forum) | `hf` CLI | Does `hf upload` work against it? |
| Newcomer from a shared link, often on a phone | Social, chat | Little | What is this, in plain words? |
| Reviewer or buyer | An internal link | Their requirements | Is it maintained, measured, secure? |
| AI agent | `llms.txt`, a docs link, a browser tool | Whatever the page says | What works, how do I call it, how do I recover? |

What each assumes and doubts as they scroll, and where the page answers:

| Depth | Question in the visitor's head | Section that answers | Evidence it uses |
|---|---|---|---|
| 0–3 s | What is this? | Hero category, H1 | "Open-source Xet storage server"; "Upload only what changed. Keep it where you choose." |
| 3–10 s | Is it for me? Does it work with my HF tools? | Hero lead and visual | Names `hf_xet` "unmodified"; the visual shows that client talking to *your* server |
| 10–20 s | Is the saving real, or a marketing diagram? | Hero visual caption | 1,141,116 bytes measured, method linked, bars to scale, download byte-identical |
| First scroll | Which tools exactly, today versus later? | Compatibility | Available / Planned per tool, in text; CI runs the real client |
| | Why would the second upload be small? (newcomers) | How it works | Three steps plus the interactive eight-part example; savings vary |
| | Is it trustworthy, maintained, fast? | Numbers you can rerun | Four measured figures, each linked to machine, versions and command; security tests, SIGKILL test, source |
| | Self-host or hosted? Lock-in? | Your infrastructure, or ours | Same open-source code; operator and data location per card; status in text |
| | Can I build it into my product? | Integration row | Storage API, platform-signed tokens |
| | Can my agent do this? | Same task, by hand or by an agent | Real quickstart output, `--json`, error contract, Markdown handoff |
| | What's the catch? | Before you decide (FAQ) | HF affiliation, `hf upload` status, savings with measured ranges including ~0%, leaving, pricing |
| End | What now? | Final invitation | Run it locally (primary), join the waitlist (secondary) |

## Section order, one job each

| # | Section | Its one job | Changed from |
|---|---|---|---|
| 1 | Hero | Say what it is, who it's for, and show it working with a stock client, measured | Was dedup-in-general with a toy diagram |
| 2 | Keep the client you already use (`#compatibility`) | Settle "does it work with my tools?" with a dated, per-tool status | Was section 5, prose |
| 3 | Why the second upload is small (`#how-it-works`) | Teach the mechanism to those who need it | Was section 2; now after the proof, with the toy example moved here |
| 4 | Numbers you can rerun (`#review`) | Earn trust with measured, reproducible figures | Was three unnumbered links |
| 5 | Your infrastructure, or ours (`#hosting`, `#integrate`) | Make the operator choice; open source as no lock-in | Waitlist was the featured card; now self-host is featured because it works today |
| 6 | Same task, by hand or by an agent (`#agents`, old `#roadmap`) | Show the human and agent interface on real output | Was mixed with the roadmap |
| 7 | Before you decide (FAQ) | Answer the catches honestly | Added "can I use `hf upload`", "what if I leave"; savings now cite measured ranges |
| 8 | Final invitation | One clear next step for both paths | Was waitlist-only |

Old anchors `#dedup`, `#hosted`, `#integrate`, `#review` and `#roadmap`
keep working.

## Opener alternatives

| # | Headline | For | Against |
|---|---|---|---|
| A | **Upload only what changed. Keep it where you choose.** (picked) | Benefit a newcomer understands without knowing "Xet"; the second line is the differentiator (choice of operator), true today for self-hosting and the stated plan for hosted | Two sentences, four lines at desktop; "Xet" only in the category line and lead |
| B | Hugging Face's Xet storage, on your own servers. | Strongest anchor for the HF-aware visitor; explicit | Leans on HF's name in the H1 (affiliation risk); excludes the hosted path; jargon for newcomers |
| C | The open-source Xet server. | Short, exact, the git-lfs pattern | Means nothing to someone who doesn't know Xet; no benefit |
| D | Your Hugging Face tools. Your storage. | Very strong if it were true | `hf upload` is not available until M2: an overclaim today. Revisit when the Hub API ships |
| E | Large files. Lighter uploads. (previous) | Simple, friendly | Generic dedup, which huggingface.co already offers; no differentiator; no category |
| F | Run Xet storage anywhere. Send only the changes. | Names both | "Anywhere" is not true yet (local disk only) |

Pick A, with the anchor carried by the category line ("Open-source Xet
storage server") and the lead (names Hugging Face's `hf_xet`,
"unmodified"). This follows pattern 1 within one glance without putting
HF's name in the headline. **When `hf upload` works against openxet (M2),
re-test D**: the stock `hf` CLI is the strongest anchor there is.

## Hero visual

**It must make obvious in three seconds:** a tool I already know
(Hugging Face's client) talks to a server I run, and only a sliver of a
large file crosses the wire, while the whole file comes back.

**Concept, picked:** a measured upload trace. A route row (`hf_xet`,
"Hugging Face's, unmodified" → `openxet serve`, "On your machine") sits
above three to-scale bars from one real test
(`compat/tests/test_storage_only.py::test_fresh_cache_edit_bytes_recorded`,
recorded in the [competitive matrix](../competitive-matrix.md#2026-10-24-openxet-at-m1)):

- Edited file, 100 MiB, with the 1 MiB edit marked amber at its true
  position and width (1%).
- Sent to the server, 1.14 MB: an empty dashed track with an amber
  sliver of its true width (1.09%).
- Downloaded back, 100 MiB, whole (the test checks the sha256).

The caption gives the exact byte count, the client version, the empty
cache, "bars to scale", and links the method. It keeps the design
system's grammar (blue stored, amber new, the whole file stays whole)
and the single elevated object.

**Alternatives considered:**

| Alternative | Why not (now) |
|---|---|
| The previous eight-part toy diagram | Teaches generic dedup, shows nothing openxet-specific, and is illustrative where we have a measurement. Kept, interactive, in "How it works" where teaching is the job |
| A real terminal session (XetHub, HF Storage pattern) | The most native visual, and real output exists (`Uploaded 0 B of 4.0 MiB (100% deduplicated).`). But the design system rejects a terminal-led first impression for newcomers, and 0 B for an identical file is less telling than an edit. Used in the agents section instead |
| Animated chunks flying client → server | Motion adds no information, needs reduced-motion handling, and invites fake timing |
| Architecture diagram (client, server, bucket, index) | Answers an operator's later question, not the first one |
| A `hf upload` terminal | Not true until M2 |

## Alignment fixes in iteration 1

From screenshots of the previous page at 375–1920 px:

- Section intros split into a heading column and a paragraph column at
  50%, while the grids below split at thirds; nothing shared an edge.
  Intros are now one column on the content's left edge, and every
  two-column layout (hero, how it works, agents, FAQ, hosting) splits on
  the same centre line.
- The FAQ's summary used flex, so inline `code` broke the line into
  three flex items; summaries now wrap their text in one element.
- Hero buttons at 375 px were two different widths on separate lines;
  they now stack at full width below 576 px.
- Orphans in the lead, availability line and headings: `text-wrap:
  pretty` on paragraphs, balanced H1 lines per sentence.
- Proof links next to their paragraph squeezed it at 1024 px; they now
  sit below it.

## Open questions for the next iteration

- **Primary hero action.** This iteration makes "Run it locally" primary
  and the waitlist secondary (the header keeps "Join the waitlist"). That
  bends design.md's waitlist-first state; is that the owner's intent?
- **Mobile hero.** The visual sits below the fold at 375 px, after the
  actions. Is a compact version of the bars worth showing earlier?
- **Four-line H1 at desktop.** Shorter alternatives lose either the
  benefit or the differentiator.
- **Proof density.** Four figures plus the hero's measurement repeat
  1.14 MB twice; would a different fourth figure (the 1 GiB round trip,
  SIGKILL survival) serve better?
- **Not tested with people.** The ten-second comprehension test in the
  [website research](website-design-2026-10-24.md#the-early-understanding-experiment)
  still has not been run.
