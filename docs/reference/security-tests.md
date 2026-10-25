# Security tests

Each security requirement in design §5.9 is a release blocker with a named
test. This page maps each requirement to its tests and the file they live
in, so a reviewer can check that a requirement is enforced and see what
runs when one of its tests fails.

A test is planned for the milestone that builds the surface it guards:
M1 for the CAS, M2 for the Hub and Git, M5 for buckets, caches and the
mirror.
Pending rows name tests that are not written yet; their surface does not
exist yet either. A row marked `pending (M2, <file>)` names the file its
milestone's plan puts the test in
([M2 plan](../superpowers/plans/2026-10-25-m2-hub-git-lfs.md)); the task
that writes the test drops the `pending`.

`scripts/check-security-tests-map.py` checks this page in CI against the
tests that actually run:
- every mapped Rust test is listed by `cargo nextest list` exactly once
  in the file the page names, and is not ignored (a test mapped to two
  files, one half of the requirement in each, is checked in both);
- every mapped compatibility test is collected by pytest from its file;
- every test name in design §5.9 is in the table below;
- every M0 or M1 row names a file; only M2 and later rows are pending,
  and a planned file's milestone matches its row's.

Run them all with:

```bash
cargo nextest run --workspace
uv run --directory compat pytest tests/test_fixture_redaction.py
OPENXET_BIN=$PWD/target/debug/openxet uv run --directory compat pytest -m agent
OPENXET_BIN=$PWD/target/debug/openxet uv run --directory compat pytest -m storage_only
# The map check, against the tests that run:
cargo nextest list --workspace --message-format json > /tmp/tests.json
uv run --directory compat pytest --collect-only -q > /tmp/compat-tests.txt
scripts/check-security-tests-map.py --nextest /tmp/tests.json --pytest /tmp/compat-tests.txt
```

Every server the compatibility suite starts, in both the `agent` and the
`storage_only` legs, has its log scanned at teardown for every credential
its clients sent (`assert_no_credential_in_log`).

## Requirements and their tests

