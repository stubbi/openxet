import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";

const base = process.argv[2];
assert.ok(base, "pass the local or explicitly authorized deployed service origin");
const origin = "https://openxet.com";
const headers = { Origin: origin, "Content-Type": "application/json" };
const home = await fetch(base);
assert.equal(home.status, 200);
assert.equal(home.headers.get("cache-control"), "no-store");
assert.match(home.headers.get("x-robots-tag"), /noindex/);
assert.equal(home.headers.get("referrer-policy"), "strict-origin");
const homeHtml = await home.text();
assert.match(homeHtml, /Join the waitlist/);
assert.match(homeHtml, /<meta name="referrer" content="strict-origin"\/?\s*>/);
const cancelForm = await fetch(`${base}/cancel`);
assert.equal(cancelForm.status, 200);
assert.equal(cancelForm.headers.get("cache-control"), "no-store");
assert.match(cancelForm.headers.get("x-robots-tag"), /noindex/);
assert.equal(cancelForm.headers.get("referrer-policy"), "strict-origin");
assert.match(await cancelForm.text(), /<meta name="referrer" content="strict-origin"\/?\s*>/);
const noRead = await fetch(`${base}/api/waitlist`);
assert.equal(noRead.status, 405);
const signup = await fetch(`${base}/api/waitlist`, {
  method: "POST", headers,
  body: JSON.stringify({ email: `smoke-${randomUUID()}@example.com`, website: "" }),
});
assert.equal(signup.status, 201, `signup status was ${signup.status}`);
const saved = await signup.json();
assert.equal(saved.schema, "openxet.waitlist.v1");
assert.equal(saved.code, "request_saved");
assert.match(saved.receipt, /^[A-Za-z0-9_-]{43}$/);
const remove = await fetch(`${base}/api/waitlist/cancel`, {
  method: "POST", headers,
  body: JSON.stringify({ receipt: saved.receipt }),
});
assert.equal(remove.status, 200);
assert.equal((await remove.json()).code, "request_removed");
const untrusted = await fetch(`${base}/api/waitlist`, {
  method: "POST", headers: { ...headers, Origin: "https://untrusted.example" },
  body: JSON.stringify({ email: "ignored@example.com" }),
});
assert.equal(untrusted.status, 403);
assert.equal(untrusted.headers.get("access-control-allow-origin"), null);
console.log("HTTP smoke passed: form, no public read, durable signup, removal, origin rejection. Synthetic request removed.");
