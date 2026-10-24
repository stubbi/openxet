# openxet hosted waitlist

This small service accepts launch-access requests from the static openxet site.
It stores email addresses in a private managed D1 database. It creates no storage
accounts, sends no email, and has no public list, search, export or admin route.

The source runs through the Sites plugin's Vinext/Cloudflare Worker starter.
Keep its Sites build integration and the project ID in `.openai/hosting.json`.
The primary website remains in the openxet repository and on GitHub Pages.
Unused starter UI components, examples, styling dependencies and application
wrappers have been removed. The imported Sites/Vinext build and preview
integration remains intact; this service uses plain elements and its own CSS.

## Contract

| Route | Behavior |
| --- | --- |
| `GET /` | Accessible HTML form; works without JavaScript |
| `POST /api/waitlist` | Save a request, then return a private removal receipt |
| `GET /cancel` | Removal confirmation form; never changes data |
| `POST /api/waitlist/cancel` | Remove the address identified by a valid receipt |
| `OPTIONS` on either API | CORS preflight for the two permitted origins |
| `GET` on either API | `405`; no data or membership lookup |

API requests use JSON or URL-encoded form bodies, at most 2,048 UTF-8 bytes.
JSON responses have schema `openxet.waitlist.v1`. Success is returned only after
the D1 operation completes. URL-encoded requests receive a readable HTML result.

Signup JSON:

```json
{"email":"you@example.com","website":""}
```

`website` is an empty bot-trap field. It is not stored. Email is trimmed,
lowercased and validated, with a 254-character maximum and an ordinary ASCII
email-address format. Plus addressing is preserved. There is no DNS lookup,
address verification, or inference of an identity from an email address.

A `201` response contains `code: "request_saved"`, a message, a hint, and
`receipt`: an unguessable 256-bit base64url capability (43 characters). The
database stores only its SHA-256 hash. Do not put the receipt in analytics,
logs or query strings. Link to:

```text
https://openxet-waitlist.jannes73978.chatgpt.site/cancel#RECEIPT
```

The fragment is not sent in the HTTP request. The confirmation page copies it
into the form and removes it from the address bar. A user must press **Remove my
request** before a POST occurs. With JavaScript off, the code can be pasted into
the labeled form field. Removal JSON is `{"receipt":"..."}`.

Valid, unknown and previously used receipts all receive the same removal
response. A successful removal deletes the email row and cascades to all of its
receipt hashes. Repeating the same removal is safe. A fabricated receipt cannot
remove any row without knowing a valid capability.

## Duplicate requests and consent boundary

One normalized email address occupies one waitlist entry. Every successful
submission receives a fresh receipt; existing receipts remain valid until the
address is removed. New and duplicate addresses get the same status and text,
and no response reveals an existing receipt or whether an address was on the
list before the request.

This is deliberately an **unverified request for launch contact**, not proof
that the submitter controls the address. Anyone able to submit an address can
obtain a capability to withdraw that address. This limits unwanted enrollment
without granting data access. Do not use this table for authentication,
authorization, billing, verified subscriptions, or mass marketing. Add verified
email ownership and a reviewed sending workflow before sending mail. This
version does not promise a confirmation email.

## Abuse limits and privacy

- Browser origins are exactly `https://openxet.com` and the service's own origin.
  Missing, opaque and other origins are rejected. CORS does not authorize a
  person: the public API intentionally permits only bounded signup/removal.
- Agents must send an allowed `Origin` and should submit an address only on its
  owner's instruction. No cookies, account token or shared write credential is
  required.
- A runtime secret, `WAITLIST_RATE_LIMIT_KEY`, must contain at least 32 random
  bytes encoded as text. Generate it through the deployment environment, never
  commit it or send it to the browser. The service fails closed if it is absent.
- D1 rate counters bound signups to 500 attempts per UTC day across the service
  and 10 per UTC hour per client IP. Removal has independent limits of 1,000 per
  day and 30 per hour. Client limits are checked before the global limit, so
  repeated requests from a blocked client do not consume other clients' capacity.
  Counters use atomic SQL upserts and survive Worker restarts.
