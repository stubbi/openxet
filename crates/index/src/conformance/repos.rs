//! [`Repos`]' cases: case-insensitive names (ruling M2-R2), label listings
//! (M2-R8, ADR 0010), per-scope SHA-256 lookups (design §5.4) and purge
//! (M2-R13).

#![allow(
    clippy::missing_panics_doc,
    reason = "every case panics when its backend fails it"
)]

use std::sync::Arc;

use openxet_protocol::MerkleHash;

use super::{Fixture, h, ok};
use crate::{
    ChunkTable, FileRecord, FileSummary, HubIndex, Index, IndexError, NewFile, Page, Registrant,
    Repo, RepoFilter, RepoId, Repos, Term, Visibility, XetFile,
};

fn repo(namespace: &str, name: &str, labels: &[&str]) -> Repo {
    Repo {
        id: RepoId::generate(),
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        private: false,
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        created_at: 1_761_400_000,
        full_repack_gen: 0,
    }
}

/// A table of `n` chunks, each 1000 bytes uncompressed and 600 stored.
fn table(label: &str, n: u32) -> ChunkTable {
    let hashes: Vec<(MerkleHash, u64)> = (0..n)
        .map(|i| (h(&format!("{label}/chunk{i}")), 1000))
        .collect();
    ChunkTable {
        hashes: hashes.into(),
        offsets: (0..=n).map(|i| i * 600).collect::<Vec<_>>().into(),
    }
}

/// Stores xorb `label` of `n` chunks for `tenant`, every chunk eligible.
async fn put(index: &dyn Index, label: &str, n: u32, tenant: &str) -> (MerkleHash, ChunkTable) {
    let xorb = h(&format!("xorb/{label}"));
    let t = table(label, n);
    let eligible: Vec<MerkleHash> = t.hashes.iter().map(|(c, _)| *c).collect();
    ok(index
        .put_xorb(&xorb, u64::from(n) * 600, &t, &eligible, tenant)
        .await);
    (xorb, t)
}

/// A file over chunks `start..end` of `xorb`, as a real shard names it:
/// its first chunk is the first term's first chunk.
fn file(label: &str, xorb: MerkleHash, t: &ChunkTable, start: u32, end: u32) -> NewFile {
    NewFile {
        record: FileRecord {
            hash: h(&format!("file/{label}")),
            size: u64::from(end - start) * 1000,
            terms: vec![Term {
                xorb,
                chunk_start: start,
                chunk_end: end,
                unpacked_bytes: (end - start) * 1000,
            }],
        },
        sha256: Some(sha(label)),
        first_chunk: Some(t.hashes[start as usize].0),
    }
}

/// A distinct SHA-256 per label; the index never checks it (design §5.4).
fn sha(label: &str) -> [u8; 32] {
    let mut out = [0; 32];
    out.copy_from_slice(h(&format!("sha256/{label}")).as_bytes());
    out
}

async fn register(index: &dyn Index, shard: &str, files: &[NewFile], scope: &str, seq: u64) {
    ok(index
        .register_shard(
            &h(shard),
            files,
            Registrant {
                scope,
                tenant: scope,
                seq,
            },
            Visibility::Global,
        )
        .await);
}

/// Every chunk of `t`'s dedup answer, in order.
async fn dedup_answers(index: &dyn Index, t: &ChunkTable) -> Vec<Option<MerkleHash>> {
    let mut out = Vec::new();
    for (chunk, _) in t.hashes.iter() {
        out.push(ok(index.dedup_xorb(chunk, "t", Visibility::Global).await));
    }
    out
}

pub async fn create_then_lookup_by_name_and_id<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let mut r = repo("Org", "Model", &["model", "eval"]);
    r.private = true;
    r.full_repack_gen = 3;
    ok(index.create_repo(&r).await);

    assert_eq!(
        ok(index.repo_by_name("Org", "Model").await),
        Some(r.clone())
    );
    assert_eq!(
        ok(index.repo_by_name("org", "MODEL").await),
        Some(r.clone())
    );
    assert_eq!(ok(index.repo_by_id(r.id).await), Some(r.clone()));
    assert_eq!(ok(index.repo_by_name("org", "other").await), None);
    assert_eq!(ok(index.repo_by_id(RepoId::generate()).await), None);

    // Another handle reads it once the first is gone: the row is durable,
    // not cached.
    let other = f.second_handle(&index).await;
    drop(index);
    assert_eq!(ok(other.repo_by_id(r.id).await), Some(r));
}

