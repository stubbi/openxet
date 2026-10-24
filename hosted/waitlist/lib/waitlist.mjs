import { resultPage } from "./waitlist-pages.mjs";

export const SERVICE_ORIGIN = "https://openxet-waitlist.jannes73978.chatgpt.site";
export const PRIVACY_VERSION = "2026-10-24";
const SCHEMA = "openxet.waitlist.v1";
const MAX_BODY = 2048;
const ORIGINS = new Set(["https://openxet.com", SERVICE_ORIGIN]);
const encoder = new TextEncoder();

const problems = {
  origin_not_allowed: [403, "This page cannot send a waitlist request.", "Use the form at https://openxet.com/waitlist/."],
  method_not_allowed: [405, "This action needs a form submission.", "Send a POST request with Content-Type: application/json."],
  invalid_request: [400, "We could not read this request.", "Check the form and try again."],
  invalid_email: [400, "Enter an email address such as you@example.com.", "Check the address and submit the form again."],
  invalid_receipt: [400, "This removal code is not valid.", "Use the private removal link saved after your request."],
  body_too_large: [413, "This request is too large.", "Send only your email address and the form fields."],
  unsupported_media_type: [415, "This request format is not supported.", "Use application/json or application/x-www-form-urlencoded."],
  rate_limited: [429, "Too many requests arrived recently.", "Wait a while, then try again. Your existing request is unchanged."],
  temporarily_unavailable: [503, "We could not save your request right now.", "Keep your address here and try again in a few minutes."],
};

function emailAddress(value) {
  if (typeof value !== "string") return null;
  const email = value.trim().toLowerCase();
  if (email.length > 254 || /[\r\n]/.test(value)) return null;
  const parts = email.split("@");
  if (parts.length !== 2) return null;
  const [local, domain] = parts;
  if (!local || local.length > 64 || local.startsWith(".") || local.endsWith(".") || local.includes("..")) return null;
  if (!/^[a-z0-9.!#$%&'*+\-/=?^_`{|}~]+$/.test(local)) return null;
  const labels = domain.split(".");
  if (labels.length < 2 || labels.some(label => !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))) return null;
  return email;
}

async function readBody(request) {
  const declared = request.headers.get("content-length");
  if (declared && (!/^\d+$/.test(declared) || Number(declared) > MAX_BODY)) {
    await request.body?.cancel();
    throw "body_too_large";
  }
  if (!request.body) throw "invalid_request";
  const reader = request.body.getReader();
  const parts = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_BODY) {
        await reader.cancel();
        throw "body_too_large";
      }
      parts.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const part of parts) { bytes.set(part, offset); offset += part.byteLength; }
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

async function sha256(value) {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(value)));
  return Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
}

function newReceipt() {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}

async function clientHash(rateKey, request) {
  const key = await crypto.subtle.importKey("raw", encoder.encode(rateKey), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const address = request.headers.get("cf-connecting-ip") || "address-unavailable";
  const digest = new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(address)));
  return Array.from(digest, byte => byte.toString(16).padStart(2, "0")).join("");
}

/**
 * The only anonymous authority is to request launch contact or withdraw it.
 * @param {{store?: ReturnType<import("./waitlist-store.mjs").createWaitlistStore>, rateKey?: string, now?: () => number}} options
 */
