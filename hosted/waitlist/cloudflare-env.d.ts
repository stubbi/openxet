declare namespace Cloudflare {
  interface Env {
    DB?: D1Database;
    WAITLIST_RATE_LIMIT_KEY?: string;
    BUCKET?: R2Bucket;
  }
}
