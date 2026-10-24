import { env } from "cloudflare:workers";
import { createWaitlistHandler } from "./waitlist.mjs";
import { createWaitlistStore } from "./waitlist-store.mjs";

export function waitlistRequest(request: Request, operation = "signup") {
  return createWaitlistHandler({
    store: env.DB ? createWaitlistStore(env.DB) : undefined,
    rateKey: env.WAITLIST_RATE_LIMIT_KEY,
  })(request, operation);
}
