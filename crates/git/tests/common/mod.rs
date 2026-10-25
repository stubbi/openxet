//! Shared test harness: a SQLite index, a local bucket and a node cache in
//! one temp dir, fixture repositories built by `testdata/build-fixture.sh`,
//! and packs cut from them with `git pack-objects`.

#![allow(dead_code, clippy::missing_panics_doc)]

pub mod fetch;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use bytes::Bytes;
use openxet_git::{Engine, GitConfig};
use openxet_index::{
    CommitTx, GitStore, Oid, Pack, PackId, PackState, RefUpdate, Repo, RepoId, Repos, SqliteIndex,
};
use openxet_storage::{Bucket, Key, LocalBucket, PackExt, PackName, RepoIdHex};
use tempfile::TempDir;

pub fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("call failed: {e}"),
    }
}

/// A `git` run with the caller's configuration shut out; panics with git's
/// stderr on failure, returns stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    git_with_input(dir, args, "")
}

pub fn git_with_input(dir: &Path, args: &[&str], input: &str) -> String {
    let mut child = ok(Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn());
    if let Some(mut stdin) = child.stdin.take() {
        ok(stdin.write_all(input.as_bytes()));
    }
    let out = ok(child.wait_with_output());
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    ok(String::from_utf8(out.stdout))
}

/// A fixture repository of `kind` (`tf10k` or `lfs`) with `commits` commits.
pub fn fixture(kind: &str, commits: u32) -> TempDir {
    let dir = ok(TempDir::new());
    let repo = dir.path().join("repo.git");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/build-fixture.sh");
    let status = ok(Command::new(script)
        .arg(kind)
        .arg(&repo)
        .arg(commits.to_string())
        .status());
    assert!(status.success(), "build-fixture.sh {kind} failed");
    dir
}

pub fn fixture_repo(dir: &TempDir) -> PathBuf {
    dir.path().join("repo.git")
}

pub fn rev(repo: &Path, name: &str) -> Oid {
    ok(git(repo, &["rev-parse", name]).trim().parse())
}

/// The files of one pack, written by `git pack-objects` into `dir`.
#[derive(Debug, Clone)]
pub struct PackFiles {
    pub name: String,
    pub dir: PathBuf,
}

impl PackFiles {
    pub fn path(&self, ext: PackExt) -> PathBuf {
        self.dir.join(format!("{}.{}", self.name, ext.as_str()))
    }

    pub fn exts(&self) -> Vec<PackExt> {
        [
            PackExt::Pack,
            PackExt::Idx,
            PackExt::Rev,
            PackExt::Bitmap,
            PackExt::Mtimes,
        ]
        .into_iter()
        .filter(|e| self.path(*e).exists())
        .collect()
    }

    pub fn bytes(&self) -> u64 {
        ok(std::fs::metadata(self.path(PackExt::Pack))).len()
    }
}

/// Packs the objects of `revs` (one `git rev-list` argument per line) from
/// `repo` into `out`.
pub fn pack_objects(repo: &Path, revs: &str, out: &Path) -> PackFiles {
    ok(std::fs::create_dir_all(out));
    let base = out.join("pack");
    let sum = git_with_input(
        repo,
        &["pack-objects", "--revs", "-q", &base.to_string_lossy()],
        &format!("{revs}\n"),
    );
    PackFiles {
        name: format!("pack-{}", sum.trim()),
        dir: out.to_path_buf(),
    }
}

/// One index, one bucket and one node cache, in one temp dir.
pub struct World {
    pub dir: TempDir,
    pub index: Arc<SqliteIndex>,
    pub bucket: Arc<LocalBucket>,
}

impl World {
    pub async fn new() -> Self {
        let dir = ok(TempDir::new());
        let index = Arc::new(ok(SqliteIndex::open(&dir.path().join("index.sqlite")).await));
        let bucket = Arc::new(ok(LocalBucket::open(&dir.path().join("bucket"))));
        Self { dir, index, bucket }
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.dir.path().join("git-cache")
    }

