//! The bucket's repository records: metadata, packs, multi-pack indexes and
//! the ref log (design §5.3, ADR 0004 items 3 and 4), and purge records
//! (ruling M2-R13).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_core::stream::BoxStream;
use openxet_protocol::{MerkleHash, chunk_hash, hash_to_hex};
use openxet_storage::{
    Bucket, Key, KeyKind, LocalBucket, MidxExt, PackExt, PackName, PurgeRecord, RefLogEntry,
    RefLogRecord, RepoIdHex, RepoMeta, STREAM_BLOCK_BYTES, StorageError,
};

const ID: &str = "0192f3a4b5c67d8e9fa0b1c2d3e4f5a6";
const OTHER: &str = "0192f3a4b5c67d8e9fa0b1c2d3e4f5a7";
const PACK: &str = "pack-1f2e3d4c5b6a79880716253443526170819a0b1c";

fn repo(id: &str) -> RepoIdHex {
    RepoIdHex::parse(id).unwrap()
}

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn meta(name: &str, labels: &[&str]) -> RepoMeta {
    RepoMeta {
        id: repo(ID),
        namespace: "org".to_owned(),
        name: name.to_owned(),
        private: true,
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        created_at: 1_761_400_000,
        deleted_at: None,
    }
}

fn pack(repo_id: &str, ext: PackExt) -> Key {
    Key::Pack {
        repo: repo(repo_id),
        name: PackName::parse(PACK).unwrap(),
        ext,
    }
}

/// A stream of `bytes` in blocks of `block` bytes.
fn blocks_of(bytes: &[u8], block: usize) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    struct Blocks(std::vec::IntoIter<Bytes>);
    impl futures_core::Stream for Blocks {
        type Item = Result<Bytes, std::io::Error>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.next().map(Ok))
        }
    }
    let chunks: Vec<Bytes> = bytes.chunks(block).map(Bytes::copy_from_slice).collect();
    Box::pin(Blocks(chunks.into_iter()))
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from(i % 251).unwrap()).collect()
}

#[test]
fn keys_display_as_documented() {
    let id = repo(ID);
    let file = h("purged");
    let cases = [
        (Key::RepoMeta { repo: id }, format!("repos/{ID}/meta")),
        (
            pack(ID, PackExt::Pack),
            format!("repos/{ID}/packs/{PACK}.pack"),
        ),
        (
            pack(ID, PackExt::Idx),
            format!("repos/{ID}/packs/{PACK}.idx"),
        ),
        (
            pack(ID, PackExt::Rev),
            format!("repos/{ID}/packs/{PACK}.rev"),
        ),
        (
            pack(ID, PackExt::Bitmap),
            format!("repos/{ID}/packs/{PACK}.bitmap"),
        ),
        (
            pack(ID, PackExt::Mtimes),
            format!("repos/{ID}/packs/{PACK}.mtimes"),
        ),
        (
            Key::Midx {
                repo: id,
                generation: 7,
                ext: MidxExt::MultiPackIndex,
            },
            format!("repos/{ID}/midx/7.multi-pack-index"),
        ),
        (
            Key::Midx {
                repo: id,
                generation: 7,
                ext: MidxExt::Bitmap,
            },
            format!("repos/{ID}/midx/7.bitmap"),
        ),
        (
            Key::Midx {
                repo: id,
                generation: 7,
                ext: MidxExt::Rev,
            },
            format!("repos/{ID}/midx/7.rev"),
        ),
        (
            Key::RefLog { repo: id, seq: 42 },
            format!("repos/{ID}/reflog/00000000000000000042"),
        ),
        (Key::Purge(file), format!("purges/{}", hash_to_hex(&file))),
    ];
    for (key, shown) in cases {
        assert_eq!(key.to_string(), shown);
    }
}

#[test]
fn repo_ids_are_32_lowercase_hex_characters() {
    assert_eq!(repo(ID).to_string(), ID);
    for bad in [
        "",
        "0192f3a4b5c67d8e9fa0b1c2d3e4f5a",
        "0192f3a4b5c67d8e9fa0b1c2d3e4f5a60",
        "0192F3A4B5C67D8E9FA0B1C2D3E4F5A6",
        "+192f3a4b5c67d8e9fa0b1c2d3e4f5a6",
        "0192f3a4-b5c6-7d8e-9fa0-b1c2d3e4",
        "../../../../../../../../etc/pass",
    ] {
        let err = RepoIdHex::parse(bad).unwrap_err();
        assert!(matches!(err, StorageError::InvalidKey { .. }), "{bad:?}");
    }
}