pub async fn name_taken_case_insensitively<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let first = repo("org", "model", &["model"]);
    ok(index.create_repo(&first).await);

    // Names are unique across labels too (ADR 0010 item 5).
    let clash = repo("Org", "Model", &["dataset"]);
    match index.create_repo(&clash).await {
        Err(IndexError::NameTaken { name }) => assert_eq!(name, "Org/Model"),
        other => panic!("expected NameTaken, got {other:?}"),
    }
    assert_eq!(ok(index.repo_by_id(clash.id).await), None);
    assert_eq!(ok(index.repo_by_name("ORG", "MODEL").await), Some(first));

    // The key joins namespace and name, so neither half collides alone.
    ok(index.create_repo(&repo("org", "model2", &[])).await);
    ok(index.create_repo(&repo("org2", "model", &[])).await);
}

pub async fn list_filters_by_label_and_paginates<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    // 150 models alternating between two namespaces, and 15 datasets
    // alternating every ten.
    let mut models = Vec::new();
    for i in 0..150 {
        let ns = if i % 2 == 0 { "alpha" } else { "Beta" };
        let r = repo(ns, &format!("m{i:04}"), &["model"]);
        ok(index.create_repo(&r).await);
        models.push(r);
    }
    for i in (0..150).step_by(10) {
        let ns = if (i / 10) % 2 == 0 { "alpha" } else { "Beta" };
        ok(index
            .create_repo(&repo(ns, &format!("d{i:04}"), &["dataset"]))
            .await);
    }
    let key = |r: &Repo| format!("{}/{}", r.namespace, r.name).to_ascii_lowercase();
    models.sort_by_key(key);

    let filter = RepoFilter {
        label: Some("model".to_owned()),
        namespace: None,
    };
    let first = ok(index.list_repos(filter.clone(), None, 100).await);
    assert_eq!(first.items.len(), 100);
    let cursor = first.next.clone().unwrap_or_else(|| panic!("a next page"));
    let second = ok(index.list_repos(filter.clone(), Some(&cursor), 100).await);
    assert_eq!(second.items.len(), 50);
    assert_eq!(second.next, None);
    let listed: Vec<Repo> = first.items.into_iter().chain(second.items).collect();
    assert_eq!(listed, models);

    // Stable: the same cursor answers the same page.
    let again = ok(index.list_repos(filter, Some(&cursor), 100).await);
    assert_eq!(again.items, listed[100..]);

    // Namespace filter, ignoring case, combined with the label.
    let beta = ok(index
        .list_repos(
            RepoFilter {
                label: Some("dataset".to_owned()),
                namespace: Some("beta".to_owned()),
            },
            None,
            100,
        )
        .await);
    let names: Vec<&str> = beta.items.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "d0010", "d0030", "d0050", "d0070", "d0090", "d0110", "d0130"
        ]
    );
    assert!(beta.items.iter().all(|r| r.namespace == "Beta"));

    // No filter lists everything; an exact page leaves no cursor.
    let all = ok(index.list_repos(RepoFilter::default(), None, 165).await);
    assert_eq!(all.items.len(), 165);
    assert_eq!(all.next, None);
    let empty = ok(index
        .list_repos(
            RepoFilter {
                label: Some("space".to_owned()),
                namespace: None,
            },
            None,
            10,
        )
        .await);
    assert_eq!(
        empty,
        Page {
            items: vec![],
            next: None
        }
    );
}

