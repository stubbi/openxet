-- OpenXet workspace registry. Per user and per machine; every row can be
-- rebuilt (`openxet workspace adopt --scan` re-registers worktrees, and the
-- dedup index refills as files are hashed again).
-- Paths are the raw bytes of the OS path. Times are nanoseconds since the
-- Unix epoch.

-- One row per registered Git worktree. project is the canonical path of the
-- repository's common Git directory.
CREATE TABLE workspaces (
    id BLOB PRIMARY KEY,
    project TEXT NOT NULL,
    path BLOB NOT NULL UNIQUE,
    branch TEXT,
    created_at INTEGER NOT NULL,
    last_active INTEGER NOT NULL,
    adopted INTEGER NOT NULL
);

CREATE INDEX workspaces_project ON workspaces (project);

-- The dedup index: for each (size, BLAKE3 hash) of a whole file, the path
-- of the copy others are cloned from.
CREATE TABLE canonical_files (
    size INTEGER NOT NULL,
    hash BLOB NOT NULL,
    path BLOB NOT NULL,
    PRIMARY KEY (size, hash)
);

-- forget_canonical drops every entry under a removed or evicted directory.
CREATE INDEX canonical_files_path ON canonical_files (path);
