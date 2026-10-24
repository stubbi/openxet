/** Durable operations are atomic at D1's statement or batch boundary. */
export function createWaitlistStore(db) {
  return {
    async save({ email, receiptHash, createdAt, privacyVersion }) {
      await db.batch([
        db.prepare("INSERT INTO waitlist_entries (email, created_at, privacy_version) VALUES (?, ?, ?) ON CONFLICT(email) DO NOTHING")
          .bind(email, createdAt, privacyVersion),
        db.prepare("INSERT INTO waitlist_receipts (receipt_hash, email, created_at) VALUES (?, ?, ?)")
          .bind(receiptHash, email, createdAt),
      ]);
    },

    async remove(receiptHash) {
      await db.prepare("DELETE FROM waitlist_entries WHERE email = (SELECT email FROM waitlist_receipts WHERE receipt_hash = ?)")
        .bind(receiptHash).run();
    },

    async consume(key, limit, expiresAt) {
      const row = await db.prepare(`INSERT INTO waitlist_rate_limits (bucket, attempts, expires_at)
        VALUES (?, 1, ?) ON CONFLICT(bucket) DO UPDATE SET attempts = attempts + 1
        WHERE attempts < ? RETURNING attempts`).bind(key, expiresAt, limit).first();
      return row !== null;
    },

    async cleanRates(now) {
      await db.prepare("DELETE FROM waitlist_rate_limits WHERE expires_at <= ?").bind(now).run();
    },
  };
}