pub async fn set_labels_replaces<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let r = repo("org", "thing", &["model"]);
    ok(index.create_repo(&r).await);

    let labels = vec!["dataset".to_owned(), "eval".to_owned()];
    ok(index.set_labels(r.id, &r.labels, &labels).await);
    let got = ok(index.repo_by_id(r.id).await).unwrap_or_else(|| panic!("repo"));
    assert_eq!(got.labels, labels);
    let as_model = ok(index
        .list_repos(
            RepoFilter {
                label: Some("model".to_owned()),
                namespace: None,
            },
            None,
            10,
        )
        .await);
    assert!(as_model.items.is_empty());

    ok(index.set_labels(r.id, &labels, &[]).await);
    let got = ok(index.repo_by_id(r.id).await).unwrap_or_else(|| panic!("repo"));
    assert!(got.labels.is_empty());

    let missing = RepoId::generate();
    match index.set_labels(missing, &[], &labels).await {
        Err(IndexError::UnknownRepo { repo }) => assert_eq!(repo, missing),
        other => panic!("expected UnknownRepo, got {other:?}"),
    }
}

/// Two writers that read the same labels can't both change them: the
/// second one's expected list is stale, and it changes nothing (ruling
/// M2-R12).
pub async fn set_labels_compares_and_swaps<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let r = repo("org", "thing", &["model"]);
    ok(index.create_repo(&r).await);

    let first = vec!["model".to_owned(), "dataset".to_owned()];
    let second = vec!["model".to_owned(), "space".to_owned()];
    ok(index.set_labels(r.id, &r.labels, &first).await);
    match index.set_labels(r.id, &r.labels, &second).await {
        Err(IndexError::LabelsChanged { repo }) => assert_eq!(repo, r.id),
        other => panic!("expected LabelsChanged, got {other:?}"),
    }
    let got = ok(index.repo_by_id(r.id).await).unwrap_or_else(|| panic!("repo"));
    assert_eq!(got.labels, first);
    // Order is part of the list.
    let reordered = vec!["dataset".to_owned(), "model".to_owned()];
    assert!(matches!(
        index.set_labels(r.id, &reordered, &second).await,
        Err(IndexError::LabelsChanged { .. })
    ));
}

pub async fn file_by_sha256_is_per_scope<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let (xorb, t) = put(&index, "x", 4, "repo:a").await;
    let f = file("same", xorb, &t, 0, 4);
    let sha = f.sha256.unwrap_or_else(|| panic!("sha"));
    register(&index, "shard", std::slice::from_ref(&f), "repo:a", 1).await;

    assert_eq!(
        ok(index.file_by_sha256("repo:a", &sha).await),
        Some(f.record.clone())
    );
    // Client-asserted, so never trusted outside the scope that registered
    // it: repo B must upload the file itself (Review Focus 1).
    assert_eq!(ok(index.file_by_sha256("repo:b", &sha).await), None);
    assert_eq!(ok(index.file_by_sha256("repo:a", &[9; 32]).await), None);

    // Of two files claiming one SHA-256 in a scope, the earliest
    // registration answers, however the later one sorts.
    let mut liar = file("liar", xorb, &t, 0, 2);
    liar.sha256 = Some(sha);
    register(&index, "shard2", std::slice::from_ref(&liar), "repo:a", 2).await;
    assert_eq!(
        ok(index.file_by_sha256("repo:a", &sha).await),
        Some(f.record)
    );
}

/// The batch form answers each SHA-256 as `file_by_sha256` does, across
/// query batches.
pub async fn xet_files_by_sha256_answers_like_file_by_sha256<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let (xorb, t) = put(&index, "x", 4, "repo:a").await;
    let files: Vec<NewFile> = (0..1_200)
        .map(|i| file(&format!("f{i}"), xorb, &t, i % 3, 4))
        .collect();
    register(&index, "shard", &files, "repo:a", 1).await;
    let first = &files[0];
    let sha = first.sha256.unwrap_or_else(|| panic!("sha"));
    let mut liar = file("liar", xorb, &t, 0, 1);
    liar.sha256 = Some(sha);
    register(&index, "shard2", std::slice::from_ref(&liar), "repo:a", 2).await;

    let mut asked: Vec<[u8; 32]> = files.iter().filter_map(|f| f.sha256).collect();
    asked.push([9; 32]);
    asked.push(sha);
    let found = ok(index.xet_files_by_sha256("repo:a", &asked).await);
    assert_eq!(found.len(), files.len(), "{:?}", found.get(&[9; 32]));
    for f in &files {
        let sha = f.sha256.unwrap_or_else(|| panic!("sha"));
        let single =
            ok(index.file_by_sha256("repo:a", &sha).await).unwrap_or_else(|| panic!("registered"));
        assert_eq!(
            found.get(&sha),
            Some(&XetFile {
                hash: single.hash,
                size: single.size,
            })
        );
    }
    assert_eq!(
        found[&sha].hash, first.record.hash,
        "the earliest registration"
    );
    assert!(ok(index.xet_files_by_sha256("repo:b", &asked).await).is_empty());
    assert!(ok(index.xet_files_by_sha256("repo:a", &[]).await).is_empty());
}