#[tokio::test]
async fn repo_meta_replace_is_durable_and_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let first = meta("model", &["model"]);
    let key = first.key();
    assert_eq!(key, Key::RepoMeta { repo: repo(ID) });

    bucket
        .put_replace(key.clone(), first.to_bytes())
        .await
        .unwrap();
    let read = bucket.get(&key).await.unwrap().unwrap();
    assert_eq!(RepoMeta::from_bytes(&key, &read).unwrap(), first);

    // A rename rewrites the one record; a longer and a shorter body each
    // replace it whole.
    for next in [
        meta("a-much-longer-model-name", &["model", "dataset"]),
        meta("m", &[]),
    ] {
        bucket
            .put_replace(key.clone(), next.to_bytes())
            .await
            .unwrap();
        let read = bucket.get(&key).await.unwrap().unwrap();
        assert_eq!(RepoMeta::from_bytes(&key, &read).unwrap(), next);
    }
    assert!(files_in(&dir.path().join(".tmp")).is_empty());
}

/// Only a repository's metadata is ever overwritten: every other object is
/// written once, and replacing one would delete data outside GC
/// (non-negotiable 3).
#[tokio::test]
async fn put_replace_refuses_every_other_kind() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    for key in [
        Key::Xorb(h("x")),
        Key::Purge(h("p")),
        pack(ID, PackExt::Pack),
        Key::RefLog {
            repo: repo(ID),
            seq: 1,
        },
    ] {
        let err = bucket
            .put_replace(key.clone(), Bytes::from_static(b"x"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidKey { .. }),
            "{key}: {err:?}"
        );
        assert_eq!(bucket.size(&key).await.unwrap(), None, "{key}");
    }
}

/// A deletion tombstones the record: `deleted_at` round-trips, and a live
/// record leaves it out, so records written before it existed read as live.
#[test]
fn repo_meta_tombstone_round_trips_and_is_absent_while_live() {
    let live = meta("model", &["model"]);
    let key = live.key();
    let bytes = live.to_bytes();
    assert!(!std::str::from_utf8(&bytes).unwrap().contains("deleted_at"));
    let old = br#"{"v":1,"id":"0192f3a4b5c67d8e9fa0b1c2d3e4f5a6","namespace":"org","name":"model","private":true,"labels":["model"],"created_at":1761400000}"#;
    assert_eq!(RepoMeta::from_bytes(&key, old).unwrap(), live);

    let deleted = RepoMeta {
        deleted_at: Some(1_761_500_000),
        ..live
    };
    let bytes = deleted.to_bytes();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .ends_with(r#","deleted_at":1761500000}"#)
    );
    assert_eq!(RepoMeta::from_bytes(&key, &bytes).unwrap(), deleted);
}

#[test]
fn repo_meta_refuses_another_key_and_unknown_fields() {
    let record = meta("model", &["model"]);
    let elsewhere = Key::RepoMeta { repo: repo(OTHER) };
    assert!(matches!(
        RepoMeta::from_bytes(&elsewhere, &record.to_bytes()),
        Err(StorageError::CorruptRecord { newer: false, .. })
    ));
    let key = record.key();
    let extra = br#"{"v":1,"id":"0192f3a4b5c67d8e9fa0b1c2d3e4f5a6","namespace":"org","name":"m","private":false,"labels":[],"created_at":1,"owner":"x"}"#;
    assert!(matches!(
        RepoMeta::from_bytes(&key, extra),
        Err(StorageError::CorruptRecord { newer: false, .. })
    ));
    let newer = br#"{"v":2}"#;
    assert!(matches!(
        RepoMeta::from_bytes(&key, newer),
        Err(StorageError::CorruptRecord { newer: true, .. })
    ));
}

