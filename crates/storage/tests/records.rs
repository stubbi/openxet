//! The bucket's registration and ownership records (ruling M1-R37): their
//! keys, their bytes, and the bucket operations `reindex` relies on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bytes::Bytes;
use openxet_protocol::{MerkleHash, chunk_hash, hash_to_hex};
use openxet_storage::{
    Bucket, Key, KeyKind, LocalBucket, NameHash, Registration, StorageError, XorbOwner,
};
use sha2::{Digest as _, Sha256};

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

fn sha256_hex(name: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(name.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

fn registration(scope: &str, tenant: &str, shard: &str, seq: u64) -> Registration {
    Registration {
        scope: scope.to_owned(),
        tenant: tenant.to_owned(),
        shard: h(shard),
        seq,
    }
}

#[test]
fn record_keys_carry_the_sha256_of_scope_and_tenant() {
    let shard = h("shard");
    let xorb = h("xorb");
    assert_eq!(
        Key::registration("org/model", "org", shard).to_string(),
        format!(
            "registrations/{}/{}/{}",
            sha256_hex("org/model"),
            sha256_hex("org"),
            hash_to_hex(&shard)
        )
    );
    assert_eq!(
        Key::xorb_owner("org", xorb).to_string(),
        format!("xorb-owners/{}/{}", sha256_hex("org"), hash_to_hex(&xorb))
    );
    assert_eq!(NameHash::of("org").to_string(), sha256_hex("org"));
}

/// Scopes and tenants are arbitrary strings of up to 1024 bytes: one that
/// looks like a path, or is too long for a file name, still maps to one
/// fixed-length key and never escapes its directory.
#[tokio::test]
async fn hostile_names_stay_inside_their_key() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let long = "s".repeat(1024);
    for name in [
        "../../etc/passwd",
        "a/b\\c",
        ".",
        "..",
        long.as_str(),
        "ünï ©ode",
    ] {
        let record = registration(name, name, "shard", 1);
        bucket
            .create(&record.key(), record.to_bytes())
            .await
            .unwrap();
        let read = bucket.get(&record.key()).await.unwrap().unwrap();
        assert_eq!(
            Registration::from_bytes(&record.key(), &read).unwrap(),
            record
        );
        let owner = XorbOwner {
            tenant: name.to_owned(),
            xorb: h("x"),
        };
        bucket.put(&owner.key(), owner.to_bytes()).await.unwrap();
    }
    for entry in walk(dir.path()) {
        let relative = entry.strip_prefix(dir.path()).unwrap();
        assert!(
            relative
                .components()
                .all(|c| c.as_os_str().len() <= 64 && c.as_os_str() != ".."),
            "{relative:?}"
        );
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        }
        out.push(p);
    }
    out
}

/// The record bytes are a versioned JSON object, the layout `reindex`
/// reads (issue #26).
#[test]
fn records_are_versioned_json() {
    let record = registration("org/model", "org", "shard", 7);
    assert_eq!(
        std::str::from_utf8(&record.to_bytes()).unwrap(),
        format!(
            r#"{{"v":1,"scope":"org/model","tenant":"org","shard":"{}","seq":7}}"#,
            hash_to_hex(&h("shard"))
        )
    );
    let owner = XorbOwner {
        tenant: "org".to_owned(),
        xorb: h("x"),
    };
    assert_eq!(
        std::str::from_utf8(&owner.to_bytes()).unwrap(),
        format!(
            r#"{{"v":1,"tenant":"org","xorb":"{}"}}"#,
            hash_to_hex(&h("x"))
        )
    );
    assert_eq!(
        XorbOwner::from_bytes(&owner.key(), &owner.to_bytes()).unwrap(),
        owner
    );
}

