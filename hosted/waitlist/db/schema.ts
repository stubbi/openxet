import { integer, sqliteTable, text, index } from "drizzle-orm/sqlite-core";

export const waitlistEntries = sqliteTable("waitlist_entries", {
  email: text("email").primaryKey(),
  createdAt: integer("created_at").notNull(),
  privacyVersion: text("privacy_version").notNull(),
});

export const waitlistReceipts = sqliteTable("waitlist_receipts", {
  receiptHash: text("receipt_hash").primaryKey(),
  email: text("email").notNull().references(() => waitlistEntries.email, { onDelete: "cascade" }),
  createdAt: integer("created_at").notNull(),
}, table => [index("waitlist_receipts_email_idx").on(table.email)]);

export const waitlistRateLimits = sqliteTable("waitlist_rate_limits", {
  bucket: text("bucket").primaryKey(),
  attempts: integer("attempts").notNull(),
  expiresAt: integer("expires_at").notNull(),
}, table => [index("waitlist_rate_expiry_idx").on(table.expiresAt)]);
