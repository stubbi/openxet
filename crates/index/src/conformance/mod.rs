//! The backend-agnostic conformance suite: every behaviour of [`Index`],
//! [`Repos`] and [`GitStore`] that holds for any backend, written once
//! against a [`Fixture`] and run by each backend's test binary through
//! [`index_conformance!`](crate::index_conformance). SQLite runs it in
//! `crates/index/tests/sqlite_conformance.rs`, Postgres in
//! `crates/index/tests/postgres_conformance.rs` (ADR 0015). What only one
//! backend can show (its file locks, its query plans, its migrations of old
//! databases, its retries) stays in that backend's own tests.
//!
//! Each case is a `pub async fn <name><F: Fixture>(f: &F)` in [`cas`],
//! [`repos`] or [`git`] and runs as its own `#[test]`, named
//! `<group>::<name>`, on indexes from [`Fixture::fresh`], so a failure names
//! its case and the security test map can name it too.
//!
//! [`Index`]: crate::Index
//! [`Repos`]: crate::Repos
//! [`GitStore`]: crate::GitStore

pub mod cas;
pub mod git;
pub mod repos;

use std::future::Future;

use openxet_protocol::{MerkleHash, chunk_hash};

use crate::Index;

/// How the suite gets indexes of one backend.
///
/// The `cas` group needs only an [`Index`]; the `repos` and `git` groups
/// need a [`HubIndex`](crate::HubIndex), so a backend can pass the CAS
/// cases before it implements the rest.
#[async_trait::async_trait]
pub trait Fixture: Send + Sync {
    /// The backend's index.
    type Index: Index;

    /// An empty, migrated index of its own: no other case's rows.
    async fn fresh(&self) -> Self::Index;

    /// Another handle on `of`'s database, with its own connections: a
    /// second node serving the same index.
    async fn second_handle(&self, of: &Self::Index) -> Self::Index;
}

/// Runs `case` to completion on a multi-threaded runtime, so concurrent
/// cases contend for real. Used by the tests
/// [`index_conformance!`](crate::index_conformance) expands to.
///
/// # Panics
///
/// If the runtime can't be built, or the case fails.
#[doc(hidden)]
#[expect(
    clippy::unwrap_used,
    reason = "a test helper: a runtime that can't start fails the test"
)]
pub fn block_on(case: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap()
        .block_on(case);
}

/// Set to `1` (as CI does) to make a test whose service isn't configured
/// fail instead of skipping, so no job passes by skipping.
pub const REQUIRE_SERVICES: &str = "OPENXET_REQUIRE_SERVICES";

/// The URL of the service a test needs, from the environment variable
/// `var`, or `None` after printing why the test is skipped. `example`
/// shows what to set it to.
///
/// # Panics
///
/// If `var` is unset or empty while [`REQUIRE_SERVICES`] is `1`.
#[must_use]
pub fn service_url(var: &str, example: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(url) if !url.is_empty() => Some(url),
        _ if std::env::var(REQUIRE_SERVICES).as_deref() == Ok("1") => panic!(
            "{var} is unset, and {REQUIRE_SERVICES}=1 requires the service: set it, for \
             example {var}={example}"
        ),
        _ => {
            eprintln!(
                "skipped: {var} is unset; set it to run this test, for example {var}={example}"
            );
            None
        }
    }
}

/// The cases `source` defines that `listed` leaves out: what
/// [`index_conformance!`](crate::index_conformance) checks, so a case
/// added to a group but not to the macro's list fails instead of never
/// running.
#[doc(hidden)]
#[must_use]
pub fn unlisted(group: &str, listed: &[&str]) -> Vec<String> {
    let source = match group {
        "cas" => include_str!("cas.rs"),
        "repos" => include_str!("repos.rs"),
        "git" => include_str!("git.rs"),
        other => return vec![format!("unknown group {other}")],
    };
    source
        .lines()
        .filter_map(|line| line.strip_prefix("pub async fn "))
        .filter_map(|rest| rest.split('<').next())
        .filter(|name| !listed.contains(name))
        .map(str::to_owned)
        .collect()
}

