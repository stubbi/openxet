import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

const implementation = await import("../lib/waitlist.mjs").catch(() => ({}));
const storage = await import("../lib/waitlist-store.mjs").catch(() => ({}));
const origin = "https://openxet.com";
const service = "https://openxet-waitlist.jannes73978.chatgpt.site";
const rateKey = "test-only-rate-key-with-at-least-32-characters";
const now = 1_800_000_000;

function request(body, overrides = {}) {
  return new Request(`${service}/api/waitlist`, {
    method: "POST",
    headers: { Origin: origin, "Content-Type": "application/json", "CF-Connecting-IP": "192.0.2.1", ...overrides.headers },
    body: JSON.stringify(body),
    ...Object.fromEntries(Object.entries(overrides).filter(([key]) => key !== "headers")),
  });
}

function setup(t) {
  assert.equal(typeof implementation.createWaitlistHandler, "function", "the public waitlist handler is implemented");
  assert.equal(typeof storage.createWaitlistStore, "function", "the durable waitlist store is implemented");
  const db = new DatabaseSync(":memory:");
  db.exec("PRAGMA foreign_keys = ON");
  const directory = resolve("drizzle");
  const migrations = readdirSync(directory).filter(name => name.endsWith(".sql")).sort();
  assert.ok(migrations.length, "the service has a generated database migration");
  for (const migration of migrations) db.exec(readFileSync(resolve(directory, migration), "utf8"));
  t.after(() => db.close());
  const prepare = (sql, values = []) => ({
    bind(...args) { return prepare(sql, args); },
    async first() { return db.prepare(sql).get(...values) ?? null; },
    async all() { return { results: db.prepare(sql).all(...values), success: true }; },
    async run() {
      const result = db.prepare(sql).run(...values);
      return { success: true, meta: { changes: Number(result.changes) } };
    },
    _sql: sql,
    _values: values,
  });
  const d1 = {
    prepare,
    async batch(statements) {
      db.exec("BEGIN");
      try {
        const results = [];
        for (const statement of statements) results.push(await statement.all());
        db.exec("COMMIT");
        return results;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
  };
  const store = storage.createWaitlistStore(d1);
  const handler = implementation.createWaitlistHandler({ store, rateKey, now: () => now });
  return { db, store, handler };
}

test("saves normalized email durably and returns an opaque cancellation receipt", async t => {
  const { db, handler } = setup(t);
  const result = await handler(request({ email: "  Alice+launch@Example.COM  ", website: "" }));
  assert.equal(result.status, 201);
  const body = await result.json();
  assert.equal(body.schema, "openxet.waitlist.v1");
  assert.equal(body.code, "request_saved");
  assert.match(body.receipt, /^[A-Za-z0-9_-]{43}$/);
  assert.equal(body.email, undefined);
  assert.equal(db.prepare("SELECT email FROM waitlist_entries").get().email, "alice+launch@example.com");
  assert.equal(db.prepare("SELECT privacy_version FROM waitlist_entries").get().privacy_version, "2026-10-24");
  assert.ok(!JSON.stringify(db.prepare("SELECT * FROM waitlist_receipts").all()).includes(body.receipt));
  assert.equal(result.headers.get("access-control-allow-origin"), origin);
  assert.equal(result.headers.get("cache-control"), "no-store");
});

test("duplicates keep one email row and return fresh capabilities without disclosing membership", async t => {
  const { db, handler } = setup(t);
  const first = await handler(request({ email: "person@example.com", website: "" }));
  const second = await handler(request({ email: "PERSON@example.com", website: "" }));
  const a = await first.json();
  const b = await second.json();
  assert.equal(first.status, second.status);
  assert.equal(a.code, b.code);
  assert.equal(a.message, b.message);
  assert.notEqual(a.receipt, b.receipt);
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 1);
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_receipts").get().n, 2);
});

test("a valid receipt withdraws the address, invalidates its other receipts, and remains safe to retry", async t => {
  const { db, handler } = setup(t);
  const first = await (await handler(request({ email: "person@example.com", website: "" }))).json();
  const second = await (await handler(request({ email: "person@example.com", website: "" }))).json();
  const cancel = receipt => request({ receipt }, { body: JSON.stringify({ receipt }) });
  const response = await handler(cancel(first.receipt), "cancel");
  assert.equal(response.status, 200);
  assert.equal((await response.json()).code, "request_removed");
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 0);
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_receipts").get().n, 0);
  assert.equal((await handler(cancel(second.receipt), "cancel")).status, 200);
  assert.equal((await handler(cancel(first.receipt), "cancel")).status, 200);
});

