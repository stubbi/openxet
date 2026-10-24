CREATE TABLE `waitlist_entries` (
	`email` text PRIMARY KEY NOT NULL,
	`created_at` integer NOT NULL,
	`privacy_version` text NOT NULL
);
--> statement-breakpoint
CREATE TABLE `waitlist_rate_limits` (
	`bucket` text PRIMARY KEY NOT NULL,
	`attempts` integer NOT NULL,
	`expires_at` integer NOT NULL
);
--> statement-breakpoint
CREATE INDEX `waitlist_rate_expiry_idx` ON `waitlist_rate_limits` (`expires_at`);--> statement-breakpoint
CREATE TABLE `waitlist_receipts` (
	`receipt_hash` text PRIMARY KEY NOT NULL,
	`email` text NOT NULL,
	`created_at` integer NOT NULL,
	FOREIGN KEY (`email`) REFERENCES `waitlist_entries`(`email`) ON UPDATE no action ON DELETE cascade
);
--> statement-breakpoint
CREATE INDEX `waitlist_receipts_email_idx` ON `waitlist_receipts` (`email`);