pub async fn file_summary_is_per_scope<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let (xorb, t) = put(&index, "x", 4, "repo:a").await;
    let f = file("same", xorb, &t, 0, 4);
    let sha = f.sha256.unwrap_or_else(|| panic!("sha"));
    register(&index, "shard", std::slice::from_ref(&f), "repo:a", 1).await;
    let mut unnamed = file("unnamed", xorb, &t, 0, 2);
    unnamed.sha256 = None;
    register(
        &index,
        "shard2",
        std::slice::from_ref(&unnamed),
        "repo:a",
        2,
    )
    .await;

    assert_eq!(
        ok(index.file_summary("repo:a", &f.record.hash).await),
        Some(FileSummary {
            size: f.record.size,
            sha256: Some(sha),
        })
    );
    assert_eq!(ok(index.file_summary("repo:b", &f.record.hash).await), None);
    assert_eq!(
        ok(index.file_summary("repo:a", &unnamed.record.hash).await),
        Some(FileSummary {
            size: unnamed.record.size,
            sha256: None,
        })
    );
}

pub async fn purge_removes_every_scope_row<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let (xorb, t) = put(&index, "secret", 6, "t").await;
    let (other_xorb, other_t) = put(&index, "other", 3, "t").await;
    let secret = file("secret", xorb, &t, 1, 4);
    let other = file("other", other_xorb, &other_t, 0, 3);
    register(&index, "s1", std::slice::from_ref(&secret), "repo:a", 1).await;
    register(&index, "s2", &[secret.clone(), other.clone()], "repo:b", 2).await;
    let dedup_before = (
        dedup_answers(&index, &t).await,
        dedup_answers(&index, &other_t).await,
    );
    assert!(dedup_before.0.iter().all(|x| *x == Some(xorb)));
    // What `openxet purge --dry-run` reports, without changing anything.
    assert_eq!(ok(index.file_scope_count(&secret.record.hash).await), 2);

    assert_eq!(ok(index.purge_file(&secret.record.hash).await), 2);
    assert_eq!(ok(index.file_scope_count(&secret.record.hash).await), 0);
    assert_eq!(ok(index.file_scope_count(&other.record.hash).await), 1);

    for scope in ["repo:a", "repo:b"] {
        assert_eq!(ok(index.file(&secret.record.hash, scope).await), None);
    }
    assert_eq!(
        ok(index.file(&other.record.hash, "repo:b").await),
        Some(other.record)
    );
    // Dedup rows describe xorbs, not files, and a rebuild from the bucket
    // restores them, so purge leaves them as they were.
    assert_eq!(
        (
            dedup_answers(&index, &t).await,
            dedup_answers(&index, &other_t).await
        ),
        dedup_before
    );

    // Repeating it removes nothing more.
    assert_eq!(ok(index.purge_file(&secret.record.hash).await), 0);
}

/// The Hub holds the index as one trait object.
pub async fn is_a_hub_index<F: Fixture>(f: &F) {
    let index = f.fresh().await;
    let hub: Arc<dyn HubIndex> = Arc::new(index);
    let r = repo("org", "model", &["model"]);
    ok(hub.create_repo(&r).await);
    assert_eq!(ok(hub.snapshot(r.id).await).full_repack_gen, 0);
    assert_eq!(ok(hub.file(&h("absent"), "repo:x").await), None);
}
