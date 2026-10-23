-- openxet index schema. The bucket is the source of truth (design §5.3);
-- every row here can be rebuilt from stored xorbs and shards.
-- Hashes are the 32 raw bytes of a MerkleHash.

-- Content-addressed xorbs. chunk_table is (hash 32 B, uncompressed length
-- u32 LE, offset u32 LE) per chunk, then the final offset (u32 LE).
-- ingested_seq orders xorbs by first ingestion for dedup answers.
CREATE TABLE xorbs (
    hash BLOB PRIMARY KEY,
    size INTEGER NOT NULL,
    chunk_count INTEGER NOT NULL,
    chunk_table BLOB NOT NULL,
    ingested_seq INTEGER NOT NULL
);

-- The scopes a xorb was uploaded to; per-scope visibility filters on it.
CREATE TABLE xorb_scopes (
    xorb BLOB NOT NULL REFERENCES xorbs (hash),
    scope TEXT NOT NULL,
    PRIMARY KEY (xorb, scope)
);

-- Content-addressed file reconstructions. terms is (xorb 32 B, chunk start
-- u32 LE, chunk end u32 LE, unpacked bytes u32 LE) per term.
CREATE TABLE files (
    hash BLOB PRIMARY KEY,
    size INTEGER NOT NULL,
    terms BLOB NOT NULL
);

-- Files registered to a scope, with the client-asserted SHA-256, which is
-- only ever trusted within the scope that registered it (design §5.4).
CREATE TABLE file_scopes (
    file BLOB NOT NULL REFERENCES files (hash),
    scope TEXT NOT NULL,
    sha256 BLOB,
    shard BLOB NOT NULL,
    PRIMARY KEY (file, scope)
);

CREATE TABLE shard_scopes (
    shard BLOB NOT NULL,
    scope TEXT NOT NULL,
    PRIMARY KEY (shard, scope)
);

-- Global dedup candidates: eligible chunks and files' first chunks.
CREATE TABLE dedup (
    chunk BLOB NOT NULL,
    xorb BLOB NOT NULL REFERENCES xorbs (hash),
    PRIMARY KEY (chunk, xorb)
);