#[tokio::test]
async fn pack_round_trips_through_file() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(&dir.path().join("bucket")).unwrap();
    let body = payload(2 * STREAM_BLOCK_BYTES + 123);
    let key = pack(ID, PackExt::Pack);

    bucket
        .put_stream(key.clone(), blocks_of(&body, 64 * 1024), body.len() as u64)
        .await
        .unwrap();
    assert_eq!(bucket.size(&key).await.unwrap(), Some(body.len() as u64));

    let cache = dir.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let local = cache.join(format!("{PACK}.pack"));
    let copied = bucket.get_to_file(key.clone(), &local).await.unwrap();
    assert_eq!(copied, body.len() as u64);
    assert_eq!(std::fs::read(&local).unwrap(), body);
    assert_eq!(
        files_in(&cache),
        std::slice::from_ref(&local),
        "no temp file is left"
    );

    // Hydrating again replaces the file whole.
    std::fs::write(&local, b"stale").unwrap();
    bucket.get_to_file(key.clone(), &local).await.unwrap();
    assert_eq!(std::fs::read(&local).unwrap(), body);

    let missing = pack(OTHER, PackExt::Pack);
    let err = bucket
        .get_to_file(missing, &cache.join("missing.pack"))
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound { .. }), "{err:?}");
    assert_eq!(files_in(&cache), [local]);
}

#[tokio::test]
async fn put_stream_refuses_a_body_of_the_wrong_length() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let body = payload(1000);
    let key = pack(ID, PackExt::Idx);
    for claimed in [999, 1001] {
        let err = bucket
            .put_stream(key.clone(), blocks_of(&body, 100), claimed)
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageError::LengthMismatch { expected, .. } if expected == claimed),
            "{err:?}"
        );
        assert_eq!(bucket.size(&key).await.unwrap(), None);
    }
    assert!(files_in(&dir.path().join(".tmp")).is_empty());
}

/// Packs are written once: a second upload of the same name keeps the
/// first object, so its `Last-Modified` stays the publish time (ADR 0004
/// item 4), and one of another size is a conflict.
#[tokio::test]
async fn put_stream_keeps_the_first_pack() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let body = payload(500);
    let key = pack(ID, PackExt::Pack);
    bucket
        .put_stream(key.clone(), blocks_of(&body, 100), 500)
        .await
        .unwrap();
    bucket
        .put_stream(key.clone(), blocks_of(&body, 100), 500)
        .await
        .unwrap();
    let err = bucket
        .put_stream(key.clone(), blocks_of(&body[..400], 100), 400)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::SizeConflict { .. }), "{err:?}");
    assert_eq!(bucket.get(&key).await.unwrap().unwrap(), body);
}

/// A pack key can only hold a name git writes, so no backend ever builds a
/// path from anything else.
#[tokio::test]
async fn pack_names_are_checked_at_construction() {
    for name in [
        "",
        "../../../escape",
        "pack-1f2e3d4c5b6a79880716253443526170819a0b1",
        "pack-1F2E3D4C5B6A79880716253443526170819A0B1C",
        "pack-1f2e3d4c5b6a79880716253443526170819a0b1c.pack",
        "1f2e3d4c5b6a79880716253443526170819a0b1c",
    ] {
        let err = PackName::parse(name).unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidKey { .. }),
            "{name:?}: {err:?}"
        );
    }
    let sha256 = format!("pack-{}", "ab".repeat(32));
    assert_eq!(PackName::parse(&sha256).unwrap().as_str(), sha256);
    assert_eq!(PackName::parse(PACK).unwrap().to_string(), PACK);

    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = Key::Pack {
        repo: repo(ID),
        name: PackName::parse(&sha256).unwrap(),
        ext: PackExt::Pack,
    };
    bucket.put_stream(key, blocks_of(b"x", 1), 1).await.unwrap();
}

