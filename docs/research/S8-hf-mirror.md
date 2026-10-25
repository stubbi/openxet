# S8: Pull-through mirror of huggingface.co

<!-- description: What a pull-through mirror of huggingface.co must serve, and how it ingests upstream data. -->

- Date: 2026-10-23. Probes ran 07:05 to 07:10 UTC; legal texts were fetched
  at 07:07 UTC.
- Clients: `huggingface_hub` 2.2.0 and `hf_xet` 1.7.0, the versions pinned
  in `compat/`. Source citations are to the installed `huggingface_hub`
  package and to the `xet-core` 1.7.0 crates (`xet-client`,
  `xet-runtime`, `xet-core-structures`).
- Prior art: `guilt/xet-server` at `22c4aa1df566041b804aee2d4b1df1b97e2f04df`
  (2026-09-15, "Release 1.1.0"; MIT). The code was read but not run.
- Evidence: `compat/fixtures/s8/hf-mirror-probes.txt` (redacted probe
  output), the S3 fixtures in `compat/fixtures/hub/`, and
  `docs/research/S1-S2-direct-downloads.md`.
- Question (design §11): which HF endpoints and what Xet read-token
  behaviour a mirror depends on, and what the terms of use say about
  caching HF content. The task brief splits it into four:
  1. Which endpoints and headers must a mirror proxy or replicate for
     `hf_hub_download`, `snapshot_download` and Xet reconstruction to work
     transparently?
  2. Can Xet reconstruction through a read token for a public repo be
     proxied and cached (TTL, URL expiry)?
  3. How does `xet-proxyd` do it, and what should we reuse conceptually?
  4. What do HF's terms and the content licences permit for caching and
     serving HF content to third parties, including on openxet.com?

Labels:
- **observed:** seen in our own requests to huggingface.co, its CAS or its
  CDN, or in captured fixtures.
- **source:** read in client, server or `xet-core` source, or in a quoted
  legal text, at the cited place.
- **inferred:** our conclusion, not directly verified.

## Answers