- Client addresses are HMACed with the runtime secret before storage. Raw IP
  addresses, user agents, referrers and submitted bot-trap fields are not stored
  by this application. Expired rate counters are removed on the next accepted
  API request. Hosting infrastructure may keep its own operational logs.
- Email rows store only normalized email, first request time and privacy-notice
  version. Receipt rows store only hash, associated email and creation time.
- Errors do not echo email, receipt, database exceptions or credentials. Request
  handlers do not log bodies. API and form results use `Cache-Control: no-store`
  and `Referrer-Policy: strict-origin`. The referrer policy omits URL paths and
  preserves the Origin header that native form submissions need; `no-referrer`
  would turn that header into `null` under [Fetch §3.2](https://fetch.spec.whatwg.org/#origin-header)
  and fail the origin check.

`CF-Connecting-IP` is consumed as the hosting edge's client-address header. If it
is absent, requests share a conservative fallback rate bucket. Before launch,
verify the platform supplies the intended client address; never substitute a
client-provided `X-Forwarded-For` value. Rotate the HMAC secret only deliberately,
because doing so resets the effective per-client rate buckets.

The daily limits are intentional early-waitlist bounds, not throughput claims.
Raise them in a reviewed source change if legitimate demand requires it. A
`429` includes `Retry-After` in seconds, exposed to the allowed browser origins
through CORS. It never changes an existing signup.

## Owner operations

The owner can inspect/export the private `waitlist_entries` table with Sites'
database tools. Keep exports private. Export only email, creation time and
privacy version for a reviewed launch-contact workflow; never export receipt
hashes or rate counters for marketing. A SQL export, where supported, is:

```sql
SELECT email, created_at, privacy_version
FROM waitlist_entries
ORDER BY created_at;
```

Users can withdraw immediately using their receipt. No automatic email-retention
schedule is deployed in this version. The maintainer reviews the list quarterly
and deletes waitlist records and any exports within 90 days after the waitlist
closes. This is an operator procedure, not automatic expiry. Stop collecting
entries when the waitlist is no longer needed, record its closure date, and
track completion of deletion. Do not publish email addresses in GitHub
issues to request support or removal.

## Development and checks

Use Node.js 22.13+ for the starter; the dependency-free handler tests use Node's
`node:sqlite` and are verified on Node 26.7.0.

```sh
node --test tests/waitlist.test.mjs
npm run db:generate
node tests/http-smoke.mjs http://127.0.0.1:8792
```

The tests use a real in-memory SQLite database with the generated migrations,
including foreign keys and transactions. They exercise save/duplicate/removal,
rollback on failure, rate limits, origin handling, input and streamed-body
bounds, form fallback, and redacted failure responses.

Follow the Sites skill for installing, building, local D1 migration application,
source synchronization and publication. Native tools own publication; do not
run `wrangler deploy`. Keep the generated SQL and Drizzle migration metadata in
source control. The build must include the `DB` binding and the runtime secret.

Before publishing, review both JSON and ordinary HTML form paths with a
synthetic `example.com` address, verify a database row exists, follow the private
removal link, verify the removal, and leave no test email in production. The HTTP
smoke script sets Origin itself and checks response headers; it does not replace
a real browser's native form submission test, including the no-JavaScript path.
Sending
mail is outside this service and requires its own explicit operation.

## Main-site integration

Use a real `<form method="post">` with its action pointing to the absolute
`/api/waitlist` URL. Add progressive JavaScript if desired:

1. Keep the address in the form while the request is pending or failed.
2. Send `{email, website: ""}` as JSON with `credentials: "omit"`.
3. A success requires a successful response, schema `openxet.waitlist.v1`,
   `code: "request_saved"`, and a 43-character receipt.
4. Show **Your request is saved** and the private removal link. Never imply
   that email ownership is verified, an account exists, or an email was sent.
5. Present `message` and `hint` as text, not HTML. A failed request stays failed;
   do not switch to localStorage or a pretend confirmation.

Use the same field labels and privacy explanation as the main site's
`design.md`. Forms must remain usable at 320 CSS pixels, with visible keyboard
focus, a persistent label and a touch target of at least 44 pixels.