/// A repository's metadata is only ever written whole with `put_replace`:
/// `put` would keep an old record of the same size and answer `Ok`, a
/// silent no-op. And `put` never touches a create-only record.
#[tokio::test]
async fn put_and_create_refuse_keys_written_another_way() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let meta_key = Key::RepoMeta { repo: repo(ID) };
    let bytes = meta("model", &[]).to_bytes();

    let err = bucket.put(&meta_key, bytes.clone()).await.unwrap_err();
    assert!(matches!(err, StorageError::InvalidKey { .. }), "{err:?}");
    assert!(err.to_string().contains("put_replace"), "{err}");
    let err = bucket.create(&meta_key, bytes).await.unwrap_err();
    assert!(matches!(err, StorageError::InvalidKey { .. }), "{err:?}");
    assert!(err.to_string().contains("put_replace"), "{err}");
    assert_eq!(bucket.size(&meta_key).await.unwrap(), None);

    for key in [
        Key::RefLog {
            repo: repo(ID),
            seq: 1,
        },
        Key::Purge(h("purged")),
        Key::registration("scope", "tenant", h("shard")),
    ] {
        let err = bucket
            .put(&key, Bytes::from_static(b"r"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageError::InvalidKey { .. }),
            "{key}: {err:?}"
        );
        assert!(err.to_string().contains("create"), "{err}");
        assert_eq!(bucket.size(&key).await.unwrap(), None, "{key}");
    }
}

#[tokio::test]
async fn list_under_filters_kind() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let (id, other) = (repo(ID), repo(OTHER));
    let mut all = Vec::new();
    for r in [ID, OTHER] {
        for ext in [PackExt::Pack, PackExt::Idx, PackExt::Mtimes] {
            let key = pack(r, ext);
            bucket.put(&key, Bytes::from_static(b"p")).await.unwrap();
            all.push(key);
        }
    }
    let midx = Key::Midx {
        repo: id,
        generation: 3,
        ext: MidxExt::MultiPackIndex,
    };
    let reflog = [1, 20, 300].map(|seq| Key::RefLog { repo: id, seq });
    for key in reflog.iter().chain([&midx]) {
        assert!(bucket.create(key, Bytes::from_static(b"r")).await.unwrap());
    }
    bucket
        .put_replace(Key::RepoMeta { repo: id }, meta("model", &[]).to_bytes())
        .await
        .unwrap();
    // Not a key any writer makes: listing skips it.
    let packs_dir = dir.path().join(format!("repos/{ID}/packs"));
    std::fs::write(packs_dir.join("stray.pack"), b"?").unwrap();
    std::fs::write(packs_dir.join(format!("{PACK}.keep")), b"?").unwrap();

    let sorted = |mut keys: Vec<Key>| {
        keys.sort_by_key(ToString::to_string);
        keys
    };
    assert_eq!(
        sorted(bucket.list_under(id, KeyKind::Pack).await.unwrap()),
        sorted(all[..3].to_vec())
    );
    assert_eq!(
        sorted(bucket.list_under(other, KeyKind::Pack).await.unwrap()),
        sorted(all[3..].to_vec())
    );
    assert_eq!(
        sorted(bucket.list_under(id, KeyKind::RefLog).await.unwrap()),
        reflog.to_vec()
    );
    assert_eq!(bucket.list_under(id, KeyKind::Midx).await.unwrap(), [midx]);
    assert_eq!(
        bucket.list_under(id, KeyKind::RepoMeta).await.unwrap(),
        [Key::RepoMeta { repo: id }]
    );
    assert!(
        bucket
            .list_under(other, KeyKind::RefLog)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        bucket
            .list_under(repo("ffffffffffffffffffffffffffffffff"), KeyKind::Pack)
            .await
            .unwrap()
            .is_empty()
    );
    let err = bucket.list_under(id, KeyKind::Xorb).await.unwrap_err();
    assert!(matches!(err, StorageError::InvalidKey { .. }), "{err:?}");

    // `reindex` walks every repository's records with `list`.
    assert_eq!(
        bucket.list(KeyKind::RepoMeta).await.unwrap(),
        [Key::RepoMeta { repo: id }]
    );
    assert_eq!(
        sorted(bucket.list(KeyKind::Pack).await.unwrap()),
        sorted(all.clone())
    );
}

