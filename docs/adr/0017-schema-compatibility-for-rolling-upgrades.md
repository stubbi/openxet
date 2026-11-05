# 0017: Keep migrations additive, and mark the rare contract migration with a schema epoch

<!-- description: Index migrations are expand migrations unless marked contract; a contract raises a schema epoch, and a node opens a database with migrations it doesn't know as long as the epoch allows, so nodes of two adjacent releases share one database through a rolling upgrade. -->

- Status: accepted
- Date: 2026-11-05
- Scope: `index`, `server`, `docs`
- Ruling: M3-R6 (design §17); issue [#47](https://github.com/stubbi/openxet/issues/47)

## Context

OpenXet nodes are stateless and scale out behind one database (design
§5.8). Upgrading them one at a time means that, for a while, nodes of
release N and N+1 serve the same database. Design §15 lists zero-downtime
rolling upgrades among what openxet.com needs from the core, and §16 asks
for upgrade tests between consecutive versions.

M1 opens the index with sqlx's migrator, which refuses a database that
carries a migration the binary doesn't know. The first N+1 node to start
migrates the database, and every N node still running, or restarting,
then refuses it: a rolling upgrade becomes a full stop.

Most schema changes are additive and harmless to an older binary: a new
table, a nullable or defaulted column, a new index. A few aren't: a
dropped column or table, or a narrowed type, breaks a binary that still
reads it. The policy has to let the first kind through and stop the
second, and say which is which in the migration itself.

## Decision

1. **Expand by default.** A migration is an *expand* migration (new
   tables, nullable or defaulted columns, new indexes) unless its file's
   first line is `-- openxet: contract`. A test checks every shipped
   migration declares its kind truthfully.
2. **A contract migration raises the epoch.** It only drops or narrows
   what no binary of the previous release reads, and it raises
   `schema_compat.min_binary_epoch` in the database.
3. **Each binary carries `openxet_index::SCHEMA_EPOCH`.** A node opens a
   database that holds migrations it doesn't know (sqlx's
   `set_ignore_missing(true)`) as long as `min_binary_epoch <=
   SCHEMA_EPOCH`, and otherwise refuses to start with `schema_too_new`
   and a hint naming the release to upgrade to.
4. **Migrating.** `index.migrate = "auto"` (the default) applies pending
   migrations at start-up, on Postgres under an advisory lock so two
   nodes starting together apply each migration once. `"off"` leaves it
   to `openxet migrate [--dry-run] [--json]` (`openxet.migrate.v1`).
5. **Release rule.** A contract migration ships only in a release whose
   predecessor no longer reads what it removes, and is applied only after
   every node runs that predecessor. The rolling-upgrade guide states the
   procedure; the multi-node rig runs it under load.
6. Migrations are numbered at merge time, in both the SQLite and the
   Postgres migration directories.

## Consequences

- Nodes of two adjacent releases share one database through a rolling
  upgrade, as long as the newer release adds only expand migrations.
  None in M3 is a contract migration.
- Removing a column takes two releases: one that stops reading it, and a
  later one whose contract migration drops it. That is slower to clean
  up, and the epoch makes the cost visible.
- An older node may run against tables and columns it doesn't know.
  Writes from a newer node must therefore stay readable by the older one:
  a new column has a default the older binary's writes satisfy.
- Tests: `older_binary_opens_database_with_expand_migration`,
  `older_binary_refuses_after_contract_migration`,
  `migration_files_declare_their_kind`, `concurrent_migrate_runs_once`,
  `migrate_dry_run_changes_nothing` and `migrate_json_schema`; the
  `rolling_upgrade` drill runs nodes of the merge base and the head
  against one database under load.

## Alternatives considered

- **Refuse unknown migrations (the M1 behaviour).** Safe, but it turns
  every upgrade into a stop of every node. Rejected.
- **Allow binaries a fixed window of N migrations ahead, without
  epochs.** It counts migrations, not what they do, so it can't tell an
  added index from a dropped column, and can't express a contract.
  Rejected.
- **Separate migration tooling outside the binary.** Operators would run
  and version one more thing, against "nothing to babysit". `openxet
  migrate` covers the operator who wants control, and `auto` the one who
  doesn't. Rejected.
