-- Repositories and their Git data (ADR 0004, rulings M2-R2, M2-R12,
-- M2-R13). SQLite only: the Postgres migrations of M3 are written
-- separately. Ids are the 16 raw bytes of a UUIDv7, OIDs the 20 raw bytes of a
-- SHA-1. Times are Unix seconds unless named otherwise.
--
-- Repository metadata, labels and purges are bucket records first and
-- index rows second (design §5.3). Refs are authoritative here while the
-- database exists; the bucket ref log is derived from ref_log, and reindex
-- rebuilds refs, ref_log and packs from the bucket (ADR 0004 item 4).
-- Rows that outlive their repository (packs, ref_log) carry no foreign key,
-- so deleting a repository hands its packs to GC instead of orphaning them.

-- name_key is lower(namespace) || '/' || lower(name), ASCII only since
-- names are ASCII (M2-R2), computed by openxet rather than SQL so SQLite
-- and Postgres agree. labels is a JSON array of strings.
CREATE TABLE repos (
    id BLOB PRIMARY KEY,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    name_key TEXT NOT NULL UNIQUE,
    private INTEGER NOT NULL,
    labels TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    full_repack_gen INTEGER NOT NULL
);

CREATE TABLE refs (
    repo BLOB NOT NULL REFERENCES repos (id),
    name TEXT NOT NULL,
    oid BLOB NOT NULL,
    PRIMARY KEY (repo, name)
);

-- One row per ref command of every committed transaction (ADR 0004 item
-- 3). AUTOINCREMENT, so a sequence number is never handed out twice, even
-- after old rows are pruned: a repack's high-water mark compares against
-- it. old is NULL for a create, new for a delete.
CREATE TABLE ref_log (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    repo BLOB NOT NULL,
    ref TEXT NOT NULL,
    old BLOB,
    new BLOB,
    actor TEXT NOT NULL,
    request TEXT NOT NULL,
    at INTEGER NOT NULL
);

CREATE INDEX ref_log_repo_seq ON ref_log (repo, seq);

-- ADR 0004 items 3 to 5. superseded_at is set when a repack replaces the
-- pack, for GC's grace period.
CREATE TABLE packs (
    id BLOB PRIMARY KEY,
    repo BLOB NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'committed', 'lost', 'expired', 'superseded', 'recovered')
    ),
    created_at INTEGER NOT NULL,
    bytes INTEGER NOT NULL,
    midx_gen INTEGER,
    superseded_at INTEGER
);

CREATE INDEX packs_repo_state ON packs (repo, state);

-- One repack leaseholder per repository (ADR 0004 item 5). epoch increases
-- with every acquisition; expires_at is Unix milliseconds on the database
-- clock, since leases are compared on it.
CREATE TABLE repo_leases (
    repo BLOB PRIMARY KEY REFERENCES repos (id),
    holder TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

-- What reindex could not place (ADR 0004 item 4, ruling R24): kind is
-- commit, tag or edge with an oid, or pack (a recovered pack) without one.
CREATE TABLE reindex_unplaced (
    repo BLOB NOT NULL REFERENCES repos (id),
    kind TEXT NOT NULL CHECK (kind IN ('commit', 'tag', 'edge', 'pack')),
    oid BLOB,
    pack BLOB NOT NULL,
    found_at INTEGER NOT NULL,
    CHECK ((kind = 'pack') = (oid IS NULL))
);

-- One entry per (repo, kind, object, pack). A plain UNIQUE would treat the
-- NULL oids of pack entries as distinct.
CREATE UNIQUE INDEX reindex_unplaced_entry
    ON reindex_unplaced (repo, kind, COALESCE(oid, X''), pack);

-- Append-only (ruling M2-R12; M4's audit API reads it): the triggers
-- refuse every change to a written row. repo is NULL for an action on no
-- repository; detail is a JSON object. No foreign key: the log outlives
-- what it describes.
CREATE TABLE audit_log (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    repo BLOB,
    detail TEXT NOT NULL,
    at INTEGER NOT NULL
);

CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log
BEGIN
    SELECT RAISE(ABORT, 'audit_log is append-only');
END;

CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log
BEGIN
    SELECT RAISE(ABORT, 'audit_log is append-only');
END;

-- Files purged (ruling M2-R13), as the bucket's purges/ records name them.
-- Who purged is in the audit_log row and the bucket record.
CREATE TABLE purges (
    file BLOB PRIMARY KEY,
    at INTEGER NOT NULL
);

-- file_by_sha256 looks a file up by its client-asserted SHA-256 within one
-- scope (design §5.4).
CREATE INDEX file_scopes_scope_sha256 ON file_scopes (scope, sha256);