| Req | Test | File | Milestone |
| --- | --- | --- | --- |
| 1 | `no_unauthenticated_reads` | `crates/cas/tests/reconstruction.rs` | M1 |
| 1 | `signed_url_bound_to_range` | `crates/auth/tests/auth.rs` | M1 |
| 1 | `no_unauthenticated_reads` | pending (M2, `crates/hub/tests/gateway.rs`) | M2 (Hub, gateway and LFS URLs) |
| 1 | `anonymous_reads_only_with_rule` | `crates/hub/tests/security.rs` | M2 |
| 2 | `credential_scoped_to_repo` | `crates/cas/tests/reconstruction.rs` | M1 (CAS routes) |
| 2 | `unreadable_repo_matches_unknown_repo_on_every_route` | `crates/hub/tests/security.rs` | M2 (Hub routes) |
| 2 | `gateway_needs_credential_scoped_to_repo` | pending (M2, `crates/hub/tests/gateway.rs`) | M2 (file gateway) |
| 2 | `repo_create_authorized_per_namespace` | `crates/hub/tests/security.rs` | M2 |
| 2 | `wrong_type_without_read_access_matches_unknown_repo` | `crates/hub/tests/security.rs` | M2 |
| 2 | `create_collision_authorized_first` | `crates/hub/tests/security.rs` | M2 |
| 2 | `credential_scoped_to_repo`, bucket routes | pending | M5 |
| 2 | `bucket_create_authorized_per_namespace` | pending | M5 |
| 2 | `bucket_add_file_hash_must_be_readable` | pending | M5 |
| 2 | `bucket_copy_file_requires_source_read` | pending | M5 |
| 3 | `cache_keyed_by_caller` | pending | M5 |
| 3 | `cache_fails_closed` | pending | M5 |
| 4 | `dedup_response_hmac_keyed` | `crates/cas/tests/dedup.rs` | M1 |
| 4 | `dedup_answer_never_contains_raw_hashes_or_file_info` | `crates/cas/tests/dedup.rs` | M1 |
| 4 | `per_tenant_dedup_isolates_tenants` | `crates/cas/tests/dedup.rs` | M1 |
| 4 | `per_tenant_multi_xorb_answer_hides_foreign_xorbs` | `crates/cas/tests/dedup.rs` | M1 |
| 4 | `per_tenant_existence_checks_hide_other_tenants` | `crates/cas/tests/reconstruction.rs` | M1 |
| 4 | `per_tenant_scopes_of_one_tenant_share_dedup` | `crates/cas/tests/dedup.rs` | M1 |
| 5 | the S6 rule tests of `verify_shard` | `crates/protocol/tests/reference_files.rs` | M0 |
| 5 | `shard_with_missing_xorb_rejected` | `crates/cas/tests/shards.rs` | M1 |
| 5 | `shard_verify_work_capped` | `crates/cas/tests/shards.rs` | M1 |
| 5 | `verification_errors_reveal_no_server_derived_values` | `crates/protocol/tests/verify_errors.rs` | M1 |
| 5 | `foreign_writer_cannot_forge_shard_under_global` | `crates/cas/tests/shards.rs` | M1 |
| 6 | `lfs_download_action_has_no_token` | pending (M2, `crates/hub/tests/lfs.rs`) | M2 |
| 6 | `responses_never_echo_credential` | `crates/cas/tests/routes_misc.rs` | M1 |
| 6 | `responses_never_echo_credential` | `crates/hub/tests/security.rs` | M2 (Hub routes) |
| 6 | `credentials_never_logged` | `crates/openxet/tests/serve.rs` | M1 |
| 6 | `credentials_never_logged` | pending (M2, `crates/openxet/tests/hub_serve.rs`) | M2 (Hub, Git and LFS routes) |
| 6 | `test_fixture_redaction` | `compat/tests/test_fixture_redaction.py` | M0 |
| 7 | `acknowledged_xorb_write_is_visible` | `crates/cas/tests/xorbs.rs` | M1 |
| 7 | `acknowledged_shard_write_is_visible` | `crates/cas/tests/shards.rs` | M1 |
| 7 | `acked_write_survives_sigkill` | `crates/openxet/tests/serve.rs` | M1 |
| 7 | `acknowledged_bucket_key_write_is_visible` | pending | M5 |
| 8 | `v2_want_unreachable_refused` | `crates/git/tests/reachability.rs` | M2 |
| 8 | `hub_read_by_sha_obeys_r22` | pending (M2, `crates/git/tests/hub_objects.rs`) | M2 |
| 8 | `writer_cannot_make_unreachable_reachable` | pending (M2, `crates/git/tests/thin_bases.rs`) | M2 (pushes) |
| 8 | `writer_cannot_make_unreachable_reachable` | pending (M2, `crates/git/tests/hub_objects.rs`) | M2 (Hub ref writes and commits) |
| 8 | `hub_ref_write_fenced` | pending (M2, `crates/git/tests/hub_objects.rs`) | M2 |
| 8 | `ref_log_in_cas_transaction` | pending (M2, `crates/git/tests/receive_pack.rs`) | M2 (pushes) |
| 8 | `ref_log_in_cas_transaction` | pending (M2, `crates/git/tests/hub_objects.rs`) | M2 (Hub ref writes and commits) |
| 8 | `ref_log_in_cas_transaction` | pending (M2, `crates/git/tests/repack.rs`) | M2 (bucket ref log derivation and retention) |

Notes on what some tests cover:

- `no_unauthenticated_reads` requests every CAS route without a credential,
  then every data URL the CAS emits without its exact range. Its M2 half
  requests every Hub, file gateway and LFS URL openxet emits without a
  credential.
- `anonymous_reads_only_with_rule` checks that a request with no
  credential reads a repository only when an `[[auth.rules]]` entry
  grants `anonymous` read on it (ruling M2-R3).
- `credential_scoped_to_repo` sends another scope's read and write
  credentials to every CAS route, aimed at the first scope's data.
  `unreadable_repo_matches_unknown_repo_on_every_route` is its Hub half:
  every Hub route, called with a credential that can't read the
  repository, answers exactly the unknown-repository response of ruling
  M2-R6.
- `bucket_add_file_hash_must_be_readable` and
  `bucket_copy_file_requires_source_read` send `batch` calls naming a hash
  registered only to another scope, one the caller has no read credential
  for, and, under `per-tenant`, one another tenant uploaded. Each must
  answer exactly as an unknown hash does.