export function createWaitlistHandler({ store, rateKey, now = () => Math.floor(Date.now() / 1000) } = {}) {
  return async function handle(request, operation = "signup") {
    const origin = request.headers.get("origin");
    const allowed = ORIGINS.has(origin);
    const media = (request.headers.get("content-type") || "").split(";")[0].trim().toLowerCase();
    const html = media === "application/x-www-form-urlencoded";
    const headers = {
      "Cache-Control": "no-store",
      "Referrer-Policy": "strict-origin",
      "X-Content-Type-Options": "nosniff",
      "X-Robots-Tag": "noindex, nofollow",
      "Vary": "Origin",
      ...(allowed ? { "Access-Control-Allow-Origin": origin, "Access-Control-Expose-Headers": "Retry-After" } : {}),
    };
    function reply(status, body, more = {}) {
      const responseHeaders = { ...headers, ...more, "Content-Type": html ? "text/html; charset=utf-8" : "application/json; charset=utf-8" };
      if (html) responseHeaders["Content-Security-Policy"] = "default-src 'none'; style-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";
      return new Response(html ? resultPage(body, operation) : JSON.stringify(body), { status, headers: responseHeaders });
    }
    function fail(code, more = {}) {
      const [status, message, hint] = code === "temporarily_unavailable" && operation === "cancel"
        ? [503, "We could not remove your request right now.", "Keep your private removal link and try again in a few minutes."]
        : problems[code];
      return reply(status, { schema: SCHEMA, code, message, hint }, more);
    }
    if (!["POST", "OPTIONS"].includes(request.method)) return fail("method_not_allowed", { Allow: "POST, OPTIONS" });
    if (!allowed) return fail("origin_not_allowed");
    if (request.method === "OPTIONS") {
      const requested = (request.headers.get("access-control-request-headers") || "").split(",").map(value => value.trim().toLowerCase()).filter(Boolean);
      if (request.headers.get("access-control-request-method") !== "POST" || requested.some(value => value !== "content-type")) return fail("origin_not_allowed");
      return new Response(null, { status: 204, headers: { ...headers, "Access-Control-Allow-Methods": "POST, OPTIONS", "Access-Control-Allow-Headers": "Content-Type", "Access-Control-Max-Age": "600" } });
    }
    if (!store || typeof rateKey !== "string" || rateKey.length < 32) return fail("temporarily_unavailable");
    if (!["application/json", "application/x-www-form-urlencoded"].includes(media)) return fail("unsupported_media_type");

    let input;
    try {
      const text = await readBody(request);
      if (html) {
        const fields = new URLSearchParams(text);
        if ([...new Set(fields.keys())].some(key => fields.getAll(key).length !== 1)) return fail("invalid_request");
        input = Object.fromEntries(fields);
      } else input = JSON.parse(text);
      if (!input || typeof input !== "object" || Array.isArray(input)) return fail("invalid_request");
    } catch (error) {
      return fail(error === "body_too_large" ? error : "invalid_request");
    }

    let email;
    if (operation === "cancel") {
      if (typeof input.receipt !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(input.receipt)) return fail("invalid_receipt");
    } else {
      if (input.website !== undefined && input.website !== "") return fail("invalid_request");
      email = emailAddress(input.email);
      if (!email) return fail("invalid_email");
    }

    try {
      const seconds = now();
      const day = Math.floor(seconds / 86400);
      const hour = Math.floor(seconds / 3600);
      const globalExpiry = (day + 1) * 86400;
      const clientExpiry = (hour + 1) * 3600;
      await store.cleanRates(seconds);
      const client = await clientHash(rateKey, request);
      if (!await store.consume(`${operation}:client:${hour}:${client}`, operation === "cancel" ? 30 : 10, clientExpiry)) return fail("rate_limited", { "Retry-After": String(clientExpiry - seconds) });
      if (!await store.consume(`${operation}:global:${day}`, operation === "cancel" ? 1000 : 500, globalExpiry)) return fail("rate_limited", { "Retry-After": String(globalExpiry - seconds) });
      if (operation === "cancel") {
        await store.remove(await sha256(input.receipt));
        return reply(200, { schema: SCHEMA, code: "request_removed", message: "Your waitlist request has been removed.", hint: "You can join again whenever you choose." });
      }
      const receipt = newReceipt();
      await store.save({ email, receiptHash: await sha256(receipt), createdAt: seconds, privacyVersion: PRIVACY_VERSION });
      return reply(201, { schema: SCHEMA, code: "request_saved", message: "Your request is saved.", hint: "Keep your private removal link if you want to cancel this request later.", receipt });
    } catch {
      return fail("temporarily_unavailable");
    }
  };
}
