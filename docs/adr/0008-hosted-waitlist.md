# 0008: Keep the hosted waitlist separate from storage identity

<!-- description: A small managed Worker and private D1 database collect hosted launch requests while the static website stays on GitHub Pages. -->

- Status: accepted
- Date: 2026-10-24
- Scope: `hosted`, `site`

## Context

The user requested a final-product website with hosted sign-up disabled and a
working waitlist, and authorized a simple service and database. GitHub Pages
serves the existing static marketing/docs site and cannot persist form posts.
Product design §15 permits operational hosted tooling behind the same core
seams; §4 and AGENTS.md forbid OpenXet-owned user accounts.

## Decision

1. Keep openxet.com and docs on the existing static GitHub Pages pipeline.
2. Use the connected Sites platform to provision one small Worker service and
   a private D1 database for launch-contact requests. Its source and migrations
   are versioned, including the exact deployment identity. This adds no
   dependency to the open-source storage server.
3. The public endpoint accepts only a bounded launch request, stores an email,
   timestamp, privacy-notice version and hashed cancellation capability, and
   returns success only after persistence. No public read/list/export route.
   Rate-control records are short-lived and use keyed IP hashes, never raw IPs.
4. The request is an unverified expression of interest. It creates no account,
   password, profile, storage permission or external-identity binding. No
   marketing or launch email is sent by this implementation.
5. A random receipt capability permits explicit cancellation; a GET never
   deletes data. Owner-only administration supports export and removal. The
   public form explains the use of the address and links the privacy notice.
6. Waitlist records are operational contact data, not Xet file objects. Privacy
   cancellation deletes these records without invoking the file garbage
   collector; it does not weaken the bucket-as-truth or GC-only file lifecycle.
7. Hosted authentication remains a future external-identity flow at the app
   origin. Do not activate sign-up/sign-in before that flow exists.
8. The maintainer reviews the list quarterly and deletes it, including exports,
   within 90 days after the waitlist closes. This is an explicit operating
   procedure, not a claim of an installed scheduled cleanup job. Withdrawal
   through a valid receipt is immediate. No automatic newsletter migration.

## Consequences

- The waitlist works without a full application backend, a new identity system,
  or exposing an API credential to the browser.
- The runtime and database are managed by Sites/Cloudflare; the privacy page
  names the actual processing arrangement. Do not promise a region, formal
  certification or contractual guarantees not established with the provider.
- Owner access to the Sites project is operationally required. Export uses the
  protected database tools, never a public administration endpoint.
- The list is not verified ownership of email addresses. Before using it for
  ongoing communications, add an appropriate email-verification/unsubscribe
  flow. Requested launch contact is distinct from a general newsletter.
- Domain consolidation or moving this service into a broader hosted backend
  later requires a migration/export plan and updated notice, not silent changes
  to the data collector.

## Alternatives

- **Browser-local storage:** rejected; it cannot collect requests for the owner.
- **A public Notion database or unauthenticated table API:** rejected; addresses
  and write credentials must not be exposed.
- **A full CRM/newsletter integration now:** unnecessary for a launch request.
- **Pretend success while waiting for a backend:** rejected; acknowledged writes
  must be real.