#[test]
fn records_that_do_not_match_their_key_or_version_are_refused() {
    let record = registration("org/model", "org", "shard", 7);
    let key = record.key();
    let shard = hash_to_hex(&h("shard"));
    let bad = [
        format!(r#"{{"v":2,"scope":"org/model","tenant":"org","shard":"{shard}","seq":7}}"#),
        format!(r#"{{"v":1,"scope":"org/other","tenant":"org","shard":"{shard}","seq":7}}"#),
        format!(r#"{{"v":1,"scope":"org/model","tenant":"other","shard":"{shard}","seq":7}}"#),
        format!(
            r#"{{"v":1,"scope":"org/model","tenant":"org","shard":"{}","seq":7}}"#,
            hash_to_hex(&h("other"))
        ),
        format!(r#"{{"v":1,"scope":"org/model","tenant":"org","shard":"{shard}"}}"#),
        format!(r#"{{"v":1,"scope":"org/model","tenant":"org","shard":"{shard}","seq":7,"x":1}}"#),
        "not json".to_owned(),
    ];
    for bytes in bad {
        let err = Registration::from_bytes(&key, bytes.as_bytes()).unwrap_err();
        assert!(
            matches!(err, StorageError::CorruptRecord { .. }),
            "{bytes}: {err:?}"
        );
        assert!(err.to_string().contains(&key.to_string()), "{err}");
    }
    let v2 = format!(r#"{{"v":2,"scope":"org/model","tenant":"org","shard":"{shard}","seq":7}}"#);
    let err = Registration::from_bytes(&key, v2.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("newer openxet"), "{err}");
}

/// `create` stores an object only if its key is absent, so the first
/// writer's record is the one every writer then reads.
#[tokio::test]
async fn create_keeps_the_first_object() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(LocalBucket::open(dir.path()).unwrap());
    let key = Key::registration("s", "t", h("shard"));
    let tasks: Vec<_> = (0..16u8)
        .map(|i| {
            let (b, key) = (Arc::clone(&bucket), key.clone());
            tokio::spawn(async move {
                let bytes = Bytes::from(vec![i; 10 + usize::from(i)]);
                (b.create(&key, bytes.clone()).await.unwrap(), bytes)
            })
        })
        .collect();
    let mut created = Vec::new();
    for t in tasks {
        let (won, bytes) = t.await.unwrap();
        if won {
            created.push(bytes);
        }
    }
    assert_eq!(created.len(), 1, "exactly one create wins");
    assert_eq!(bucket.get(&key).await.unwrap(), Some(created[0].clone()));
    assert!(
        walk(&dir.path().join(".tmp")).is_empty(),
        "temp files left behind"
    );
    assert!(
        !bucket
            .create(&key, Bytes::from_static(b"later"))
            .await
            .unwrap()
    );
    assert_eq!(bucket.get(&key).await.unwrap(), Some(created[0].clone()));
}

#[tokio::test]
async fn get_of_a_missing_key_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    assert_eq!(bucket.get(&Key::Shard(h("missing"))).await.unwrap(), None);
}

/// `list` names every object of a kind, which is how `reindex` finds them.
#[tokio::test]
async fn list_names_every_object_of_a_kind() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let mut want: Vec<(KeyKind, Key)> = Vec::new();
    for i in 0..5 {
        let label = format!("o{i}");
        let keys = [
            (KeyKind::Xorb, Key::Xorb(h(&label))),
            (KeyKind::Shard, Key::Shard(h(&label))),
            (
                KeyKind::Registration,
                Key::registration(&format!("scope {i}"), "t", h(&label)),
            ),
            (
                KeyKind::XorbOwner,
                Key::xorb_owner(&format!("t{i}"), h(&label)),
            ),
        ];
        for (kind, key) in keys {
            bucket.put(&key, Bytes::from_static(b"x")).await.unwrap();
            want.push((kind, key));
        }
    }
    std::fs::write(dir.path().join("shards").join("stray"), b"x").unwrap();
    for kind in [
        KeyKind::Xorb,
        KeyKind::Shard,
        KeyKind::Registration,
        KeyKind::XorbOwner,
    ] {
        let mut listed: Vec<String> = bucket
            .list(kind)
            .await
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        listed.sort();
        let mut expected: Vec<String> = want
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, key)| key.to_string())
            .collect();
        expected.sort();
        assert_eq!(listed, expected, "{kind:?}");
    }
}
