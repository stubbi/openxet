# S13: Hosted openxet.com, features and pricing

<!-- description: Competitor prices and sentiment, the hosted 1.0 feature set, a pricing proposal with unit economics, agentic-age positioning, the first-visit journey and the decisions left to the user. -->

- Date: 2026-10-24. Baseline repository: `779370e` (end of M1).
- Status: **research and proposal.** The pricing (§3), the positioning
  (§4) and every number in them are proposals for the user to approve.
  Nothing here is decided, and nothing here is published on openxet.com.
- Inputs: the [design](../superpowers/specs/2026-10-23-openxet-design.md)
  (§1, §1a, §7.1, §12, §15, §16), [ADR 0008](../adr/0008-hosted-waitlist.md),
  the [website research](website-design-2026-10-24.md) and the
  [competitive matrix](../competitive-matrix.md). Spike S12 (buckets and
  a possible S3 API) runs in parallel; where this note depends on it, it
  says so.
- Labels: **Verified** means read on the vendor's own page or source on
  the access date, or measured. **Estimated** means our assumption or
  calculation, with the inputs stated. **Unverified** means only a
  third-party source was found. All URLs were accessed on 2026-10-24.
  Prices change; re-check before a pricing page quotes any of them.

## Summary

- **The opening.** Hugging Face bills private storage by logical bytes
  except on Enterprise, where billing is dedup-based (Verified, HF Storage
  Buckets docs). Every object store bills bytes as written. Nobody sells
  dedup-aware billing to individuals and small teams. openxet already
  meters logical and physical bytes as a core feature (design §5.7, §1a),
  so it can bill **physical bytes after dedup on every tier** and show
  the split everywhere.
- **Recommended tiers** (§3.3): Free $0 with 100 GB stored and no card;
  Pro $9 per user per month with 1 TB stored; Team $20 per user per month
  with 1 TB stored per seat, pooled; Enterprise from $50 per user per
  month, billed annually. Extra storage is $15 per TB-month of physical
  bytes on every paid tier. Downloads are included up to 5× stored bytes
  per month. There are no request fees, and **agents are never seats.**
- **Unit economics** (§3.5): on Hetzner Object Storage in the EU or
  Backblaze B2 in the US, storage costs about $7 per TB-month (Verified
  list prices). Gross margin is about 65–85% at expected use; a fully
  used tier ranges from a 13% loss (Pro) to about 50% (Team) (Estimated). Cloudflare R2 at $15 per TB-month would
  break the $15 overage price, so R2 suits a cache or CDN tier, not the
  primary pool.
- **Recommended positioning** (§4): *"Hugging Face–compatible storage
  where every new checkpoint costs only its changed bytes, and every agent
  gets a key to exactly one repo."*
- **The core already has** the storage-only CAS, dedup that holds across
  machines, `--json` output with versioned schemas, stable exit codes,
  structured errors with a hint, scoped and expiring CAS tokens, and a
  generated `llms.txt` (§2). **Hosted 1.0 still needs** the Hub API (M2),
  OIDC and identity-bound access tokens (M3), metering, quotas and the
  audit log (M4), pools and residency (M5), the web UI, MCP and approval
  gates (M6), and the hosted-only operational layer: billing, sign-up,
  abuse handling and the status page (proposed **M8**). Design §12 lists
  milestones up to M7 only. M8 is this note's name for the hosted
  milestone in the user's mandate.

## 1. Competitor teardown

### 1.1 Prices

Prices are list prices for the cheapest standard tier in the US or EU, in
the vendor's currency, without tax. "TB" is the vendor's unit. Some
vendors mean 10¹² bytes and some 2⁴⁰. Tigris and GitHub say 2³⁰ for GB.

