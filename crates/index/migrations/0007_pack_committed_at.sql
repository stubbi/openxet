-- When a pack became committed, Unix seconds on the database clock (ADR
-- 0004 item 5): GC deletes a multi-pack-index generation once a newer one
-- has been committed for the grace period, and a pack's created_at can't
-- say when that was (a repack's outputs carry their oldest input's time).
-- NULL for a pack never committed by a transaction: pending, lost,
-- expired, or written by reindex.
ALTER TABLE packs ADD COLUMN committed_at INTEGER;