/// Expands to the conformance suite's tests, one `#[test]` per case, for
/// the [`Fixture`] the function `$fixture` (in the invoking module, taking
/// no arguments) returns, or for none if it returns `None`: a backend whose
/// service isn't configured skips (see [`service_url`]). The groups to run
/// follow the colon: `cas` ([`Index`](crate::Index)), `repos`
/// ([`Repos`](crate::Repos)) and `git` ([`GitStore`](crate::GitStore)).
///
/// ```ignore
/// fn sqlite() -> Option<SqliteFixture> { /* ... */ }
/// openxet_index::index_conformance!(sqlite: cas, repos, git);
/// ```
#[macro_export]
macro_rules! index_conformance {
    ($fixture:ident: $($group:ident),+ $(,)?) => {
        $( $crate::index_conformance!(@$group $fixture); )+
    };
    (@cas $fixture:ident) => {
        $crate::index_conformance!(@cases cas $fixture [
            probe_answers_on_an_open_index,
            put_xorb_is_idempotent,
            xorb_visible_per_tenant_only_when_per_tenant,
            register_shard_is_idempotent_per_scope,
            file_only_readable_in_registered_scope,
            files_answer_each_scopes_own_registrations_in_one_call,
            files_handles_more_files_than_one_query_binds,
            term_count_sums_the_scopes_files_once_each,
            same_file_two_scopes_keeps_each_scopes_terms,
            register_shard_rejects_invisible_xorb_without_revealing_existence,
            dedup_xorb_prefers_newest_and_respects_visibility,
            chunk_tables_returns_only_present_visible,
            chunk_tables_and_layouts_handle_more_xorbs_than_one_query_binds,
            chunk_layouts_answer_as_chunk_tables_do_without_hashes,
            registration_records_where_each_term_is_stored,
            a_term_past_its_xorbs_chunks_is_refused_at_registration,
            register_shard_is_atomic,
            reopen_keeps_everything,
            put_xorb_rejects_malformed_table,
            dedup_shard_xorbs_lists_the_shards_xorbs_in_term_order,
            dedup_shard_xorbs_none_without_a_shard,
            dedup_shard_xorbs_prefers_the_newest_shard,
            dedup_shard_xorbs_respects_tenant_and_visibility,
            registration_seq_counts_up_across_handles,
            global_registration_stays_readable_under_per_tenant,
            scopes_of_one_tenant_share_xorbs_under_per_tenant,
            lowest_numbered_registration_of_a_file_wins,
            ties_on_the_number_break_on_shard,
            distinct_concurrent_writers_never_fail,
            concurrent_put_xorb_two_pools,
            concurrent_register_same_shard_two_pools,
        ]);
    };
    (@repos $fixture:ident) => {
        $crate::index_conformance!(@cases repos $fixture [
            create_then_lookup_by_name_and_id,
            name_taken_case_insensitively,
            list_filters_by_label_and_paginates,
            set_labels_replaces,
            set_labels_compares_and_swaps,
            file_by_sha256_is_per_scope,
            xet_files_by_sha256_answers_like_file_by_sha256,
            file_summary_is_per_scope,
            purge_removes_every_scope_row,
            is_a_hub_index,
        ]);
    };
    (@git $fixture:ident) => {
        $crate::index_conformance!(@cases git $fixture [
            snapshot_reads_refs_and_committed_packs_only,
            commit_applies_all_updates_or_none,
            commit_requires_pending_pack,
            commit_fenced_by_full_repack_gen,
            commit_writes_one_ref_log_row_per_update,
            mark_pack_is_guarded,
            lease_epoch_increases_and_expires,
            publish_repack_rejects_stale_epoch_and_advanced_ref_log,
            unplaced_rows_persist_and_clear_by_coverage,
            delete_repo_hands_packs_to_gc,
            concurrent_hub_commits_two_pools_one_winner,
            packs_list_every_state_and_delete_is_guarded,
            ref_log_head_and_prune,
            reindex_restore_and_discard,
            pack_deletion_claims_exclude_new_rows,
            inserted_at_is_the_database_clock,
            renew_lease_keeps_epoch_until_lost,
        ]);
    };
    (@cases $group:ident $fixture:ident [$($case:ident),+ $(,)?]) => {
        mod $group {
            $(
                #[test]
                fn $case() {
                    $crate::conformance::block_on(async {
                        let Some(fixture) = super::$fixture() else {
                            return;
                        };
                        $crate::conformance::$group::$case(&fixture).await;
                    });
                }
            )+

            #[test]
            fn every_case_is_listed() {
                let unlisted = $crate::conformance::unlisted(
                    stringify!($group),
                    &[$(stringify!($case)),+],
                );
                assert!(unlisted.is_empty(), "cases never run: {unlisted:?}");
            }
        }
    };
}

/// A distinct hash per label.
fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

/// `r`'s value, or a panic naming the error.
fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("index call failed: {e}"),
    }
}