- `acknowledged_bucket_key_write_is_visible` acknowledges key writes,
  kills the server, deletes the database, runs `reindex` from the
  object-store bucket alone, and expects every acknowledged key with its
  last writer. It also stalls a writer between allocating its `seq` and
  creating its record until a sweeper aborts that `seq`, and expects the
  write to fail and to stay absent after `reindex`.
- `wrong_type_without_read_access_matches_unknown_repo` reads a
  repository through a typed route whose label it lacks, with a
  credential that can't read it, and compares status, headers (apart
  from per-request ids and dates) and body with an unknown repository's.
- `create_collision_authorized_first` creates a taken name in a
  namespace the caller may not create in and expects the same `403` as
  for a free name; with create but no read access, the `409` carries no
  labels.
- `per_tenant_existence_checks_hide_other_tenants` checks that, to another
  tenant, a `per-tenant` server holding a tenant's data answers every CAS
  route exactly as an empty server would.
- `per_tenant_scopes_of_one_tenant_share_dedup` checks the other side of
  isolation by tenant (ruling M1-R38): two scopes of one tenant share dedup
  and shard verification while files stay per scope, and a second tenant
  still sees none of it.
- `credentials_never_logged` runs a server at `trace` level through
  uploads, downloads and every kind of refusal, then scans its whole log
  for each credential, signature and key. The compatibility suite repeats
  the scan for every server it starts (`assert_no_credential_in_log`
  below).

## Supporting tests

These tests keep the requirements' tests complete, or check a part of a
requirement that the named tests don't.

| Req | Test | File | What it adds |
| --- | --- | --- | --- |
| 1, 2, 6 | `every_served_route_is_in_routes_or_the_allowlist` | `crates/cas/tests/routes_misc.rs` | Every route the CAS serves is in the route list the credential, scoping and echo matrices walk, or on the explicit allowlist: `/healthz`, `/readyz` and the signed transfer route. |
| 1, 2, 6 | `router_serves_exactly_its_endpoints` | `crates/cas/tests/routes_misc.rs` | The route list matches the methods the router really answers. |
| 1 | `transfer_requires_signature` | `crates/cas/tests/reconstruction.rs` | A transfer URL's signature binds one xorb, one range and an expiry. |
| 1 | `probes_need_no_credential_and_name_no_data` | `crates/cas/tests/routes_misc.rs` | The probes, the only unauthenticated routes, name nothing stored. |
| 4 | `head_under_per_tenant_hides_other_tenants_xorbs` | `crates/cas/tests/xorbs.rs` | `HEAD` of a xorb under `per-tenant`. |
| 4 | `per_tenant_shard_cannot_reference_foreign_xorb` | `crates/cas/tests/shards.rs` | Shard verification's xorb lookups under `per-tenant`. |
| 4 | `switching_global_to_per_tenant_keeps_reads_and_dedup` | `crates/cas/tests/reconstruction.rs` | After a switch from `global`, a tenant keeps its reads and dedup over xorbs another tenant uploaded, and a third tenant stays isolated (ruling M1-R39). |
| 4 | `jwt_rejects_empty_or_malformed_tenant` | `crates/auth/tests/auth.rs` | A token's `tenant`, which decides isolation, passes the scope's checks and doesn't start with the reserved `scope:`, or the token is refused. |
| 4 | `a_scope_named_like_a_tenant_stays_out_of_it` | `crates/cas/tests/dedup.rs` | A credential naming no tenant is in tenant `scope:{scope}`, so a scope named like a tenant never sees that tenant's xorbs, nor the other way round (ruling M1-R43). |
| 7 | `index_scope_tables_rebuild_from_the_bucket` | `crates/cas/tests/bucket_truth.rs` | Every acknowledged registration and ownership is in the bucket: the index's scope tables rebuild from it alone (ruling M1-R37). |
| 6 | `auth_errors_never_contain_the_credential` | `crates/auth/tests/auth.rs` | Credential errors never carry the credential. |
| 6 | `signed_urls_are_never_logged` | `crates/cas/tests/reconstruction.rs` | A transfer URL's signature never reaches the log. |
| 6 | `assert_no_credential_in_log` | `compat/conftest.py` | The log of every server the compatibility suite starts, in the `agent` and `storage_only` legs, is scanned for the credentials its clients sent. |