    pub fn config(&self) -> GitConfig {
        GitConfig::new(self.cache_dir())
    }

    pub async fn engine(&self) -> Engine {
        ok(Engine::start(self.index.clone(), self.bucket.clone(), self.config()).await)
    }

    pub async fn repo(&self, namespace: &str, name: &str) -> RepoId {
        let repo = Repo {
            id: RepoId::generate(),
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            private: false,
            labels: vec!["model".to_owned()],
            created_at: 1_761_400_000,
            full_repack_gen: 0,
        };
        ok(self.index.create_repo(&repo).await);
        repo.id
    }

    /// Uploads every file of `pack` to the bucket.
    pub async fn upload(&self, repo: RepoId, pack: &PackFiles) {
        let repo_hex = ok(RepoIdHex::parse(&repo.to_string()));
        for ext in pack.exts() {
            let key = Key::Pack {
                repo: repo_hex,
                name: ok(PackName::parse(&pack.name)),
                ext,
            };
            let body = ok(std::fs::read(pack.path(ext)));
            ok(self.bucket.put(&key, Bytes::from(body)).await);
        }
    }

    /// Uploads `pack` and records it as pending.
    pub async fn pending(&self, repo: RepoId, pack: &PackFiles, created_at: u64) -> Pack {
        self.pending_with(repo, pack, created_at, None).await
    }

    /// Uploads `pack` and records it as pending, written with `midx_gen`.
    pub async fn pending_with(
        &self,
        repo: RepoId,
        pack: &PackFiles,
        created_at: u64,
        midx_gen: Option<u64>,
    ) -> Pack {
        self.upload(repo, pack).await;
        let row = Pack {
            id: PackId::generate(),
            repo,
            name: pack.name.clone(),
            state: PackState::Pending,
            created_at,
            bytes: pack.bytes(),
            midx_gen,
        };
        ok(self.index.insert_pack(&row).await);
        row
    }

    /// Uploads `pack` and commits it with `updates`.
    pub async fn commit(
        &self,
        repo: RepoId,
        pack: &PackFiles,
        created_at: u64,
        updates: &[RefUpdate],
    ) -> Pack {
        self.commit_with(repo, pack, created_at, None, updates)
            .await
    }

    /// Uploads `pack`, written with `midx_gen`, and commits it with
    /// `updates`.
    pub async fn commit_with(
        &self,
        repo: RepoId,
        pack: &PackFiles,
        created_at: u64,
        midx_gen: Option<u64>,
        updates: &[RefUpdate],
    ) -> Pack {
        let row = self.pending_with(repo, pack, created_at, midx_gen).await;
        let generation = ok(self.index.snapshot(repo).await).full_repack_gen;
        ok(self
            .index
            .commit(CommitTx {
                repo,
                updates,
                pack: Some(row.id),
                full_repack_gen: generation,
                actor: "tester",
                request: "test",
            })
            .await);
        Pack {
            state: PackState::Committed,
            ..row
        }
    }

    pub async fn sql(&self) -> sqlx::SqlitePool {
        ok(sqlx::SqlitePool::connect(&format!(
            "sqlite://{}",
            self.dir.path().join("index.sqlite").display()
        ))
        .await)
    }
}

pub fn set(name: &str, old: Option<Oid>, new: Oid) -> RefUpdate {
    RefUpdate {
        name: name.to_owned(),
        old,
        new: Some(new),
    }
}

/// The file names in a snapshot's `objects/pack`, sorted.
pub fn pack_dir_listing(snapshot_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = ok(std::fs::read_dir(snapshot_dir.join("objects/pack")))
        .map(|e| ok(e).file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// What a snapshot holding exactly `packs` lists in `objects/pack`.
pub fn expected_listing(packs: &[&PackFiles]) -> Vec<String> {
    let mut names: Vec<String> = packs
        .iter()
        .flat_map(|p| {
            p.exts()
                .into_iter()
                .map(|e| format!("{}.{}", p.name, e.as_str()))
        })
        .collect();
    names.sort();
    names
}
