-- What reconstruction planning reads of a xorb's chunk table, without the
-- chunk hashes: (uncompressed length u32 LE, offset u32 LE) per chunk, then
-- the final offset (u32 LE). Planning a file over 1,000 full xorbs reads
-- 8 MB here instead of 40 MB of chunk tables. Kept in its own table so the
-- read never walks chunk_table's overflow pages. Xorbs stored before this
-- migration have no row; their layout is read from xorbs.chunk_table.
CREATE TABLE xorb_layouts (
    xorb BLOB PRIMARY KEY REFERENCES xorbs (hash),
    chunk_layout BLOB NOT NULL
);