#[tokio::test]
async fn purge_record_is_create_only() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let first = PurgeRecord {
        file: h("purged"),
        actor: "operator".to_owned(),
        at: 1_761_400_000,
        through_seq: Some(42),
    };
    let key = first.key();
    assert_eq!(key, Key::Purge(h("purged")));
    assert!(bucket.create(&key, first.to_bytes()).await.unwrap());

    let again = PurgeRecord {
        actor: "someone-else".to_owned(),
        at: 1_761_400_999,
        ..first.clone()
    };
    assert!(!bucket.create(&key, again.to_bytes()).await.unwrap());
    let stored = bucket.get(&key).await.unwrap().unwrap();
    assert_eq!(PurgeRecord::from_bytes(&key, &stored).unwrap(), first);
    assert_eq!(
        bucket.list(KeyKind::Purge).await.unwrap(),
        std::slice::from_ref(&key)
    );

    let err = PurgeRecord::from_bytes(&Key::Purge(h("other")), &stored).unwrap_err();
    assert!(matches!(err, StorageError::CorruptRecord { .. }), "{err:?}");
}

/// `through_seq` (ruling M2-R13 as amended) is optional within version 1,
/// as `deleted_at` is: a record without it reads as `None`, which skips
/// every registration of the file, and one with it round-trips.
#[test]
fn purge_record_through_seq_is_optional_within_version_1() {
    let key = Key::Purge(h("purged"));
    let hex = hash_to_hex(&h("purged"));
    let without = format!(r#"{{"v":1,"file":"{hex}","actor":"operator","at":1761400000}}"#);
    let record = PurgeRecord::from_bytes(&key, without.as_bytes()).unwrap();
    assert_eq!(record.through_seq, None);
    assert_eq!(&record.to_bytes()[..], without.as_bytes());

    let with = PurgeRecord {
        through_seq: Some(7),
        ..record
    };
    let bytes = with.to_bytes();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains(r#""through_seq":7"#),
        "{bytes:?}"
    );
    assert_eq!(PurgeRecord::from_bytes(&key, &bytes).unwrap(), with);
}

#[test]
fn ref_log_record_round_trips_and_checks_its_entries() {
    let entry = |seq: u64, old: Option<&str>, new: Option<&str>| RefLogEntry {
        seq,
        ref_name: "refs/heads/main".to_owned(),
        old: old.map(str::to_owned),
        new: new.map(str::to_owned),
        actor: "alice".to_owned(),
        request: "req-1".to_owned(),
        at: 1_761_400_000 + seq,
    };
    let a = "1f2e3d4c5b6a79880716253443526170819a0b1c";
    let b = "ab".repeat(32);
    let record = RefLogRecord {
        repo: repo(ID),
        seq: 9,
        entries: vec![
            entry(7, None, Some(a)),
            entry(8, Some(a), Some(&b)),
            entry(9, Some(&b), None),
        ],
    };
    let key = record.key();
    assert_eq!(
        key,
        Key::RefLog {
            repo: repo(ID),
            seq: 9
        }
    );
    assert_eq!(
        RefLogRecord::from_bytes(&key, &record.to_bytes()).unwrap(),
        record
    );

    let broken = [
        // Its key is the highest entry's sequence number.
        RefLogRecord {
            seq: 8,
            ..record.clone()
        },
        // Entries are in increasing order.
        RefLogRecord {
            entries: vec![entry(9, None, Some(a)), entry(9, Some(a), None)],
            ..record.clone()
        },
        RefLogRecord {
            entries: Vec::new(),
            ..record.clone()
        },
        RefLogRecord {
            entries: vec![entry(9, Some("not-an-oid"), None)],
            ..record.clone()
        },
        RefLogRecord {
            entries: vec![entry(9, None, Some(&a.to_uppercase()))],
            ..record.clone()
        },
    ];
    for bad in broken {
        let key = Key::RefLog {
            repo: repo(ID),
            seq: bad.seq,
        };
        let err = RefLogRecord::from_bytes(&key, &bad.to_bytes()).unwrap_err();
        assert!(
            matches!(err, StorageError::CorruptRecord { .. }),
            "{bad:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn last_modified_reports_put_time() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = pack(ID, PackExt::Pack);
    assert_eq!(bucket.last_modified(key.clone()).await.unwrap(), None);

    let before = now();
    bucket
        .put_stream(key.clone(), blocks_of(b"pack", 2), 4)
        .await
        .unwrap();
    let after = now();
    let modified = bucket.last_modified(key).await.unwrap().unwrap();
    assert!(
        (before..=after).contains(&modified),
        "{before} <= {modified} <= {after}"
    );
}

fn files_in(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}