test("an invented receipt cannot remove another request", async t => {
  const { db, handler } = setup(t);
  await handler(request({ email: "person@example.com", website: "" }));
  const response = await handler(request({ receipt: "x".repeat(43) }), "cancel");
  assert.equal(response.status, 200);
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 1);
});

for (const email of ["", "not-an-email", "person@localhost", "name\r\n@example.com", "a..b@example.com", "a@-example.com", "a@exam_ple.com", "a".repeat(65) + "@example.com", "x@y." + "z".repeat(252)]) {
  test(`rejects malformed email ${JSON.stringify(email).slice(0, 70)}`, async t => {
    const { db, handler } = setup(t);
    const response = await handler(request({ email, website: "" }));
    assert.equal(response.status, 400);
    const body = await response.json();
    assert.equal(body.code, "invalid_email");
    assert.ok(body.message && body.hint);
    assert.equal(body.email, undefined);
    assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 0);
  });
}

test("does not accept a filled bot trap or store that address", async t => {
  const { db, handler } = setup(t);
  const response = await handler(request({ email: "person@example.com", website: "https://bot.example" }));
  assert.equal(response.status, 400);
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 0);
});

test("rejects foreign and missing origins without permissive CORS", async t => {
  const { db, handler } = setup(t);
  for (const value of ["https://evil.example", "https://openxet.com.evil.example", "null", ""]) {
    const response = await handler(request({ email: "person@example.com" }, { headers: { Origin: value } }));
    assert.equal(response.status, 403);
    assert.equal(response.headers.get("access-control-allow-origin"), null);
    assert.equal(response.headers.get("access-control-expose-headers"), null);
  }
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 0);
});

test("preflight permits only the approved origin, POST and Content-Type", async t => {
  const { handler } = setup(t);
  const response = await handler(new Request(`${service}/api/waitlist`, {
    method: "OPTIONS",
    headers: { Origin: origin, "Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "content-type" },
  }));
  assert.equal(response.status, 204);
  assert.equal(response.headers.get("access-control-allow-origin"), origin);
  assert.equal(response.headers.get("access-control-allow-methods"), "POST, OPTIONS");
  assert.equal(response.headers.get("access-control-allow-credentials"), null);
});

test("no GET endpoint reads or mutates the waitlist", async t => {
  const { db, handler } = setup(t);
  for (const operation of ["signup", "cancel"]) {
    const response = await handler(new Request(`${service}/api/waitlist`, { headers: { Origin: origin } }), operation);
    assert.equal(response.status, 405);
    assert.equal(response.headers.get("allow"), "POST, OPTIONS");
  }
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 0);
});

test("rejects invalid JSON and unsupported media without storing a request", async t => {
  const { handler } = setup(t);
  assert.equal((await handler(request({}, { body: "{" }))).status, 400);
  assert.equal((await handler(request({}, { headers: { "Content-Type": "text/plain" } }))).status, 415);
  assert.equal((await handler(request(null))).status, 400);
});

test("bounds streamed request bodies without trusting Content-Length", async t => {
  const { handler } = setup(t);
  let cancelled = false;
  const bytes = new TextEncoder().encode("x".repeat(2049));
  const body = new ReadableStream({
    pull(controller) { controller.enqueue(bytes); },
    cancel() { cancelled = true; },
  });
  const response = await handler(new Request(`${service}/api/waitlist`, {
    method: "POST", body, duplex: "half", headers: { Origin: origin, "Content-Type": "application/json" },
  }));
  assert.equal(response.status, 413);
  assert.equal(cancelled, true);
});

