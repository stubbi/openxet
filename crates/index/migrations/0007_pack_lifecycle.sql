-- The pack sweep's clocks and claims (ADR 0004 item 5).

-- When a pack became committed, Unix seconds on the database clock: GC
-- deletes a multi-pack-index generation once a newer one has been
-- committed for the grace period. NULL for a pack never committed by a
-- transaction: pending, lost, expired, or written by reindex.
ALTER TABLE packs ADD COLUMN committed_at INTEGER;

-- When the row was inserted, or reindex discard made it lost, Unix seconds
-- on the database clock: GC ages pending, lost and expired packs by it. created_at is the age of the
-- pack's objects, which a repack's outputs inherit from their oldest
-- input, so it can't say how long a row has been in flight. Rows from
-- before this migration take their created_at, the best time they have.
ALTER TABLE packs ADD COLUMN inserted_at INTEGER NOT NULL DEFAULT 0;
UPDATE packs SET inserted_at = created_at;

-- A pack name the sweep is deleting the files of. A pack's name is its
-- checksum, so a new row can name files the sweep is about to delete (a
-- push sent again, a byte-identical repack output): the claim is inserted
-- only while no pending, committed or recovered row names the pack, and
-- inserting a pack row fails, retryably, while a claim on its name exists.
-- The sweep removes the claim once the files are gone, and finishes any
-- claim an interrupted sweep left.
CREATE TABLE pack_deletions (
    repo BLOB NOT NULL,
    name TEXT NOT NULL,
    claimed_at INTEGER NOT NULL,
    PRIMARY KEY (repo, name)
);
