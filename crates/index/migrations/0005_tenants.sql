-- Tenants (ruling M1-R38): the scope is the unit of access, the tenant the
-- unit of isolation. Xorbs are owned per tenant, and a registration
-- records its tenant and the sequence number its bucket record carries
-- (ruling M1-R37), so every row here is derivable from the bucket's
-- registrations/ and xorb-owners/ records with the stored shards and xorbs.
-- Rows from before this migration take the tenant a credential naming none
-- gets: `scope:` and the scope (ruling M1-R43), which no explicit tenant can
-- equal.

-- The tenants a xorb is owned by: those it was uploaded under, and those
-- of every registration whose files reference it (ruling M1-R39), so
-- switching `global` to `per-tenant` never hides a registered file's xorbs
-- from its own tenant.
CREATE TABLE xorb_owners (
    xorb BLOB NOT NULL REFERENCES xorbs (hash),
    tenant TEXT NOT NULL,
    PRIMARY KEY (xorb, tenant)
);

INSERT INTO xorb_owners (xorb, tenant)
SELECT xorb, 'scope:' || scope FROM xorb_scopes;

-- Ownership by reference, for shards registered since migration 0002
-- recorded their xorbs. Earlier shards gain it when registered again.
INSERT INTO xorb_owners (xorb, tenant)
SELECT sx.xorb, 'scope:' || ss.scope FROM shard_xorbs sx JOIN shard_scopes ss ON ss.shard = sx.shard
WHERE true
ON CONFLICT DO NOTHING;

DROP TABLE xorb_scopes;

-- One row per registration of a shard to a scope for a tenant, as one
-- bucket record names it.
CREATE TABLE shard_registrations (
    shard BLOB NOT NULL,
    scope TEXT NOT NULL,
    tenant TEXT NOT NULL,
    registered_seq INTEGER NOT NULL,
    PRIMARY KEY (shard, scope, tenant)
);

INSERT INTO shard_registrations (shard, scope, tenant, registered_seq)
SELECT shard, scope, 'scope:' || scope, registered_seq FROM shard_scopes;

DROP INDEX shard_scopes_registered_seq;
DROP TABLE shard_scopes;

-- Orders the newest-registration lookup of dedup answers.
CREATE INDEX shard_registrations_registered_seq ON shard_registrations (registered_seq);

-- The sequence number of the registration a file's row came from. Of two
-- registrations of one file to one scope, the lower number's row is kept,
-- whichever committed first, so the row is the one a rebuild from the
-- bucket picks.
ALTER TABLE file_scopes ADD COLUMN registered_seq INTEGER NOT NULL DEFAULT 0;

UPDATE file_scopes SET registered_seq = COALESCE(
    (SELECT r.registered_seq FROM shard_registrations r
     WHERE r.shard = file_scopes.shard AND r.scope = file_scopes.scope),
    0
);

-- The last registration sequence number handed out: one row, id 0.
CREATE TABLE registration_seq (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    last INTEGER NOT NULL
);

INSERT INTO registration_seq (id, last)
SELECT 0, COALESCE(MAX(registered_seq), 0) FROM shard_registrations;
