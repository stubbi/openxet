-- Where each term of a registered file is stored: (start u32 LE, end u32 LE)
-- per term, the bytes offsets[chunk_start]..offsets[chunk_end] of its xorb,
-- derived from xorb_layouts at registration. Reconstruction then reads a
-- layout only for a term it trims, the first or last of a range, instead of
-- every xorb a large file names. 8 bytes per term. NULL for files
-- registered before this migration, which are planned from layouts.
ALTER TABLE file_scopes ADD COLUMN term_bytes BLOB;