| Product | Storage / month | Egress | Requests | Free tier | Label and source |
|---|---|---|---|---|---|
| **Hugging Face Hub**, private repos and buckets | $18/TB pay-as-you-go above the included amount; $16 from 50 TB, $14 from 200 TB, $12 from 500 TB | "Egress and CDN included at no extra cost". Buckets: "up to 8:1 of your total storage" | None listed | Free user or org: 100 GB private, "best-effort" public | Verified: [pricing](https://huggingface.co/pricing), [storage limits](https://huggingface.co/docs/hub/storage-limits), [storage](https://huggingface.co/storage) |
| Hugging Face, public repos | Public add-on $12/TB ($10/TB at 50 TB) | Included | None listed | Best-effort; PRO "up to 10TB included" | Verified: [storage limits](https://huggingface.co/docs/hub/storage-limits) |
| Hugging Face plans | PRO $9/month (1 TB private); Team $20/user/month; Enterprise $50/user/month (1 TB private per seat; Team adds 12 TB public base, Enterprise 200 TB) | as above | as above | as above | Verified: [pricing](https://huggingface.co/pricing), [storage limits](https://huggingface.co/docs/hub/storage-limits) |
| Hugging Face billing basis | Logical bytes, except "Enterprise plans benefit from dedup-based billing, where shared chunks across files directly reduce the billed footprint" | | | | Verified: [Storage Buckets docs](https://huggingface.co/docs/hub/storage-buckets#pricing) |
| **GitHub Git LFS** | $0.07/GiB (≈ $72/TiB) above the included amount | $0.0875/GiB, billed to the repo owner for every download | None | 10 GiB storage and 10 GiB bandwidth (Free, Pro); 250 GiB each (Team, Enterprise Cloud) | Verified: [GitHub docs](https://docs.github.com/en/billing/concepts/product-billing/git-lfs) for the allowances; per-GiB rates from GitHub's [billing docs](https://docs.github.com/en/billing/using-the-new-billing-platform/about-billing-for-git-large-file-storage) as quoted in search results |
| **Cloudflare R2** Standard | $0.015/GB ($15/TB) | Free | Class A $4.50/million; Class B $0.36/million | 10 GB-month, 1 M Class A, 10 M Class B | Verified: [R2 pricing](https://developers.cloudflare.com/r2/pricing/) |
| **Backblaze B2** | $6.95/TB | Free up to 3× average monthly storage, then $0.01/GB; free through CDN and compute partners | Class A–C free; Class D $0.004 per 10,000 after 2,500/day | First 10 GB | Verified: [B2 pricing](https://www.backblaze.com/cloud-storage/pricing) |
| **AWS S3** Standard, us-east-1 | $0.023/GB first 50 TB | $0.09/GB for the first ~10 TB after 100 GB/month free (aggregated across AWS) | PUT/LIST $0.005 per 1,000; GET $0.0004 per 1,000 | Up to $200 in Free Tier credits for new customers | GET rate, 100 GB free egress and credits Verified on [S3 pricing](https://aws.amazon.com/s3/pricing/) (the storage tables did not render); storage and egress rates Unverified, from [Upstash, 2026](https://upstash.com/blog/aws-s3-pricing-explained-2026) |
| **Tigris** Standard | $0.02/GB | Free | Class A $0.005 per 1,000; Class B $0.0005 per 1,000; DELETE free | 5 GB, 10,000 Class A, 100,000 Class B | Verified: [Tigris pricing](https://www.tigrisdata.com/pricing/) |
| **Wasabi** Hot | $7.99/TB, minimum charge of 1 TB | "No charge for egress or API requests" | Free | Trial: 100 GB for 30 days (company email) | Verified: [Wasabi pricing](https://wasabi.com/pricing). The 90-day minimum storage duration also applies (Verified, same page) |
| **Hetzner Object Storage** | €6.49/month base including 1 TB and 1 TB egress, then €0.0087/TB-hour (≈ €6.35/TB-month) | €1.00/TB beyond the included TB; internal eu-central traffic free | Free | None | Verified: [Hetzner](https://www.hetzner.com/storage/object-storage/). EU only (Falkenstein, Nuremberg, Helsinki) |
| **DagsHub** | Individual $0 (20 GB, or 200 GB per the comparison table); Team $119/user/month monthly or $99 yearly, up to 1 TB | Not published | Not published | Individual: non-commercial private repos, 2 collaborators | Verified: [DagsHub pricing](https://dagshub.com/pricing/). The page contradicts itself on free storage |
| **Weights & Biases** (now under CoreWeave) | Free 5 GB; Pro from $60/month with 100 GB, then $0.03/GB ($30/TB) | Not published | Not published | 5 GB | Verified: `wandb.ai/site/pricing` redirects to [CoreWeave Forge pricing](https://coreweave.com/forge-pricing) |
| **lakeFS** | Team $499/month (5 users, 500 GB under management, self-managed); Enterprise custom | Your own store | Your own store | 30-day trial, card required | Verified: [lakeFS pricing](https://lakefs.io/pricing/). Open source core exists |
| **Oxen.ai** | Free 50 GB; Hacker ~$30/month (100 GB); Pro ~$60/month (500 GB) | Not found | Not found | 50 GB | Unverified: the official page returned HTTP 429 and a bot wall; figures from [aitoolsatlas](https://aitoolsatlas.ai/tools/oxen-ai/pricing) and [everydev](https://www.everydev.ai/tools/oxen-ai) |
| **Replicate** | No storage product. API prediction inputs, outputs and files "are automatically removed after an hour" | n/a | n/a | n/a | Verified: [Replicate data retention](https://replicate.com/docs/topics/predictions/data-retention) |

What the table says (Estimated, our reading):
- **The per-TB floor is about $7** (B2, Hetzner, Wasabi). Hugging Face
  private storage costs 2.6× that, but includes egress, a CDN and the
  tooling. GitHub LFS costs about 10× that and also charges for egress.
- **Egress is where users get hurt.** S3 and GitHub LFS charge for it;
  R2, Tigris and Wasabi don't; B2 and Hugging Face include it up to a
  multiple of storage.
- **Only Hugging Face Enterprise bills after dedup.** For everyone else, a
  team that saves 50 checkpoints of a 10 GB model pays for 500 GB,
  however little changed between them.
- **Agent output has no durable home in inference products.** Replicate
  deletes API outputs after an hour, so agents and pipelines need storage
  elsewhere.

### 1.2 What users love and hate

Sentiment is from public threads, cited per claim. It is anecdotal: a
handful of loud threads, not a survey. Hacker News threads were found
through the HN Algolia search API.

| Product | Loved | Hated |
|---|---|---|
| Hugging Face Hub | The default home for models; free public hosting; tooling that everything integrates with (Estimated, from its position, not a cited thread) | Quota surprises: a "Private repository storage limit reached" 403 with the UI showing about 66 of 100 GB used ([HF forum](https://discuss.huggingface.co/t/403-error-private-repository-storage-limit-reached-quota-shows-space-remaining/153121)); a 403 saying "your storage patterns tripped our internal systems" at 0 GB used, with slow replies ([HF forum](https://discuss.huggingface.co/t/403-forbidden-your-storage-patterns-tripped-our-internal-systems-please-contact-us-at-website-huggingface-co-so-we-can-verify-your-account-and-unlock-more-storage-for-your-use-case/167941), [HF forum](https://discuss.huggingface.co/t/403-forbidden-error-cannot-use-storage/167289)); a per-repo 1 GB LFS cap with no visible way to raise it ([HF discussion](https://huggingface.co/spaces/huggingface/README/discussions/6)) |
| GitHub LFS | Built into the forge people already use | Public repos stop cloning when the owner runs out of quota: "This repository is over its data quota" ([HN](https://news.ycombinator.com/item?id=38854234), [HN](https://news.ycombinator.com/item?id=21300233), [HN](https://news.ycombinator.com/item?id=41091523)); the owner pays for strangers' downloads, which makes LFS dearer than committing the file ([HN](https://news.ycombinator.com/item?id=11358792)); a move to metered billing with a $0 budget broke builds, and the smallest top-up disappeared ([GitHub community #151710](https://github.com/orgs/community/discussions/151710)) |
| Cloudflare R2 | Zero egress, the launch drew 216 comments ([HN](https://news.ycombinator.com/item?id=28682237)); sustained comparison interest ([HN](https://news.ycombinator.com/item?id=42256771)) | Operation pricing confuses people who expect "free egress" to mean free reads ([HN](https://news.ycombinator.com/item?id=33865229)); a global outage ([HN](https://news.ycombinator.com/item?id=42960291)) |
| Backblaze B2 | Half the price of comparable stores and good transfer rates ([HN](https://news.ycombinator.com/item?id=25915228)); simplest cheap option for backups ([HN](https://news.ycombinator.com/item?id=17406366)) | The egress quota beyond 3× storage ([HN](https://news.ycombinator.com/item?id=47675770)) |
| AWS S3 | The default; reliable; the API everyone speaks (Estimated) | Egress fees: "practically useless for all but enterprise customers" ([HN](https://news.ycombinator.com/item?id=29332716)); "if you have any traffic at all, the traffic will dominate" ([HN](https://news.ycombinator.com/item?id=24540193)) |
| Wasabi | No egress or request fees (Verified, pricing page) | The 90-day minimum storage duration charges overwritten or deleted data for 90 days ([HN](https://news.ycombinator.com/item?id=26543985), [HN](https://news.ycombinator.com/item?id=17406731), [HN](https://news.ycombinator.com/item?id=43805614)); egress limited "on the honor system" to stored volume ([HN](https://news.ycombinator.com/item?id=43803256)); outages ([HN](https://news.ycombinator.com/item?id=42279055)) |
| Tigris | Globally distributed, zero egress (Verified, pricing page) | Little sentiment found; one thread disputes its benchmark claims ([HN](https://news.ycombinator.com/item?id=39361506)) |
| DagsHub | A DVC remote with a UI on top ([HN](https://news.ycombinator.com/item?id=32832912)) | Seen as the bottleneck in DVC benchmarks, according to a DVC maintainer ([HN](https://news.ycombinator.com/item?id=34955398)); team price of $99–119 per user (Verified, pricing page) |
| Weights & Biases | "Looks great" ([HN](https://news.ycombinator.com/item?id=45676676)) | "Really expensive" for a small team, plus no self-hosted SaaS option ([HN](https://news.ycombinator.com/item?id=45676676)) |
| lakeFS | Git-style branches over your own object store ([HN](https://news.ycombinator.com/item?id=26530059)) | Removing ACLs from the open-source edition ([HN](https://news.ycombinator.com/item?id=41061786)), later softened by pluggable auth ([HN](https://news.ycombinator.com/item?id=41170351)) |
| Oxen.ai | Fast versioning of many-file datasets, in Rust ([HN](https://news.ycombinator.com/item?id=34831547)); its own benchmark against DVC and Git LFS ([HN](https://news.ycombinator.com/item?id=42037112), vendor-run) | Seen moving toward fine-tuning and inference ([HN](https://news.ycombinator.com/item?id=44919197)); "Git-LFS sucks, so build a new system" scepticism ([HN](https://news.ycombinator.com/item?id=48571859)) |
| Replicate | n/a (not a storage product) | n/a |

**Lessons for openxet.com** (Estimated, each traced to a row above):
1. **Never cut off reads because of a quota.** GitHub LFS's lockout breaks
   other people's clones. Over-quota blocks new writes, never reads.
2. **Quotas must be legible and match the bill.** Hugging Face users hit
   403s that the UI contradicts. Our usage page shows the exact number
   the quota checks, and every quota error names the number, the limit
   and the next step.
3. **Egress anxiety is the strongest feeling in this market.** Include
   generous downloads, publish the ratio, and let budgets cap the rest.
4. **No minimum durations or hidden minimum charges.** Wasabi's 90 days is
   the most-cited complaint against an otherwise popular product.
5. **Don't take features out of the open source.** lakeFS's ACL episode
   shows the cost. Design §15's no-private-fork rule already prevents it.
6. **Dedup is invisible unless the bill shows it.** Hugging Face Xet
   saves bytes for everyone but only Enterprise is billed for that.

## 2. The feature set for hosted 1.0

"Core today" is the state at `779370e`: **Exists**, **Partial** or
**Needed**. "Milestone" is where the design puts it (§12). **M8** is the
hosted milestone this note proposes, for the operational parts that
design §15 allows to be hosted-only (billing, sign-up, abuse, status
page). Every core row stays in the open-source build; a row marked M8
lives behind the core seams (authorizer webhook, metering, admin API).

### 2.1 Onboarding

| Feature | Why it matters | For | Core today | Milestone |
|---|---|---|---|---|
| Sign in with GitHub, Google or OIDC; no password, no profile | Design §15; openxet never owns users (§4) | Both | Needed: OIDC/JWT authenticator | M3 (authenticator); M8 (hosted sign-in page, per ADR 0008 item 7) |
| Identity-bound access token shown once with copy-ready `HF_ENDPOINT` and `HF_TOKEN` lines | The 60 s path (§1a): no `hf auth login`, no config file | Both | Partial: static tokens and `oxt_` secrets exist (M1-R28); identity binding and revocation don't | M3 |
| Stock `hf upload` works with two env vars | The headline promise; `hf upload` calls `create_repo(exist_ok=True)`, so the first upload creates the repo (Verified, `huggingface_hub` `main`, `src/huggingface_hub/cli/upload.py`, 2026-10-24) | Both | Needed: Hub API | M2 |
| Repos private by default on openxet.com | Safe default (§7.1); nothing becomes public by accident | Humans | Needed | M2 (repo create), M8 (hosted default) |
| Live "waiting for your first upload" panel that turns into the savings view | Confirms success without a terminal round trip | Humans | Needed: web UI plus metering | M4 (metering), M6 (web UI) |
| A copy-ready dedup demo (upload, change 1 MB, upload again) | First upload shows no savings; the second shows the product | Humans | Needed: docs and UI | M6 |
| Client-version-aware errors (old `huggingface_hub`, no `hf_xet`, `hf_` token sent to openxet) | Removes the commonest dead ends (§5.3) | Both | Partial: structured errors with `hint` exist in the CLI and CAS | M2 |
| Server-side chunking for clients without Xet | Old clients and plain `git-lfs` still get dedup in storage | Both | Needed | M4 |
| Browser drag-and-drop upload | Visitors on a phone or a locked-down laptop still finish | Humans | Needed: not in design §10.10's list | Propose M6 |

### 2.2 Organizations and teams

| Feature | Why it matters | For | Core today | Milestone |
|---|---|---|---|---|
| Namespaces from the IdP: personal namespace from the login; organizations from GitHub orgs, Google Workspace domains or OIDC group claims | No second member list to keep in sync; leaving the IdP group removes access | Both | Needed: config-rule and webhook authorizers | M3 |
| Roles read, write, admin per org and repo, mapped from claims | Design §5.4 authorizer; re-evaluated on every use | Both | Partial: CAS tokens carry scope and mode | M3 |
| Invite a teammate by GitHub handle or email; acceptance is their own sign-in | Referral loop; no openxet-held invitee profile (the invite is a pending authorization rule) | Humans | Needed | M6 (web UI), M8 |
| Per-namespace repo creation policy | Security requirement 2 (§5.9) | Both | Needed | M2 |

### 2.3 Tokens and agent surfaces

| Feature | Why it matters | For | Core today | Milestone |
|---|---|---|---|---|
| Delegated, attenuated agent tokens: chosen repos, read or write, expiry in hours, optional byte and operation caps, revocable | Design §7.1. An agent gets one repo, not the human's account | Both | Partial: CAS tokens already carry scope, mode and `exp`, capped at 24 h (M1-R19); Hub-level attenuated tokens are needed | M3 (token store), M6 (UI) |
| Token secret scanning: `oxt_` prefix registered with GitHub's secret-scanning partner programme | Leaked tokens get revoked within minutes | Both | Partial: the prefix exists for this purpose (M1-R28); registration is a non-code task | M8 |
| `--json` with versioned schemas, stable exit codes, `--dry-run`, no prompts | Design §7.1 | Agents | Exists for every current command ([CLI](../reference/cli.md), [exit codes](../reference/exit-codes.md)); `--dry-run` comes with the first state-changing admin commands | M1 onward |
| Structured errors with `code`, `message`, `hint` over HTTP and CLI | Agents recover without parsing prose | Agents | Exists for the CAS and CLI | M1 onward |
| OpenAPI generated from code and checked in CI | Agents and SDK generators | Agents | Needed; the design has no milestone for it | Propose M2, with the Hub routes |
| MCP server (`openxet mcp`) respecting the token's scope | Agents in Claude Code, Cursor and others use repos as tools | Agents | Needed | M6 |
| `llms.txt` and per-page Markdown | Agent-readable docs | Agents | Exists (M1-R36, generated at deploy) | Done |
| Handoff: every UI action shows the equivalent CLI and API call | Human hands a task to an agent and back (§7.1) | Both | Needed | M6 |

### 2.4 Data, safety and money

| Feature | Why it matters | For | Core today | Milestone |
|---|---|---|---|---|
| Audit log attributing each action as human, or agent on behalf of a human | Design §7.1; trust in delegated agents | Both | Needed; the design names it but no milestone | Propose M4 (with metering and the admin API), surfaced in M6 |
| Approval gates for agent-initiated destructive or costly actions (repo and branch delete, force-moving refs, GC, large egress), with a pending state the agent can poll | Design §7.1; the safe way to let agents act at night | Both | Needed | Propose M6 (needs the web UI) |
| Dedup savings dashboard: logical vs physical bytes, ratio and money, per repo, org and revision | Design §15 "every page shows what the user gains"; the visible differentiator | Humans; `--json` for agents | Partial: `openxet upload --json` reports bytes uploaded ([CLI](../reference/cli.md)); per-tenant metering is needed | M4 (metering), M6 (UI) |
| Usage and billing dashboard: stored bytes vs quota, egress vs allowance, forecast | Lesson 2 (§1.2): the number the quota checks is the number shown | Both | Needed | M4 (metering), M8 (billing) |
| Budgets and alerts: monthly cap per org and per token, alerts at 50/80/100%, hard stop on writes only | Lesson 3; agents can't run up a bill | Both | Needed: quotas are a core requirement (§15) | M4 (quota enforcement in core), M8 (alerts, billing link) |
| Quotas and per-tenant rate limits | §15 noisy-neighbour isolation; M1-R40 leaves per-credential limits to M3 | Both | Partial: process-wide memory and connection bounds exist (M1-R40) | M3 |
| `per-scope` dedup on hosted, so no tenant can probe another's data | §5.4, security requirement 4 | Both | Exists (tested: `per_scope_existence_checks_hide_other_tenants`) | Done |
| Webhooks on push and commit | Platforms and CI react to new checkpoints | Agents | Needed | M2 (post-receive, §10.9) |
| Server-side copy between repos the caller can read | "Fork" a model into your org instantly, like HF's `hf buckets cp` | Both | Needed | Propose M5, with buckets |

### 2.5 Platform

| Feature | Why it matters | For | Core today | Milestone |
|---|---|---|---|---|
| Regions: EU from day one (§15), then US; residency per org | Regulated and sovereign users (§1) | Humans (buyers) | Needed: pools and residency | M5 |
| Bring your own bucket (S3, GCS, Azure) | Headline option in §15; storage stays on the customer's bill | Humans (buyers) | Partial: the local-disk store exists; S3, GCS and Azure come in M3 | M3, M5 (pools) |
| Export and no lock-in: `hf download`, `git clone --mirror`, and for BYO-bucket orgs, `openxet reindex` on their own bucket | The bucket is the source of truth (§5.3), so leaving means pointing a self-hosted openxet at the same bucket | Both | Partial: formats are stock Xet; `reindex` is M4 | M4 |
| Public status page and published SLO | §15 operations bar | Both | Needed | M8 |
| SLA tiers: none on Free and Pro, 99.9% on Team, 99.95% on Enterprise | Buyers need a contractual number (Estimated levels) | Humans (buyers) | n/a (contract) | M8 |
| Support from every page, first reply within one business day on every tier | §15 fixes this for everyone | Humans | n/a | M8 |
| SSO for enterprise: SAML or OIDC to the customer's IdP; enforced SSO per org | Enterprise buying requirement | Humans | Partial: OIDC in M3; SAML is new | M3 (OIDC); propose M8 for SAML via a broker |
| SCIM deprovisioning | Revokes tokens and SSH keys the moment the IdP removes someone. openxet already re-evaluates authorization on use (§5.4); SCIM adds immediate token revocation, without storing profiles | Humans (buyers) | Needed; not in the design | Propose M8, behind the authorizer seam |
| Terms, DPA, privacy notice, DMCA/DSA takedown, abuse handling | §15 prerequisite for public launch | Humans | Needed | M8 |
| Zero-downtime rolling upgrades | §15; issue #47 migration policy | Both | Needed | M3 |
| Tested backups and restores; DR drills | §15, §16 | n/a | Needed: `reindex` and `verify` | M4, M8 (schedule) |

## 3. Pricing proposal

*Proposal for the user to approve.*

### 3.1 Value metric: physical bytes after dedup

**Bill the bytes openxet actually stores for the org, after
chunk-level dedup and compression. Show logical bytes beside them on
every page and invoice.**

Why:
1. **It is the product.** The design's first success criterion is that
   a slightly changed 10 GB checkpoint moves megabytes (§1). Billing
   logical bytes would charge users for exactly what openxet saves them.
2. **It aligns incentives with agents.** Agents generate many near
   identical revisions. Physical billing makes that cheap and makes
   keeping history the default, instead of squashing it to save money.
   That is the advice HF's own limits page gives (Verified,
   [storage limits](https://huggingface.co/docs/hub/storage-limits)).
3. **It is a real differentiator today.** Only HF Enterprise bills after
   dedup (Verified). Offering it from the free tier up is the clearest
   single reason to choose openxet.com.
4. **The core already measures it.** Logical and physical metering per
   scope and tenant is a core feature (§5.7). The design's target is that
   metered physical bytes sum to within 1% of bucket bytes, and are exact
   per tenant under `per-scope` dedup (§1a), which hosted uses.
5. **Our cost follows it.** Storage cost is physical bytes.

Rules that make it fair and legible (proposed):
- **Billed physical bytes are per org** (the tenant, §5.4), counting each
  xorb the org has registered once, however many of its repos use it. A
  xorb another tenant also stores is still billed to each tenant that
  registered it. Under `per-scope` dedup that is the only answer that
  leaks nothing across tenants. openxet.com's own storage saving from
  cross-tenant overlap is kept as margin, and this page says so.
- **Billing stops when data is unreferenced, not when GC deletes it.**
  GC keeps bytes for at least 28 days (M1-R5). Users shouldn't pay for
  that grace. We absorb about a month of storage on deleted data
  (Estimated: small, since deletion is rare for versioned data).
- **Repos and buckets are billed the same way.** S12 decides whether
  buckets gain an S3 API. If they do, small objects stay billed by their
  stored bytes, with no minimum object size. Revisit only if metadata
  cost per object proves material.
- **The usage page and the quota check use one number.** Lesson 2 (§1.2).

Risks:
- **Users may find a physical-byte bill harder to predict.** The answer
  is a forecast on the usage page and budgets (§2.4).
- **Dedup ratios vary.** Random or encrypted data dedups at 0%. Pricing
  must work at a ratio of 1. It does: $15/TB physical is still below HF's
  $18/TB logical for private data.
- **A competitor can copy it.** HF could extend dedup billing to PRO and
  Team. Then the differentiators left are owning your bucket, region and
  identity, open source, and agent safety (§4), which is why the
  positioning doesn't rest on price alone.

### 3.2 Seats: humans only

Seats are people in the org's IdP. **Agents, CI jobs and service tokens
are never seats.** Their cost is the storage and downloads they cause,
which the budget caps (Estimated: this reflects how agent use scales in
these teams, not data we have). Per-seat pricing with free agents is
simple for buyers and avoids penalizing the behaviour we want.

### 3.3 Tiers

All prices USD, excluding tax. EUR prices at parity (€9, €20, €50) are a
user decision (§6).

| | **Free** | **Pro** | **Team** | **Enterprise** |
|---|---|---|---|---|
| Price | $0, no card, no time limit (§15) | $9 per user per month | $20 per user per month | From $50 per user per month, annual; minimum 20 seats |
| Stored (physical) included | 100 GB | 1 TB | 1 TB per seat, pooled across the org | 1 TB per seat, pooled, plus volume pricing |
| Extra storage | Not available: writes stop at the limit, reads never do | $15 per TB-month | $15 per TB-month | $12 per TB-month from 50 TB, $10 from 500 TB |
| Downloads included | 5× stored, per month | 5× stored | 5× stored | Negotiated (default 8×) |
| Extra downloads | Not available: downloads continue at reduced priority | $5 per TB | $5 per TB | Included in contract |
| Requests | No fees. Rate limit 50 requests/s per org | No fees. 200/s | No fees. 1,000/s | No fees. Custom |
| Members | You plus 2 collaborators | 1 user (personal namespace) | Unlimited seats | Unlimited seats |
| Agent tokens, MCP, `--json`, OpenAPI | Included | Included | Included | Included |
| Audit log retention | 7 days | 90 days | 1 year, exportable | Custom, streamable to the customer's SIEM |
| Approval gates, budgets, alerts | Budget alerts | Gates and budgets | Gates, budgets, org-wide policies | Plus custom approval policies |
| Identity | GitHub, Google | GitHub, Google, OIDC | Plus IdP group mapping, enforced SSO | Plus SAML, SCIM |
| Regions | EU or US | EU or US | EU or US, per org | Per org or per repo residency; dedicated pool; KMS keys |
| Bring your own bucket | No | No | Yes: seats plus $5 per TB-month managed | Yes |
| SLA | None (status page) | None (status page) | 99.9% | 99.95%, with credits |
| Support | First reply within one business day (§15) | Same | Same, plus a shared channel | Named contact; P1 response in 1 hour |

Notes on the choices (Estimated reasoning):
- **Pro and Team match HF's prices** ($9 and $20, Verified). Stored bytes
  are physical, not logical, so the same headline buys several times the
  history for checkpoint-heavy work. Matching HF's price avoids a "cheaper
  but riskier" frame, and the extra value shows on the dashboard.
- **$15/TB undercuts HF's $18/TB private rate** at a dedup ratio of 1, and
  at a ratio of 3 it is $5 per logical TB.
- **The free tier is 100 GB stored, not uploaded.** It matches HF's free
  private allowance (Verified), and with dedup it holds more. It costs us
  at most about $1 per fully used account per month (§3.5).
- **Public repos are not free-unlimited at launch.** openxet.com is not a
  discovery hub, and free public storage invites abuse (the HF forum
  threads in §1.2 show HF's abuse controls hurting honest users). Public
  repos count against the same allowance. Revisit with data.
- **Over quota, writes stop and reads never do.** That avoids GitHub's
  lockout of other people's clones (§1.2).

### 3.4 Downloads, requests and agent-heavy use

- **Downloads included up to 5× stored per month** on every tier. That is
  between B2's 3× and HF buckets' 8× (both Verified). Hetzner's internal
  traffic is free and its dedicated nodes have unlimited traffic at 1
  Gbit/s (Verified), so EU downloads cost us almost nothing per TB. The US
  pool on B2 costs $10/TB beyond 3× unless it is served through a CDN
  partner (Verified: B2 lists free egress through partners), which is the
  main risk to this number.
- **No request fees, ever.** Xet traffic is coarse: a 10 GB upload is about
  150 xorb POSTs of up to 64 MiB (the protocol limit), so requests don't
  drive cost. Rate limits per org and per token protect the service
  instead, and every `429` carries `Retry-After` and a hint.
- **Agent tokens can carry their own caps:** bytes written, bytes read,
  operations and expiry (§7.1). An org budget applies above all tokens.
- **Budgets are hard caps on writes and on paid extras**, never on reads of
  what's stored. An agent that hits one gets a structured error with the
  budget's name and the human to ask, and can open an approval request
  (§2.4) instead of failing.
- **Abuse:** free-tier downloads beyond 5× drop to reduced priority rather
  than stopping, so a public repo that goes viral keeps working while the
  owner decides whether to upgrade.

### 3.5 Unit economics

All Estimated, from the Verified list prices in §1.1 and these
assumptions:
- Storage cost $7.0 per TB-month: Hetzner Object Storage at €6.35 per
  TB-month and €1.00 per TB egress in the EU; Backblaze B2 at $6.95 with
  3× free egress in the US; €1 ≈ $1.08.
- Downloads cost $0.5 per TB on average (EU nearly free, US within 3× free,
  some US overage).
- Platform fixed cost about $750 per month at launch: three Hetzner AX42
  nodes (€79 each, Verified) for openxet, two AX102 (€159 each, Verified)
  for a Postgres primary and replica, plus monitoring, backups, status
  page and email.
- Payments: Stripe EEA card fees 1.5% + €0.25 (Verified) and Billing at
  0.7% (Verified); US cards assumed 2.9% + $0.30 (Unverified, US page not
  read). Tax handling (Stripe Tax at 0.5%, Verified) is included.
- "Expected" use is 30% of the included storage, with downloads at 2×
  stored. "Full" use is 100% of the included storage and the full 5×
  downloads.
- Support and engineering time are not in the cost. They are the
  founders' time until revenue pays for them.

| Tier | Revenue per month | Cost at expected use | Gross margin, expected | Cost at full use | Gross margin, full |
|---|---|---|---|---|---|
| Free | $0 | 30 GB × $7/TB ≈ $0.21, plus $0.03 downloads ≈ **$0.25** | n/a (acquisition cost) | 100 GB × $7/TB + 0.5 TB × $0.5/TB ≈ **$0.95** | n/a |
| Pro, 1 seat | $9 | $2.10 storage + $0.30 downloads + $0.70 fees ≈ $3.10 | **≈ 66%** | $7.00 + $2.50 + $0.70 ≈ $10.20 | **≈ −13%** |
| Team, 10 seats | $200 | 3 TB: $21 + $3 downloads + $8 fees ≈ $32 | **≈ 84%** | 10 TB: $70 + $25 + $8 ≈ $103 | **≈ 49%** |
| Extra storage, per TB | $15, plus up to 5 TB of downloads | $7 + $1 (2× downloads) + $0.50 fees ≈ $8.50 | **≈ 43%** | $7 + $2.50 + $0.50 = $10 | **≈ 33%** |
| Extra downloads, per TB | $5 | $0.5–1 (EU) | ≈ 80–90% | $10 (US beyond B2's 3×, no CDN) | **−100%** |
| BYO bucket, per TB managed | $5 | Index and compute ≈ $0.50 | **≈ 90%** | ≈ $1 | ≈ 80% |
| Enterprise, 20 seats | $1,000 + storage | Dedicated pool and support time dominate; not modeled | 60–75% target | | |

Readings:
- **A fully used Pro account loses money** if it also downloads 5×
  stored. Seat plans rely on average use, as HF's do. If measured use runs
  above 60% of the allowance, lower downloads to 3× or raise Pro to $12.
- **Extra storage at $15/TB has a thin margin** on $7 storage. It is
  priced to win against HF's $18 logical. Volume discounts are only
  offered where a dedicated pool or longer commitment lowers our cost.
- **Cloudflare R2 doesn't work as the primary pool:** at $15/TB the
  extra-storage price would have no margin. R2 suits a download cache or
  CDN in front of the primary pool, where its free egress pays.
- **Break-even** on the $750 fixed cost: about 130 Pro seats at expected
  use (about $5.90 contribution each), plus about $0.25 per active free
  account. With 5,000 free accounts that adds $1,250, so roughly 340
  Pro-equivalent seats in total.
- **Running our own Ceph or MinIO on Hetzner SX storage servers** would
  cut storage to roughly €1.2 per raw TB (SX135 at €204 for 8 × 22 TB,
  from a third-party listing, Unverified), about €2–3.5 per usable TB
  after redundancy. It is a later cost lever with real operating burden,
  not a launch choice.
- **Both storage providers need checking** against the range-binding
  probe (ADR 0002) before direct downloads are allowed; otherwise
  downloads go through the transfer role, which Hetzner's unlimited node
  traffic makes affordable in the EU.

### 3.6 Alternative models

| Model | How it works | For it | Against it |
|---|---|---|---|
| **A. Seats plus physical bytes** (recommended) | §3.3: per-human seats with an included physical allowance, pay-as-you-go storage above it, agents free | Predictable for buyers; matches HF's shape so comparison is easy; dedup value on every tier; funds support | A fully used Pro account is unprofitable; two meters (seats and bytes) to explain |
| **B. Pure usage** | No seats. $15 per TB-month physical, downloads 5× included, everything else free; Team features as a flat $99 per org per month | Ideal for agents and CI-heavy teams; one meter; looks like R2 or B2, which developers like | Revenue tracks storage only, and storage margins are thin; small teams pay almost nothing while still using support; no per-seat expansion revenue |
| **C. Logical bytes at a lower rate** | Bill uploaded bytes at, say, $8 per logical TB | Easiest to predict; matches every object store | Hides the product's value; charges users for history; margin falls as dedup improves; incentive to squash history, against design §1 |
| **D. Flat capacity blocks** (Wasabi-style) | $X per TB block per month, downloads fair-use | Simple invoices | Pays for unused capacity; "fair use" is the opposite of a legible quota (§1.2 lesson 2) |

**Recommendation: A**, with B's spirit inside it: agents are never seats,
and there are no request fees.

## 4. Positioning for the agentic age

### 4.1 Concrete benefits

Each benefit is a capability with a test or a measurable target, not a
slogan.

| Benefit | What an agent or team actually gets | Built on | Status |
|---|---|---|---|
| Agents can push checkpoints safely | A token for one repo, write-only, expiring in 8 hours, capped at 200 GB written. A leak can't touch anything else. | §7.1 attenuated tokens; CAS tokens already scope repo, mode and expiry (ADR 0007) | Partial (M3) |
| Destructive actions wait for a human | `delete repo` from an agent returns `pending_approval` with a poll URL; the human approves in the UI or CLI | §7.1 approval gates | Needed (M6) |
| Many iterations cost only the changes | 50 checkpoints of a 10 GB model where each step changes ~1% are billed at about 15 GB, not 500 GB (Estimated example; real ratios depend on the training regime, since a full fine-tune can change every weight) | Xet dedup plus physical billing (§3.1) | Dedup measured: 1,141,116 B sent for a 1 MiB edit of a 100 MiB file, fresh cache ([competitive matrix](../competitive-matrix.md)); billing needs M4 metering |
| Agents never parse prose | Every command has `--json` with a versioned schema; every error has `code` and `hint`; exit codes are stable | §7.1 | Exists for current commands |
| Agents find their tools | `openxet mcp` exposes repos, files, upload, download, usage and admin as typed tools, limited to the token's scope; `llms.txt` and OpenAPI describe the rest | §7.1 | `llms.txt` exists; MCP M6; OpenAPI proposed M2 |
| Every action is attributed | "agent `ci-nightly` on behalf of alice@acme deleted branch `exp-41`" in the audit log | §7.1 | Needed (proposed M4) |
| Agent output outlives the run | Replicate deletes API outputs after an hour (Verified); a bucket or repo on openxet.com keeps them, deduplicated | Repos and buckets (M5 for buckets) | Needed |
| No lock-in for the team that owns the agent | Stock `hf`, `git` and `git-xet` read everything; BYO-bucket orgs can take their bucket and run `openxet reindex` themselves | §5.3, §6 | Partial (formats exist; `reindex` M4) |

### 4.2 Positioning statements

**Recommended:**

> **Hugging Face–compatible storage where every new checkpoint costs only
> its changed bytes, and every agent gets a key to exactly one repo.**

It names the familiar workflow, the pricing difference and the agent
safety difference in one sentence, and each claim maps to a feature
above.

Alternatives:
1. *"Point `HF_ENDPOINT` at us and keep your tools. Pay for the bytes we
   store, not the bytes you upload."* Developer-first and concrete;
   doesn't mention agents.
2. *"Let your agents push all night. Pay for the changes, approve the
   deletes."* The sharpest agentic line; narrower audience.
3. *"Open-source Xet storage, hosted. Your region, your identity
   provider, or your own bucket."* Sovereignty-first, for regulated
   buyers; doesn't mention price.

Caveats to settle before publishing any of them:
- "Hugging Face–compatible" is nominative use. The site already says
  openxet is independent and not endorsed. Keep that next to the line.
- The name "Xet" carries trademark risk on a paid service (design §14).
  A tagline that uses "Xet" adds to it.
- Each claim must be true on the day it's published. Today, only the
  dedup claim is measured.

## 5. From first visit to fan

### 5.1 The journey, step by step

**Step 1: the landing page.** The CTA leads, and the copy states what
happens next and how long it takes.

```
┌──────────────────────────────────────────────────────────────────────┐
│ openxet                                     Docs  Pricing  GitHub ★   │
├──────────────────────────────────────────────────────────────────────┤
│  Store big files. Pay for what changed.                              │
│  Works with hf, huggingface_hub, git-xet. Agents get one-repo keys.  │
│                                                                      │
│  [ Sign in with GitHub ]  [ Google ]  [ SSO ]                        │
│  Free: 100 GB stored, no card. About 60 seconds to your first upload.│
│                                                                      │
│  revision 1  ▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇   10.0 GB uploaded                     │
│  revision 2  ░░░░░░░▇░░░░░░░░    1.1 MB uploaded  (illustration)     │
└──────────────────────────────────────────────────────────────────────┘
```

**Step 2: sign in with GitHub.** One consent screen, asking only for
identity (no repo scopes). openxet stores the token hash, the bound
subject and a claims snapshot, nothing else (§5.4).

```
┌─────────────────────── github.com ───────────────────────┐
│  openxet.com wants to: read your public profile and      │
│  org membership.  It cannot read or write your repos.    │
│                         [ Authorize openxet ]            │
└──────────────────────────────────────────────────────────┘
```

**Step 3: the token and the commands.** One screen, already done for
the user: namespace from the GitHub login, token shown once, commands
with the token filled in.

```
┌──────────────────────────────────────────────────────────────────────┐
│ Welcome, alice. Your namespace is  alice  (orgs: acme-ml)             │
│                                                                      │
│ 1  Point hf at openxet (this terminal):                    [ Copy ]  │
│    export HF_ENDPOINT=https://openxet.com                            │
│    export HF_TOKEN=oxt_••••••••••••••••  (shown once)  [ Reveal ]     │
│                                                                      │
│ 2  Upload anything:                                        [ Copy ]  │
│    pip install -U huggingface_hub                                    │
│    hf upload alice/first-repo ./model.safetensors                    │
│                                                                      │
│  ◌ Waiting for your first upload…   Stuck? [ Troubleshoot ] [ Help ] │
│  Keep these settings: [ bash/zsh ] [ fish ] [ PowerShell ] [ .env ]   │
└──────────────────────────────────────────────────────────────────────┘
```

**Step 4: `hf upload`.** Nothing about the client changes.

```
$ hf upload alice/first-repo ./model.safetensors
Processing Files (1 / 1): 100%|██████████| 2.13GB / 2.13GB
New Data Upload        : 100%|██████████| 2.13GB / 2.13GB
https://openxet.com/alice/first-repo/blob/main/model.safetensors
```

(The progress-bar labels are illustrative; the client prints its own.)

**Step 5: savings shown.** The waiting panel becomes the repo view. The
first upload stores everything, so the page teaches the second step
instead of showing a zero.

```
┌──────────────────────────────────────────────────────────────────────┐
│ ✓ alice/first-repo · 1 commit · 2.13 GB          58 s since sign-in  │
│                                                                      │
│  Uploaded (logical)   2.13 GB      Stored (billed)   2.07 GB         │
│  Saved so far         3% (compression)                               │
│                                                                      │
│  See dedup work: change part of the file and upload it again.        │
│    python -c "f=open('model.safetensors','r+b');f.seek(2**30);        │
│               f.write(b'\0'*2**20)"                       [ Copy ]   │
│    hf upload alice/first-repo ./model.safetensors                    │
└──────────────────────────────────────────────────────────────────────┘
```

After the second upload:

```
┌──────────────────────────────────────────────────────────────────────┐
│ alice/first-repo · 2 commits                                         │
│  Uploaded (logical)   4.26 GB      Stored (billed)   2.07 GB         │
│  Saved                51%  ≈ $0.03/month at $15/TB                   │
│  Last upload sent     1.1 MB of 2.13 GB                              │
│  ▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇░  2.07 GB of 100 GB free storage used           │
└──────────────────────────────────────────────────────────────────────┘
```

(The figures are an illustration of the layout, not a measurement.)

**Step 6: invite a teammate, or give an agent a key.**

```
┌──────────────────────────────────────────────────────────────────────┐
│ Share alice/first-repo                                                │
│  Person:  [ github handle or email ]  role [ write ▾ ]  [ Invite ]   │
│           They sign in with their own GitHub/Google. No new password.│
│                                                                      │
│  Agent:   name [ nightly-trainer ]  repo [ alice/first-repo ]         │
│           access [ write ▾ ]  expires [ 8 h ▾ ]  cap [ 200 GB ]       │
│           destructive actions [ need my approval ▾ ]   [ Create ]    │
│           → HF_TOKEN=oxt_…  ·  MCP config  ·  equivalent CLI command  │
└──────────────────────────────────────────────────────────────────────┘
```

**Timing budget for the 60 s target** (Estimated; to be measured on the
release check of §1a): landing to sign-in 3 s; GitHub consent 5–10 s (one
click when already signed in to GitHub); token screen and copying two
blocks 10 s; `pip install -U huggingface_hub` 5–20 s (skipped when
already installed); `hf upload` of a small file 5 s. Total 28–48 s, with
the install as the largest variable. A ≥ 1 GiB first upload, which §1a
measures, adds the transfer time at the user's uplink: about 9 s at
1 Gbit/s, over a minute at 100 Mbit/s. The release check should state
the uplink it ran on.

### 5.2 Moments of delight

1. **The second upload.** "1.1 MB of 2.13 GB sent" is the moment the
   product makes sense. Everything in step 5 exists to get there.
2. **Money, not just bytes.** "Saved 51%, about $X/month" on every repo,
   org and invoice (§15).
3. **No new account.** Sign in with what you have; invite by GitHub
   handle; leaving the GitHub org removes access by itself.
4. **The agent key that can't hurt you.** One repo, eight hours, a byte
   cap, and deletes waiting for your approval.
5. **The bill that matches the page.** The number on the usage bar is the
   number on the invoice and the number the quota checks.
6. **The exit is in the docs.** "How to leave openxet.com" is a real page
   with working commands, before anyone asks.
7. **Errors that finish the job.** Each one names what happened and the
   next command, with a link to the exact docs section.

### 5.3 Dead ends and their fixes

| Dead end | Fix |
|---|---|
| The visitor has no GitHub account, or the employer blocks GitHub OAuth | Google and OIDC SSO on the same screen. No email-and-password fallback: openxet never owns users |
| The visitor is on a phone | "Continue on your computer" sends nothing; it shows a QR code and short link to the token page, and the browser upload (proposed M6) lets them try now |
| Python or `pip` is missing | The token screen detects the platform and offers the HF CLI's own install route next to `pip`; the troubleshoot page lists each |
| An old `huggingface_hub` without `hf_xet`, or `hf_xet` below the 1.7.0 floor (M1-R9) | Server-side chunking (M4) still accepts the upload and dedups it in storage; the response hint says `pip install -U huggingface_hub` for faster uploads |
| `HF_TOKEN` still holds a huggingface.co token (`hf_` prefix) | `401` with code `foreign_token` and a hint naming the token page. Only the prefix is checked; the token is never echoed or logged (§5.9 req 6) |
| A new terminal loses the env vars | "Keep these settings" writes the two lines for the user's shell, or a `.env` |
| `HF_ENDPOINT` set but the user runs `hf auth login` | Works: `hf auth login` against openxet answers `whoami-v2` from the bound identity (M2) |
| The upload is interrupted | `hf_xet` retries; dedup means the retry resends only what's missing |
| A repo name already exists | `create_repo(exist_ok=True)` succeeds for the owner; for someone else's namespace the error names the user's own namespace |
| Over the free quota | Writes get a structured `quota_exceeded` naming used, limit and the upgrade or cleanup link; reads keep working |
| Budget reached by an agent | `budget_exceeded` naming the budget and its owner; the agent can open an approval request |
| A token leaks to GitHub | GitHub's secret scanning notifies openxet.com, which revokes the token and shows why in the owner's console and audit log (M8 partner registration) |
| The invitee signs in with a different identity than the one invited | The pending invite lists who it is for; the inviter can retarget it in one click |
| The waiting panel never turns green | After 90 s it shows the three commonest causes (endpoint unset, wrong token, firewall) and an `openxet doctor` one-liner (M6) |
| Outage | Every error and the console link to the status page |
| The visitor wants to leave | Export page: `hf download`, `git clone --mirror`, and the BYO-bucket `reindex` path |

### 5.4 What to measure

Design §7 forbids client telemetry, and the website has no tracking.
These metrics come from openxet.com's own server-side records of
authenticated requests, aggregated per day, with no third-party analytics
and no client-side tracking. The privacy notice must say so before launch
(§6).

| Metric | Definition | Initial target (Estimated) |
|---|---|---|
| Time to first upload (TTFU) | From the first request to the sign-in endpoint to the first registered shard by that identity | p50 < 45 s, p90 < 60 s (§1a: < 60 s from the landing page) |
| Sign-in completion | Identities created ÷ sign-in starts | > 80% |
| Activation | New identities with ≥ 1 upload within 24 h | > 60% |
| Aha activation | New namespaces with ≥ 2 revisions of a file and savings > 0 within 7 days | > 30% |
| Retention | Namespaces with any authenticated write in week 1, still writing in week 4 and week 12 | W4 > 40% |
| Stored-bytes retention | Physical bytes still stored after 90 days ÷ bytes stored in week 1 | > 80% |
| Referrals | Invites sent per activated namespace; invites accepted ÷ sent; agent tokens created per activated namespace | ≥ 0.5 invites per activated org; > 50% accepted |
| Paid conversion | Free namespaces upgrading within 90 days | 3–5% (typical of developer tools; Estimated) |
| Dead-end rate | Structured errors per 1,000 requests, by `code`; share followed by a success within 10 minutes | Falling every release; top 5 codes each have a docs fix |
| Support | First-reply time | 100% within one business day (§15) |
| Agent health | Share of operations from agent tokens; approval-request latency; agent-leg pass rate | Tracked; agent leg 100% (§1a) |

## 6. Open decisions for the user

Each has a recommendation. None is decided by this note.

1. **Value metric.** Physical bytes after dedup on every tier, with
   logical shown beside them. *Recommend: yes* (§3.1).
2. **Prices.** Free $0 (100 GB), Pro $9, Team $20, Enterprise from $50
   per user per month; $15 per extra TB-month; downloads 5× included;
   no request fees. *Recommend: approve, and review after the first 90
   days of design-partner data* (§3.3).
3. **Agents and seats.** Agents, CI and service tokens are never seats.
   *Recommend: yes, and say so on the pricing page* (§3.2).
4. **Free public storage.** Count public repos against the same
   allowance at launch, rather than HF-style free public hosting.
   *Recommend: count them; revisit with abuse data* (§3.3).
5. **Launch storage.** Hetzner Object Storage (EU) and Backblaze B2 (US)
   as primary pools, R2 only as a cache or CDN. *Recommend: yes, subject
   to the ADR 0002 range-binding probe and a durability review of both
   providers* (§3.5).
6. **Billing data and tax.** Keep billing contacts and payment data in
   the payment processor, never in openxet's core (§4 no user database).
   Choose Stripe with Stripe Tax, or a merchant of record such as Paddle
   to carry EU VAT, and decide which legal entity sells the service.
   *Recommend: a merchant of record at launch, to keep tax work off a
   small team.*
7. **When deleted data stops being billed.** At unreference, not at GC.
   *Recommend: yes; we absorb the 28–30 day grace* (§3.1).
8. **Measuring the funnel without trackers.** Server-side, aggregated,
   authenticated events only, disclosed in the privacy notice; the
   landing page stays tracker-free. *Recommend: yes* (§5.4).
9. **Milestone M8.** Add the hosted milestone to design §12, and write
   ADRs for the metering-to-billing seam and for approval gates before
   building them. *Recommend: yes, in the next design PR.*
10. **Name.** "Xet" in a paid service's name and tagline adds trademark
    risk (design §14). *Recommend: ask Hugging Face before billing goes
    live, and keep the rename cheap as §14 says.*
11. **When billing switches on.** Design §15: two or three design
    partners, aqora first, run before billing. *Recommend: switch on
    billing once partners' measured TTFU meets §1a and no partner
    reports a blocking issue for 30 days.*
12. **Currency.** USD only, or USD and EUR at parity. *Recommend: both
    at parity, since the EU region is a headline feature.*