test("URL-encoded submissions return a receipt link without reflecting email", async t => {
  const { handler } = setup(t);
  const response = await handler(request({}, {
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: "email=person%40example.com&website=",
  }));
  assert.equal(response.status, 201);
  assert.match(response.headers.get("content-type"), /text\/html/);
  const html = await response.text();
  assert.match(html, /Your request is saved/);
  assert.match(html, /\/cancel#[A-Za-z0-9_-]{43}/);
  assert.ok(!html.includes("person@example.com"));
  assert.match(html, /noindex/);
  assert.equal(response.headers.get("referrer-policy"), "strict-origin");
});

test("form failures remain readable and do not render submitted markup", async t => {
  const { handler } = setup(t);
  const response = await handler(request({}, {
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: "email=%3Cscript%3Ealert(1)%3C%2Fscript%3E&website=",
  }));
  assert.equal(response.status, 400);
  const html = await response.text();
  assert.match(html, /Enter an email address/);
  assert.ok(!html.includes("alert(1)"));
});

test("database failure never reports success or includes the underlying exception", async t => {
  const { store } = setup(t);
  const broken = { ...store, save: async () => { throw new Error("database credentials private person@example.com"); } };
  const handler = implementation.createWaitlistHandler({ store: broken, rateKey, now: () => now });
  const response = await handler(request({ email: "person@example.com", website: "" }));
  assert.equal(response.status, 503);
  const body = await response.text();
  assert.ok(!body.includes("person@example.com"));
  assert.ok(!body.includes("credentials"));
  assert.match(body, /temporarily_unavailable/);
});

test("an unconfigured service fails closed", async t => {
  const { store } = setup(t);
  for (const options of [{ store }, { rateKey }]) {
    const handler = implementation.createWaitlistHandler(options);
    assert.equal((await handler(request({ email: "person@example.com" }))).status, 503);
  }
});

test("per-address rate limit persists and never stores a raw client IP", async t => {
  const { db, handler, store } = setup(t);
  for (let i = 0; i < 10; i++) assert.equal((await handler(request({ email: `person${i}@example.com`, website: "" }))).status, 201);
  const restarted = implementation.createWaitlistHandler({ store, rateKey, now: () => now });
  const blocked = await restarted(request({ email: "person11@example.com", website: "" }));
  assert.equal(blocked.status, 429);
  assert.ok(Number(blocked.headers.get("retry-after")) > 0);
  assert.ok(!JSON.stringify(db.prepare("SELECT * FROM waitlist_rate_limits").all()).includes("192.0.2.1"));
  const other = await handler(request({ email: "other@example.com", website: "" }, { headers: { "CF-Connecting-IP": "192.0.2.2" } }));
  assert.equal(other.status, 201);
});

test("the rate store atomically bounds global writes across client keys", async t => {
  const { store } = setup(t);
  const outcomes = await Promise.all(Array.from({ length: 7 }, () => store.consume("test-global", 4, now + 3600)));
  assert.equal(outcomes.filter(Boolean).length, 4);
});

test("allowed cross-origin clients can read the rate-limit retry delay", async t => {
  const { handler } = setup(t);
  for (let i = 0; i < 10; i++) await handler(request({ email: "person@example.com", website: "" }));
  const limited = await handler(request({ email: "person@example.com", website: "" }));
  assert.equal(limited.status, 429);
  assert.equal(limited.headers.get("access-control-allow-origin"), origin);
  assert.equal(limited.headers.get("access-control-expose-headers"), "Retry-After");
  assert.ok(Number(limited.headers.get("retry-after")) > 0);
  assert.equal(limited.headers.get("access-control-allow-credentials"), null);
});

for (const [operation, clientLimit, blockedAttempts, acceptedStatus] of [["signup", 10, 500, 201], ["cancel", 30, 1000, 200]]) {
  test(`${operation}: one rate-limited client cannot exhaust capacity for other clients`, async t => {
    const { db, handler } = setup(t);
    const body = operation === "signup" ? { email: "person@example.com", website: "" } : { receipt: "x".repeat(43) };
    for (let i = 0; i < clientLimit; i++) assert.equal((await handler(request(body), operation)).status, acceptedStatus);
    for (let i = 0; i < blockedAttempts; i++) assert.equal((await handler(request(body), operation)).status, 429);
    const freshClient = await handler(request(body, { headers: { "CF-Connecting-IP": "192.0.2.2" } }), operation);
    assert.equal(freshClient.status, acceptedStatus);
    const global = db.prepare("SELECT attempts FROM waitlist_rate_limits WHERE bucket = ?").get(`${operation}:global:${Math.floor(now / 86400)}`);
    assert.equal(global.attempts, clientLimit + 1);
  });
}

test("a failed receipt insert rolls back the email write", async t => {
  const { db, store } = setup(t);
  await store.save({ email: "first@example.com", receiptHash: "collision", createdAt: now, privacyVersion: "2026-10-24" });
  await assert.rejects(store.save({ email: "second@example.com", receiptHash: "collision", createdAt: now, privacyVersion: "2026-10-24" }));
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 1);
});

test("cancellation failure keeps the saved request and gives a removal-specific next step", async t => {
  const { db, store, handler } = setup(t);
  const saved = await (await handler(request({ email: "person@example.com", website: "" }))).json();
  const failing = implementation.createWaitlistHandler({ store: { ...store, remove: async () => { throw new Error("private database error"); } }, rateKey, now: () => now });
  const response = await failing(request({ receipt: saved.receipt }), "cancel");
  assert.equal(response.status, 503);
  const body = await response.json();
  assert.match(body.message, /remove/);
  assert.match(body.hint, /removal link/);
  assert.equal(db.prepare("SELECT count(*) AS n FROM waitlist_entries").get().n, 1);
});

test("failed cancellation forms explain that removal did not happen", async t => {
  const { handler } = setup(t);
  const response = await handler(request({}, {
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: "receipt=invalid",
  }), "cancel");
  assert.equal(response.status, 400);
  assert.match(await response.text(), /Request not removed/);
});
