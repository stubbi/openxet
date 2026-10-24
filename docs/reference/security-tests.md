# Security tests

Each security requirement in design §5.9 is a release blocker with a named
test. This page maps each requirement to its tests and the file they live
in, so a reviewer can check that a requirement is enforced and see what
runs when one of its tests fails.

A test is planned for the milestone that builds the surface it guards:
M1 for the CAS, M2 for the Hub and Git, M5 for caches and the mirror.
Pending rows name tests that are not written yet; their surface does not
exist yet either.

`scripts/check-security-tests-map.py` checks this page in CI against the
tests that actually run:
- every mapped Rust test is listed by `cargo nextest list` exactly once,
  from the file the page names, and is not ignored;
- every mapped compatibility test is collected by pytest from its file;
- every test name in design §5.9 is in the table below;
- every M0 or M1 row names a file; only M2 and later rows are pending.

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
| 2 | `credential_scoped_to_repo` | `crates/cas/tests/reconstruction.rs` | M1 (CAS routes) |
| 2 | `credential_scoped_to_repo`, Hub routes | pending | M2 |
| 2 | `repo_create_authorized_per_namespace` | pending | M2 |
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
| 6 | `lfs_download_action_has_no_token` | pending | M2 |
| 6 | `responses_never_echo_credential` | `crates/cas/tests/routes_misc.rs` | M1 |
| 6 | `credentials_never_logged` | `crates/openxet/tests/serve.rs` | M1 |
| 6 | `test_fixture_redaction` | `compat/tests/test_fixture_redaction.py` | M0 |
| 7 | `acknowledged_xorb_write_is_visible` | `crates/cas/tests/xorbs.rs` | M1 |
| 7 | `acknowledged_shard_write_is_visible` | `crates/cas/tests/shards.rs` | M1 |
| 7 | `acked_write_survives_sigkill` | `crates/openxet/tests/serve.rs` | M1 |
| 8 | `v2_want_unreachable_refused` | pending | M2 |
| 8 | `hub_read_by_sha_obeys_r22` | pending | M2 |
| 8 | `writer_cannot_make_unreachable_reachable` | pending | M2 |
| 8 | `hub_ref_write_fenced` | pending | M2 |
| 8 | `ref_log_in_cas_transaction` | pending | M2 |

Notes on what some tests cover:

- `no_unauthenticated_reads` requests every CAS route without a credential,
  then every data URL the CAS emits without its exact range. In M2 it
  extends to the HTTP read gateway and LFS `href`s.
- `credential_scoped_to_repo` sends another scope's read and write
  credentials to every CAS route, aimed at the first scope's data.
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
| 4 | `jwt_rejects_empty_or_malformed_tenant` | `crates/auth/tests/auth.rs` | A token's `tenant`, which decides isolation, passes the scope's checks or the token is refused. |
| 7 | `index_scope_tables_rebuild_from_the_bucket` | `crates/cas/tests/bucket_truth.rs` | Every acknowledged registration and ownership is in the bucket: the index's scope tables rebuild from it alone (ruling M1-R37). |
| 6 | `auth_errors_never_contain_the_credential` | `crates/auth/tests/auth.rs` | Credential errors never carry the credential. |
| 6 | `signed_urls_are_never_logged` | `crates/cas/tests/reconstruction.rs` | A transfer URL's signature never reaches the log. |
| 6 | `assert_no_credential_in_log` | `compat/conftest.py` | The log of every server the compatibility suite starts, in the `agent` and `storage_only` legs, is scanned for the credentials its clients sent. |