1. **Endpoints.** A mirror must serve the same read surface as the Hub
   layer: `revision` info, `tree` (paginated), `HEAD`/`GET resolve`,
   `xet-read-token`, and CAS `v2`/`v1` reconstruction plus range-served
   xorb bytes. It must emit **its own** URLs and tokens in every header
   that points somewhere (`Location`, `Link: rel="xet-auth"`,
   `X-Xet-Cas-Url`, reconstruction URLs), and pass through HF's error
   codes (`GatedRepo`, `RepoNotFound`, `EntryNotFound`, `RevisionNotFound`)
   (source, observed). Details are in [What a mirror must serve](#what-a-mirror-must-serve).
2. **Reconstruction can be fetched upstream, but HF's answer cannot be
   cached and handed out** (observed):
   - The read token lives **900 s**. Anonymous tokens work for public
     repos.
   - Tokens are file-scoped per repo: a token for repo A got `404` for a
     file of repo B (one probe).
   - Reconstruction responses carry no `Cache-Control`.
   - Xorb URLs live **3600 s**. Their CloudFront policy binds the expiry,
     a byte range and an `xip` parameter. The range is enforced as
     **containment**, not equality.
   - So the mirror fetches xorb bytes promptly with its own per-repo
     token, verifies them, and stores the **chunks** natively. It serves
     its own reconstructions and its own range-bound URLs (ADR 0002).

   HF's xorbs cannot be copied as whole objects: HF only signs the ranges
   a file needs, and files often use part of a shared xorb (observed).
   The mirror therefore re-packs the verified chunks into local xorbs.
   The Xet file hash depends only on the chunk hashes and sizes, so it is
   unchanged and equals HF's `X-Xet-Hash` (source, observed).
3. **`xet-proxyd`** embeds a normal CAS and Hub server and fills them on a
   miss, which is the right shape. Its known limits match what we
   observed:
   - It only caches whole xorbs, so files that use part of a shared xorb
     are relayed and not cached.
   - Small non-Xet files are never cached.
4. **Legal.** No HF text we found addresses mirrors directly. The texts
   quoted below support different readings for (a) self-hosted use, (b) a
   hosted mirror on openxet.com and (c) gated or private content. (b) and
   (c) carry real exposure and need a lawyer (source). See
   [Legal risk for the user](#legal-risk-for-the-user).

## Observed behaviour of huggingface.co

Probed with a stdlib Python client against the public dataset
`xet-team/xet-spec-reference-files` (anonymous), `openai-community/gpt2`
(anonymous), a private model repo (the owner's fine-grained token), and
`meta-llama/Llama-3.2-1B` and `google/gemma-2-2b` (gated). The output is in
`compat/fixtures/s8/hf-mirror-probes.txt`.

| What | Observed |
|---|---|
| `HEAD resolve` of a Xet file | `302`, `Cache-Control: no-store`, `X-Xet-Hash`, `X-Repo-Commit`, `X-Linked-Size`, `X-Linked-ETag` (sha256), `Link: <…/xet-read-token/{commit}>; rel="xet-auth", <cas…/v1/reconstructions/{hash}>; rel="xet-reconstruction-info"` |
| `resolve` redirect target | `https://us.aws.cdn.hf.co/xet-bridge-us/{repo_id}/{xet hash}?…`, a CloudFront URL that expires 3600 s after issue. Its policy has **no** `ByteRange`: any range, or the whole file, is served. |
| `xet-read-token` | `200` anonymously for a public repo, and with the owner's token for a private one. `exp` is 900 s after issue. The JWT claims are `access: READ`, `repoId`, `userId`, `iat`, `exp`, `jti`, `iss`, `hfCdnTier`, `forceS3Link`, `repo_storage_type`. There is no `Cache-Control`. |
| Token scope | A reference-files token asking for a `gpt2` file hash: `404 (1) not found`. With a `gpt2` token: `200`. |
| `GET /v2/reconstructions` without a token | `401 (4) Unauthorized` |
| `v2` reconstruction | `{terms, xorbs, offset_into_first_range}`, one range per URL, no `Cache-Control`. A `Range` header returns only the covering terms, plus `offset_into_first_range`. |
| `v1` reconstruction | `{terms, fetch_info, offset_into_first_range}`, `url_range` per entry. |
| Xorb URL | `https://us.aws.cdn.hf.co/xorbs/default/{xorb}?user_id=…&repo_id=…&xip=REDACTED&Expires=…&Policy=REDACTED&Signature=REDACTED&Key-Pair-Id=REDACTED`. It expires 3600 s after issue. Its policy is `{"DateLessThan": {"EpochTime": …}, "ByteRange": {"ExpectedHeader": "bytes=A-B"}}`, and `xip` is part of the signed resource. |
| Range enforcement on a xorb URL signed for `bytes=0-14737812` | exact `206`; `0-99` `206`; `1-14737812` `206`; `14737700-14737812` `206`; `0-0` `206`. `0-14737813` `403 invalid range`; `14737813-14769748` (the footer) `403`; `0-` `403`; no `Range` `403 expected range header`. Dropping `xip`: `403 invalid resource`. |
| `user_id` in URLs | `public` for anonymous tokens; the **token owner's user id** for the private repo. |
| `xip` | `xip` enforcement was not tested. |
| Bytes | The CDN range for the reference CSV is byte-identical to the first 14737813 bytes of `testdata/xet-spec-reference/eea25d6e….xorb`. `reference_file_hash_matches` (`crates/protocol/tests/reference_files.rs`) recomputes `118a5332…`, the `X-Xet-Hash`, from those chunks. |
| Shared xorbs | The private 10528-byte file uses chunks 0 to 1 (bytes 0 to 1727) of a 22598282-byte xorb. `gpt2/model.safetensors` has 9 terms over 9 xorbs, one range per URL. |
| `HEAD /v1/files/{hash}` | `200`, `Content-Length` = file size |
| `GET /v2/file-chunk-hashes/{hash}` | Needs `X-Range-Dirty`. With `bytes=1000000-1999999`: `200 {totalChunks, fileSize, windows:[{dirtyByteRange:[987576,2327521]}], hashRanges:[2 subtrees], gapVerification:[]}`. |
| `GET /v1/chunks/default-merkledb/{hash}` with a read token | `404` |
| Private repo, anonymous | `401`, `X-Error-Message: Invalid username or password.` |
| Gated repo | `api/models/{id}` and `tree` are `200` anonymously (`gated: "manual"`, `license:llama3.2` / `license:gemma`). `README.md` and Llama's `LICENSE.txt` resolve anonymously. Weights, `config.json` and `xet-read-token` answer `401 X-Error-Code: GatedRepo` anonymously, and `403 GatedRepo` with a token that has no grant ("not in the authorized list" / "awaiting a review"). |
| Rate limits | Anonymous: `"resolvers";q=3000;w=300` and `"api";q=500;w=300`. Free user token: 5000 and 1000. This matches HF's published tiers (below). |

Two consequences follow from the source:
- **Re-packing preserves file identity.** `file_hash(chunks)` is
  `aggregated_node_hash(chunks).hmac(0)` over `(chunk hash, size)` pairs
  (`xet-core-structures` `merklehash/aggregated_hashes.rs:183-195`). It
  does not depend on which xorb holds a chunk.
- **Per-window verification is possible.** `file-chunk-hashes` returns
  Merkle subtree summaries for the gaps around a dirty window, and "per-chunk
  hashes are never transferred" (`xet-client` `cas_types/mod.rs:384-418`).
  Inferred: merging the gap subtrees with the subtree of chunks fetched
  for one window reproduces the file hash. That verifies one window
  without fetching the whole file. We did not run this merge.

## What a mirror must serve

These are the read paths `huggingface_hub` 2.2.0 takes. With the
fixtures they come from, they are the contract a mirror must meet (source,
observed):

| Client step | Request | The mirror must |
|---|---|---|
| `snapshot_download` | `GET /api/{type}s/{id}/revision/{rev}` (`_snapshot_download.py:279`) | resolve a branch to a commit `sha` (asserted). Ref resolution goes upstream; anything keyed by commit is immutable. |
| `snapshot_download` | `GET /api/{type}s/{id}/tree/{sha}?recursive=true` (`_snapshot_download.py:292`), paginated by `Link: rel="next"` (`utils/_pagination.py:51`) | page through upstream to the end, keep `xetHash`, `lfs.oid` and `oid`, and page its own answer. |
| `hf_hub_download` | `HEAD /{type}/{id}/resolve/{rev}/{path}` (`file_download.py:1666-1688`) | answer with `X-Repo-Commit`, `X-Linked-Size`, `X-Linked-Etag`, `X-Xet-Hash` and an `xet-auth` link **on the mirror's own origin**. Small files: `200` with `ETag` (Git blob SHA-1). |
| Xet download | `GET` the `xet-auth` link (`file_download.py:575`) | mint its **own** token (design §5.4), with body and headers both (S3 Answer 2). |
| Xet download | `GET /v2/reconstructions/{hash}` with `Range` blocks of 256 MB, `416` past EOF; `v1` fallback | serve from local terms with ADR 0002 URLs. |
| Without `hf_xet`, or `hf download` of a small file | `GET resolve`, following the redirect | stream bytes itself (the §5.5 gateway). Never `302` to `*.hf.co`: that bypasses the mirror. |
| Errors | `X-Error-Code` `GatedRepo`, `RepoNotFound`, `RevisionNotFound`, `EntryNotFound`, and the message "Access to this resource is disabled." for a disabled repo (`utils/_http.py:880-935`) | relay the upstream code and message, so `GatedRepoError` still tells the user to request access. |

Notes:
- `parse_xet_file_data_from_response` rewrites `https://huggingface.co/`
  prefixes in the `xet-auth` link to `HF_ENDPOINT` (`utils/_xet.py:75-76`,
  source). So relaying HF's `Link` header by mistake would still reach
  the mirror for the token. The `rel="xet-reconstruction-info"` CAS URL,
  `Location` and `X-Xet-Cas-Url` are not rewritten.
- `hf_hub_download` sends `X-HF-Download-Counter: 1` on download calls
  (`utils/_http.py:265-300`). Inferred: HF counts one download per mirror
  ingest, not per mirror user, so authors' download statistics drop.

## How the mirror ingests (decided by R19; design §10.6)

Inferred from the observations above, and tested only in the pieces
named:
1. **Metadata.** Resolve the ref upstream, with a short TTL; that also
   re-checks access. Fetch `revision` and the full `tree` at the commit
   and store them as a **mirrored commit object in the bucket**, so
   `reindex` and air-gap transfer work (design §5.3). Data keyed by commit
   SHA never changes.
2. **Xet files.**
   - Fetch a read token for that repo; one token cannot cover another
     repo.
   - Fetch the reconstruction. Fetch each term's byte range from HF's URL
     straight away: URLs live 3600 s, and sub-ranges are allowed.
   - Decompress and hash every chunk.
   - Verify the file hash against `X-Xet-Hash`. Large files could later
     verify per 256 MB block through `file-chunk-hashes` windows. Until
     the window-merge proof test passes, a file's shard is registered
     only after the whole-file `X-Xet-Hash` check. Chunks fetched earlier
     stay staged and unreferenced.
   - Check sha256 against `X-Linked-ETag` while streaming.
   - Write the verified chunks into **local xorbs**, keeping each chunk's
     compressed bytes, and register a shard to the mirrored repo's scope.
   - Re-packed xorbs and the generated shard go through the normal upload
     acceptance:
     - xorb hash recomputation;
     - the `verify_shard` S6 rules, including verification entries;
     - the 64 MiB body limit;
     - the `MAX_REFERENCED_CHUNKS` cap, splitting shards as needed.

     Ingest has no trusted bypass.
   - Then serve it through the normal paths.
   - Unverified bytes are never stored or served.
3. **Non-Xet files.**
   - Regular Git blobs: fetch through `resolve` and verify against the
     blob SHA-1 `ETag`.
   - Legacy LFS files: verify against sha256, then chunk them on ingest
     (design §10.3).
4. **First-request latency.** `hf_xet`'s read timeout is 300 s and resets
   on received data (`xet-runtime` `config/groups/client.rs:56-63`). So
   ingesting the requested 256 MB block before answering needs about
   1 MB/s from upstream. That needs measuring before we choose between
   "block until ingested" and "stream through the transfer role while
   ingesting".
5. **Concurrency and quota.**
   - Ingest is single-flight per file.
   - Upstream calls honour `RateLimit` and `429`. HF's published budget is
     per IP for anonymous use and per account with a token.
   - Ref and tree caching keep the API count low.

Never hand HF's signed URLs to clients:
- they expire 3600 s after issue;
- they carry `xip` and, for private repos, the operator's `user_id`;
- the client would bypass the mirror, so nothing would be cached.

## `xet-proxyd` (guilt/xet-server @ `22c4aa1`)

Read: `cmd/xet-proxyd/main.go`, `internal/proxycas/proxycas.go`,
`internal/proxyhub/{proxyhub,resolve,token}.go`, `docs/MIRRORING.md`
(source).

How it works:
- **Two listeners.** A Hub-facing proxy (`HF_ENDPOINT`) and a CAS-facing
  proxy. Each **embeds** the project's normal `hubserver`/`casserver` and
  fills it through `Ingest*` on a miss, then delegates to the normal
  handler. That is how a proxy data directory becomes a plain `xetd`
  data directory for offline use.
- **Tokens.** The real HF token is passed through unchanged. Only
  `casUrl` is rewritten to the proxy. On a CAS `401`, it mints a
  fresh token with the same caller credential and retries once (1.1.0).
- **Xorbs.** It fetches `v1` reconstructions upstream. It caches a xorb
  only if the `fetch_info` ranges start at chunk 0 and hash to the xorb
  hash. Otherwise it relays the reconstruction live, with HF's URLs, and
  caches nothing for that file (`errPartialXorbCoverage`). Its docs call
  it "*not* a dependable way to construct a complete offline library" and
  recommend download-then-upload seeding.
- **Non-Xet files** are always relayed live, and never cached.
- **Metadata** is refreshed upstream first by default.
- **Writes** (commit, preupload, repo create) are relayed to HF.

What to reuse conceptually:
- the mirror writes through the same storage and serving code as normal
  uploads, so mirrored data is native, dedups and survives without the
  upstream;
- single-flight healing of an expired upstream token;
- rate-limiting misses by caller, because a miss costs an upstream call;
- the two-phase "download, then upload" seeding as the fallback when
  upstream is unreachable.

What OpenXet avoids:
- **Caching whole xorbs only.** Partial-xorb files are common (observed
  above). Re-packing verified chunks avoids the problem.
- **Handing HF URLs to clients** on the relay path.
- **Forwarding writes upstream.** That is outside a mirror's job.

## Legal risk for the user

This section quotes texts and says what they literally cover. It is not
legal advice and draws no legal conclusions. Every **⚖ Lawyer:** item
needs counsel before we ship the corresponding mode.

Sources, all fetched 2026-10-23 07:07 UTC:
- HF Terms of Service, https://huggingface.co/terms-of-service
  (effective 15 September 2022)
- HF Supplemental Terms,
  https://cdn-media.huggingface.co/landing/assets/Supplemental+Terms.pdf
  (effective 28 April 2025; one page, covering paid features only)
- HF Content Policy, https://huggingface.co/content-policy (effective
  10 April 2025)
- Gated models, https://huggingface.co/docs/hub/models-gated
- Hub rate limits, https://huggingface.co/docs/hub/rate-limits
- Llama 3.2 Community License, https://huggingface.co/meta-llama/Llama-3.2-1B/blob/main/LICENSE.txt
- Gemma Terms of Use, https://ai.google.dev/gemma/terms (last modified
  1 April 2026)
- Apache-2.0, https://www.apache.org/licenses/LICENSE-2.0.txt

**No separate API terms were found:** `/api-terms` and `/terms-of-use`
return `404`. `robots.txt` is `Allow: /`. The Terms define "Services"
broadly, and the Content Policy is "incorporated into our Terms of
Service".

### What the texts say

**Public content licence (ToS, "Your Content"):**
> If you decide to set your Repository public, you grant each User a
> perpetual, irrevocable, worldwide, royalty-free, non-exclusive license to
> use, display, publish, reproduce, distribute, and make derivative works
> of your Content **through our Services and functionalities**;

> When Content contains notice of a reasonable and customary license, (such
> as an open source license) such Content is intended to remain under the
> terms of such license when further accessed, distributed, or used.
> Neither party is permitted to remove reference to any such license.

> Any Content you download, access or use from us or another User, is at
> your own risk and subject to these Terms and/or the terms accompanying
> such Content.

**Accounts and credentials (ToS):**
> You may not disclose your password to any third party, and you are solely
> responsible for any action taken with your Account.

The indemnity covers claims arising from
> any other party's use of any Service with your credentials

**HF's own materials (ToS, "Intellectual Property"):**
> All materials that we produce, including the Website, design, code,
> graphics, interfaces, trademarks, and logo shall remain our exclusive
> property. You may not alter, reproduce, republish, license any of our
> proprietary materials, unless we expressly give you a written permission
> to do so.

**Termination (ToS):**
> we reserve the right to suspend or terminate your access to the Services
> anytime with or without cause

**Export (ToS):**
> you may only access and use the Service in compliance with U.S. and other
> applicable export control and sanctions laws and regulations.

**Platform abuse (Content Policy, "Restricted Content" 5):**
> Abuse or interference with Hugging Face services, including:
> Using unauthorized bot APIs or remote management tools. …
> Using tools like Cloudflare Tunnel, TOR, proxies, VNC, Chrome Remote
> Server, etc., to bypass restrictions. …
> Spam (e.g., advertising products/services, excessive bulk activity, or
> disrupting user experience).

**Gating (Content Policy definitions and gated-models doc):**
> "Gated": Gated Repositories and their Community Content are visible to
> everyone, but access to their ML artifacts (data, model weights) requires
> either accepting conditions in a click-through form or approval by the
> Repository maintainers.

> Users must agree to share their contact information (username and email
> address) with the model authors to access the model files when enabled.
> … Access requests are always granted to individual users rather than to
> entire organizations.

> The model authors have complete control over model access. In
> particular, they can decide at any time to block your access to the
> model without prior notice

**Takedowns (Content Policy):**
> If this is confirmed, we will inform the User who uploaded the allegedly
> infringing Content and disable it.

**Rate limits (docs):** "we enforce rate limits on all requests made to
the Hugging Face Hub". They are counted "per IP address" for anonymous
users, and per user for accounts ("For organizations, rate limits are
applied individually to each member").

**Llama 3.2 Community License §1.b:**
> i. If you distribute or make available the Llama Materials (or any
> derivative works thereof), or a product or service (including another AI
> model) that contains any of them, you shall (A) provide a copy of this
> Agreement with any such Llama Materials; and (B) prominently display
> "Built with Llama" on a related website, user interface, blogpost, about
> page, or product documentation. …
> iii. You must retain in all copies of the Llama Materials that you
> distribute the following attribution notice within a "Notice" text file
> …
> iv. Your use of the Llama Materials must comply with … the Acceptable Use
> Policy

§2 requires a separate licence from Meta above 700 million monthly
active users.

**Gemma Terms of Use §1.1(b), §3.1:**
> "Distribution" or "Distribute" means any transmission, publication, or
> other sharing of Gemma or Model Derivatives to a third party, including
> by providing or making Gemma or its functionality available as a hosted
> service via API, web access, or any other electronic or remote means
> ("Hosted Service").

> You must include the use restrictions referenced in Section 3.2 as an
> enforceable provision in any agreement (e.g., license agreement, terms of
> use, etc.) governing the use and/or distribution of Gemma or Model
> Derivatives and you must provide notice to subsequent users you
> Distribute to that Gemma or Model Derivatives are subject to the use
> restrictions in Section 3.2.
> You must provide all third party recipients of Gemma or Model
> Derivatives a copy of this Agreement.
> You must cause any modified files to carry prominent notices stating
> that you modified the files.
> All Distributions (other than through a Hosted Service) must be
> accompanied by a "Notice" text file that contains the following notice:
> "Gemma is provided under and subject to the Gemma Terms of Use found at
> ai.google.dev/gemma/terms".

**Apache-2.0 §4** (a common repo licence): redistribution requires giving
recipients "a copy of this License" and keeping a `NOTICE` file's
attribution notices.

### (a) Self-hosted mirror for an organisation's own use

What the texts cover:
- The repo's own licence travels with the content ("intended to remain
  under the terms of such license"). Downloading is "subject to … the
  terms accompanying such Content".
- Licences like Llama 3.2 and Apache-2.0 grant reproduction and
  distribution, with conditions:
  - keep the licence file, `NOTICE` and attribution;
  - Llama: "Built with Llama" when a product or service contains the
    materials;
  - Llama: the Acceptable Use Policy.
- A mirror that copies the whole repo, `LICENSE` and `NOTICE` files
  included, keeps those references. Inferred: the mirror must never strip
  files.
- The Content Policy's list names "proxies … to bypass restrictions" and
  "excessive bulk activity". Inferred: a mirror that obeys HF's rate
  limits and gating has more to argue than one that pools users to evade
  them.
- The operator's HF account carries responsibility for "any action taken
  with your Account".

⚖ Lawyer:
- Is an internal mirror "distribution" or "make available" under the
  licences of the repos the org mirrors (Llama, Gemma, RAIL, CC-BY-NC,
  "other")? Gemma's definition includes any "transmission … to a third
  party". Whether employees or affiliates are third parties is a legal
  question.
- Does re-serving HF-generated metadata count as reproducing "our
  proprietary materials"? This covers model info fields such as
  `securityFileStatus`, `downloads` and `likes`. The mirror could limit
  itself to fields the client needs.

### (b) A hosted mirror on openxet.com for third parties

What the texts cover:
- The ToS licence to other users is granted "through our Services and
  functionalities". openxet.com is not HF's Service. So for content
  served to openxet.com users, the texts leave **only the repo's own
  licence** as the basis.
  - Many repos have no licence, or use "other" or custom terms.
  - Some, like Gemma, define a hosted service as "Distribution", which
    triggers obligations on the distributor: enforceable use
    restrictions in our terms, and a copy of the agreement for every
    recipient.
- HF disables content after DMCA takedowns. A hosted mirror that keeps
  serving the earlier copy is then the one distributing it. Design §15
  already requires DMCA/DSA processes before launch.
- One upstream identity (an IP or a token) serving many users
  concentrates HF's per-IP and per-user rate limits. The Content Policy
  lists "proxies … to bypass restrictions" and "excessive bulk activity".
- HF may "suspend or terminate your access to the Services anytime with
  or without cause". A hosted mirror's upstream is therefore at HF's
  discretion.
- Export and sanctions duties attach to the operator, who re-serves
  content worldwide.
- Download statistics: authors would see one HF download per mirror
  ingest (inferred from `X-HF-Download-Counter`). That is not a legal
  term, but it harms authors and HF and invites friction.

⚖ Lawyer, and **ideally written permission from HF before launch**:
- Does the ToS permit a third-party service to systematically fetch and
  re-serve Hub content? Is a pull-through mirror one of the "proxies …
  to bypass restrictions"?
- What are openxet.com's obligations as a distributor under per-repo
  licences? Can they be met automatically, for example by surfacing the
  licence, adding use restrictions to the openxet.com terms, and
  displaying "Built with Llama"? Or does that need a per-licence
  allow-list?
- What are our host-provider duties (DMCA safe harbour, EU DSA) for
  mirrored content we did not upload, and how fast must upstream removals
  propagate?
- Trademark: we must not present the mirror as Hugging Face.

### (c) Gated or private content

What the texts cover:
- Gated access is granted "to individual users rather than to entire
  organizations", against sharing "contact information … with the model
  authors". Authors can revoke it "at any time".
- A mirror that fetches gated weights with one token and serves them to
  others gives access to people who never agreed or shared their contact
  details. HF answers those people `GatedRepo` (observed).
- The ToS makes the account holder responsible for "any other party's
  use of any Service with your credentials".
- Private repos are visible to "only you or members of your
  organization". For them HF says it "will use reasonable and appropriate
  measures designed to keep your Content confidential". Inferred: a
  mirror copy puts that duty on the mirror operator instead.

Inferred design constraint (not a legal conclusion): gated and private
content must be authorised **per caller, upstream**:
- Each user presents their own HF credential.
- The mirror re-checks access upstream with a short TTL before serving
  cached data.
- When the mirror cannot check, it fails closed.

Sharing one operator token's gated access across users is the pattern
the texts single out.

⚖ Lawyer:
- An org-wide mirror of a gated model, fetched with one member's grant,
  even with accept-on-behalf flows: R19 forbids this; confirm counsel
  agrees.
- If openxet.com ever enables the mirror (R19 disables it for every
  tenant), tenants mirroring their **own** private HF repos would raise
  data-processing (DPA) and confidentiality questions, as for any hosted
  data.

### Headline for the user

- Self-hosted mirroring of public, non-gated repos for an org's own use
  has the most textual support, provided the licence files are kept and
  each repo's licence is honoured.
- A hosted openxet.com mirror for third parties is **high risk on the
  texts**:
  - HF's licence to users covers redistribution "through our Services"
    only;
  - per-model licences put distributor duties on us;
  - the Content Policy names proxies.

  Do not ship it without counsel and, ideally, HF's agreement.
- Gated and private content must never be shared across users through one
  token.

## Design changes (R19)

R19 decided the policy:
- the mirror ships for self-hosted deployments: public, ungated repos,
  with licence files kept;
- gated and private content is served only per caller, never shared
  through one token, with no override;
- the mirror is disabled on openxet.com until counsel review and,
  ideally, an agreement with HF.

Design rules for OpenXet's mirror, from OpenXet's own requirements
(design §5.9 requirement 3, §10.6):
- Token and response caches are keyed by caller, repo and revision.
- A fallback never serves authorized content without a fresh upstream
  check. When the check can't run, the mirror fails closed.
- A user's credential is never combined with the operator's.

The ADR below records the mechanics and stays proposed until it is filed.
Design §10.6 now states:
- **Native ingest by verified chunks.** Never cache whole HF xorbs, and
  never hand out HF URLs.
- **Normal upload acceptance.** Ingest has no trusted bypass.
- **No unverified chunks.**
- **Licence files kept.**
- **Per-caller upstream authorisation** for gated and private content,
  failing closed, with no shared-credential override.
- **Upstream-removal propagation.** Repos HF removes are hidden, and their
  data is reclaimed only by GC (non-negotiable 3).
- **Read-only mirrored namespaces.**
- **Pre-seeding and transfer** through the same ingest path, plus a
  bucket export.
- **The mirror is disabled on openxet.com**, for every tenant, including
  their own private repos, until counsel review and ideally a written
  agreement with HF.

§11 marks S8 answered.

## Proposed ADR (records R19's mechanics; not yet filed)

> **ADR 0006 (proposed): HF mirror ingests verified chunks and authorises
> per caller.**
>
> - Status: proposed
> - Scope: `hub`, `cas`, `auth`, `transfer`, `hosted`
>
> **Context.** Controller ruling R19 is the governing decision. It
> covers:
> - self-hosted mirroring of public, ungated repos, with licence files
>   kept;
> - per-caller access to gated and private content, with no override;
> - no mirror on openxet.com until counsel review.
>
> This ADR records the mechanics. Design §10.6 promises a pull-through
> mirror. S8 observed
> how huggingface.co behaves:
> - Read tokens live 900 s and are scoped per repo.
> - Xorb URLs live 3600 s, are range-contained, and carry `xip` and the
>   owner's `user_id`.
> - Files reference parts of shared xorbs.
> - Gated access is per user.
>
> `xet-proxyd` shows that caching whole xorbs leaves many files
> uncacheable. The legal texts give a self-hosted mirror of
> public content the most support, and a hosted third-party mirror or
> shared gated access the least.
>
> **Decision.**
> 1. **Upstreams are configured per namespace pattern.** For example,
>    `huggingface.co` for `*` or for `meta-llama/*`. Mirrored repos keep
>    their HF ids and are read-only on the mirror: writes get `403` with a
>    hint to push upstream.
> 2. **Ingest, not relay.**
>    - Xet files are fetched by reconstruction and xorb ranges with a
>      per-repo upstream token.
>    - Every chunk is decompressed and hashed.
>    - The file hash must equal `X-Xet-Hash`, and sha256 must equal
>      `X-Linked-ETag`.
>    - Verified chunks are re-packed into local xorbs, and a shard is
>      registered to the mirrored repo's scope.
>    - Non-Xet files are verified by Git blob SHA-1 or sha256.
>    - Mirrored commit metadata is written to the bucket.
>    - Clients only ever see OpenXet URLs and tokens.
>    - Until the window-merge proof test passes, a file's shard is
>      registered only after the whole-file `X-Xet-Hash` check. Chunks
>      fetched earlier stay staged and unreferenced.
>    - Re-packed xorbs and the generated shard go through the normal
>      upload acceptance:
>      - xorb hash recomputation;
>      - the `verify_shard` S6 rules, including verification entries;
>      - the 64 MiB body limit;
>      - the `MAX_REFERENCED_CHUNKS` cap, splitting shards as needed.
>
>      Ingest has no trusted bypass.
>    - The mirror serves every file of a mirrored revision, including
>      `LICENSE`, `NOTICE` and the README, and never filters them out of
>      `tree` or `resolve`.
> 3. **Authorisation.**
>    - Public, ungated repos: the OpenXet authorizer alone.
>    - Gated and private repos: the OpenXet authorizer **and** an
>      upstream check with the caller's own HF credential, failing
>      closed. The credential is passed as `X-HF-Upstream-Token` or set up
>      through `openxet login` (a builder-check call). It is never
>      persisted or logged, and is used only for that caller's check.
>    - The check cache is keyed by (caller, repo, revision) and holds only
>      yes or no, with a TTL.
>    - Gated and private content is never fetched for or served to one
>      caller using another caller's or the operator's HF credential;
>      there is no override (R19).
> 4. **Freshness.**
>    - Refs are re-resolved upstream with a TTL (default 60 s).
>    - Data keyed by commit is immutable.
>    - While upstream is unreachable, stale serving, with a warning
>      header, covers only content whose last successful upstream check
>      was public and ungated. A later gated, private or `404` answer
>      hides it.
> 5. **Upstream removal.** Upstream `404`, a disabled repo or `451` hides
>    the mirrored repo. Its data is reclaimed only by GC.
> 6. **Air gap.**
>    - `openxet mirror pull <repo>[@rev]` pre-seeds through the same
>      ingest.
>    - `openxet mirror export|import` moves the bucket objects of the
>      chosen scopes. The receiving side runs `reindex`.
> 7. **Hosted.** **The mirror is disabled on openxet.com**, for every
>    tenant, including their own private repos, until counsel review and
>    ideally a written agreement with HF.
>
> **Consequences.**
> - Mirrored data dedups with local data at chunk level and is fully
>   offline-capable.
> - Ingest costs CPU, for decompression and hashing, and needs a
>   benchmark.
> - First-download latency must be measured against `hf_xet`'s 300 s
>   read timeout.
> - The `file-chunk-hashes` window verification needs a proof test before
>   we rely on it for streaming ingest.

## Not verified

- `xip` enforcement was not tested.
- The `file-chunk-hashes` subtree merge for per-window verification. We
  read the source but did not run it.
- Files whose Xet chunks HF created by migrating from LFS. We assumed
  they use the same chunk hashing; any file re-packed by our ingest
  verifies against `X-Xet-Hash` either way.
- Datasets with `tree` pagination; no response we saw had `Link:
  rel="next"`.
- `huggingface.js` and `git-xet` read paths through a mirror.
- Whether HF publishes mirror-specific terms or a partner programme. We
  found none.

## Method

- The probes were a stdlib-only Python client (`http.client`, redirects
  not followed), run from the scratchpad and kept outside the repo. The
  HF token was read from a file into the environment only. Output was
  redacted before it went into `compat/fixtures/s8/` (the redaction guard
  passes).
- `xet-server` was shallow-cloned into its own scratch directory and read
  only.
- The legal texts were fetched with `curl` and converted to text for
  quoting. Quotes are verbatim; ellipses mark cuts.
