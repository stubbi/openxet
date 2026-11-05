-- OpenXet's Postgres index schema: the CAS tables of SQLite's migrations
-- 0001 to 0005, at their end state (ruling M3-R2). The bucket is the
-- source of truth (design §5.3); every row here can be rebuilt from stored
-- xorbs and shards and the bucket's registrations/ and xorb-owners/
-- records (ruling M1-R37). The repository and Git tables of SQLite's 0006
-- and 0007 come with the Postgres Repos and GitStore (M3 plan, Task 4).
--
-- Hashes are the 32 raw bytes of a MerkleHash. The blob encodings are
-- SQLite's, documented in crates/index/migrations/. Text that is compared
-- for order (tenants, in the file_scopes tie-break) or used as a key is
-- COLLATE "C", so it orders byte-wise as SQLite's BINARY does and a
-- rebuild from the bucket keeps the same rows on either backend.
--
-- The tables keyed by a content hash are hash-partitioned on it, 16 ways
-- (design §5.8); changing the count is a migration.

-- Orders xorbs by first ingestion for dedup answers. A sequence, so
-- concurrent writers never compute the same number; a value taken by an
-- insert that conflicts is skipped, which keeps the order.
CREATE SEQUENCE xorb_ingested_seq;

CREATE TABLE xorbs (
    hash BYTEA PRIMARY KEY,
    size BIGINT NOT NULL,
    chunk_count BIGINT NOT NULL,
    chunk_table BYTEA NOT NULL,
    ingested_seq BIGINT NOT NULL DEFAULT nextval('xorb_ingested_seq')
) PARTITION BY HASH (hash);

-- What reconstruction planning reads of a xorb: its chunk lengths and
-- offsets without the chunk hashes. Every xorb has a row.
CREATE TABLE xorb_layouts (
    xorb BYTEA PRIMARY KEY REFERENCES xorbs (hash),
    chunk_layout BYTEA NOT NULL
) PARTITION BY HASH (xorb);

-- The tenants a xorb is owned by (rulings M1-R38, M1-R39).
CREATE TABLE xorb_owners (
    xorb BYTEA NOT NULL REFERENCES xorbs (hash),
    tenant TEXT COLLATE "C" NOT NULL,
    PRIMARY KEY (xorb, tenant)
) PARTITION BY HASH (xorb);

-- Files registered to a scope, each with that scope's own reconstruction
-- (ruling M1-R16). Of several registrations of one file to one scope, the
-- row of the lowest (registered_seq, shard, registered_tenant) is kept
-- (ruling M1-R37).
CREATE TABLE file_scopes (
    file BYTEA NOT NULL,
    scope TEXT COLLATE "C" NOT NULL,
    size BIGINT NOT NULL,
    terms BYTEA NOT NULL,
    sha256 BYTEA,
    shard BYTEA NOT NULL,
    term_bytes BYTEA NOT NULL,
    registered_seq BIGINT NOT NULL,
    registered_tenant TEXT COLLATE "C" NOT NULL,
    PRIMARY KEY (file, scope)
) PARTITION BY HASH (file);

-- Global dedup candidates: eligible chunks and files' first chunks.
CREATE TABLE dedup (
    chunk BYTEA NOT NULL,
    xorb BYTEA NOT NULL REFERENCES xorbs (hash),
    PRIMARY KEY (chunk, xorb)
) PARTITION BY HASH (chunk);

DO $$
DECLARE
    parent TEXT;
    remainder INT;
BEGIN
    FOREACH parent IN ARRAY ARRAY['xorbs', 'xorb_layouts', 'xorb_owners', 'file_scopes', 'dedup']
    LOOP
        FOR remainder IN 0..15 LOOP
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF %I FOR VALUES WITH (MODULUS 16, REMAINDER %s)',
                parent || '_p' || remainder, parent, remainder
            );
        END LOOP;
    END LOOP;
END
$$;

-- One row per registration of a shard to a scope for a tenant, as one
-- bucket record names it.
CREATE TABLE shard_registrations (
    shard BYTEA NOT NULL,
    scope TEXT COLLATE "C" NOT NULL,
    tenant TEXT COLLATE "C" NOT NULL,
    registered_seq BIGINT NOT NULL,
    PRIMARY KEY (shard, scope, tenant)
);

-- The distinct xorbs a shard's files reference, pos counting from 0 in
-- the shard's term order (ruling M1-R30).
CREATE TABLE shard_xorbs (
    shard BYTEA NOT NULL,
    pos BIGINT NOT NULL,
    xorb BYTEA NOT NULL REFERENCES xorbs (hash),
    PRIMARY KEY (shard, pos)
);

CREATE INDEX shard_xorbs_xorb ON shard_xorbs (xorb);

-- The registration sequence number a registration takes before it writes
-- its bucket record (ruling M1-R37).
CREATE SEQUENCE registration_seq;
