-- Global dedup answers offer the xorbs of the newest shard that references
-- the matched xorb (ruling M1-R30). Both are derived from stored shards and
-- registrations, so they are rebuildable like every other row.

-- The order shards were registered to a scope in, for "newest". Rows from
-- before this migration share 0 and count as oldest.
ALTER TABLE shard_scopes ADD COLUMN registered_seq INTEGER NOT NULL DEFAULT 0;

-- register_shard takes MAX(registered_seq) + 1 under the write lock; this
-- keeps it one seek, and orders the dedup lookup.
CREATE INDEX shard_scopes_registered_seq ON shard_scopes (registered_seq);

-- The distinct xorbs a shard's files reference, pos counting from 0 in
-- order of first reference across the files' terms (the shard's term
-- order, `UploadShard::referenced_xorbs`). Shards registered before this
-- migration have no rows until they are registered again; their dedup
-- answers offer the single matched xorb.
CREATE TABLE shard_xorbs (
    shard BLOB NOT NULL,
    pos INTEGER NOT NULL,
    xorb BLOB NOT NULL REFERENCES xorbs (hash),
    PRIMARY KEY (shard, pos)
);

CREATE INDEX shard_xorbs_xorb ON shard_xorbs (xorb);
