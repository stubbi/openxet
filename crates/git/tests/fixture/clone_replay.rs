//! Spike S9's server-only fetch replay (`docs/research/S9-git-engine.md`,
//! "Server only"), for the fetch gate (`tests/fetch_gate.rs`) and benchmark
//! (`benches/fetch.rs`): one protocol v2 `fetch` request, served once by
//! openxet's upload-pack and once by `git upload-pack` alone on the same
//! repository, the response read whole and discarded, so no client
//! `index-pack` is measured.
//!
//! The repository is `testdata/build-fixture.sh`'s `tf10k` of
//! [`COMMITS`] commits, repacked into one pack with a bitmap as S9's was
//! (`git repack -adb`), and committed to openxet's index and bucket as that
//! one pack. Each side's CPU is what the operating system charged for the
//! request: for openxet, this process and every git it started (getrusage
//! `RUSAGE_SELF` plus `RUSAGE_CHILDREN`, as S9 counted it); for git, the
//! `git upload-pack` child and its `pack-objects`. Run each request once
//! first: openxet's first request hydrates its node cache.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt as _;
use nix::sys::resource::{UsageWho, getrusage};
use openxet_git::{Engine, GitConfig};
use openxet_index::{
    CommitTx, GitStore as _, Oid, Pack, PackId, PackState, RefUpdate, Repo, RepoId, Repos as _,
    SqliteIndex,
};
use openxet_storage::{Bucket as _, Key, LocalBucket, PackExt, PackName, RepoIdHex};
use tempfile::TempDir;

/// Commits in the replayed repository: S9's `tf10k` had 10,001.
pub const COMMITS: u32 = 10_001;

/// What serving one request cost.
#[derive(Debug, Clone, Copy)]
pub struct Served {
    /// CPU time, user and system.
    pub cpu: Duration,
    /// Wall time from the request to the response's last byte.
    pub wall: Duration,
    /// Response bytes.
    pub bytes: u64,
}

/// The replayed repository, served by openxet and by git.
pub struct Replay {
    engine: Engine,
    repo: RepoId,
    /// The bare repository git serves directly.
    src: PathBuf,
    /// `refs/heads/main`.
    pub main: Oid,
    /// `refs/tags/v1.0`, an annotated tag of `main`.
    pub tag: Oid,
    _dir: TempDir,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn oid(dir: &Path, rev: &str) -> Oid {
    git(dir, &["rev-parse", rev]).trim().parse().unwrap()
}

/// CPU this process and its reaped children have used so far.
fn cpu_now(who: &[UsageWho]) -> Duration {
    who.iter()
        .map(|w| {
            let usage = getrusage(*w).unwrap();
            let micros = |t: nix::sys::time::TimeVal| {
                Duration::from_secs(u64::try_from(t.tv_sec()).unwrap())
                    + Duration::from_micros(u64::try_from(t.tv_usec()).unwrap())
            };
            micros(usage.user_time()) + micros(usage.system_time())
        })
        .sum()
}

fn pkt(out: &mut Vec<u8>, line: &str) {
    match line {
        "0000" | "0001" => out.extend_from_slice(line.as_bytes()),
        line => {
            let data = format!("{line}\n");
            out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
            out.extend_from_slice(data.as_bytes());
        }
    }
}

/// A protocol v2 `fetch` request for `wants` with `haves`, as `git fetch`
/// sends it: `thin-pack` and `ofs-delta`, no progress.
pub fn fetch_request(wants: &[Oid], haves: &[Oid]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in ["command=fetch", "object-format=sha1", "0001"] {
        pkt(&mut out, line);
    }
    for line in ["thin-pack", "no-progress", "ofs-delta"] {
        pkt(&mut out, line);
    }
    for want in wants {
        pkt(&mut out, &format!("want {want}"));
    }
    for have in haves {
        pkt(&mut out, &format!("have {have}"));
    }
    pkt(&mut out, "done");
    pkt(&mut out, "0000");
    out
}

impl Replay {
    /// The `fetch` request of a clone: every ref, nothing in hand.
    pub fn clone_request(&self) -> Vec<u8> {
        fetch_request(&[self.main, self.tag], &[])
    }

    /// The `fetch` request of a clone `behind` commits behind `main`.
    pub fn fetch_behind_request(&self, behind: u32) -> Vec<u8> {
        let have = oid(&self.src, &format!("main~{behind}"));
        fetch_request(&[self.main], &[have])
    }

    /// openxet serves `request`, as the smart HTTP route hands it over.
    pub async fn openxet(&self, request: &[u8]) -> Served {
        let body: openxet_storage::BodyStream =
            futures_util::stream::iter([Ok(Bytes::copy_from_slice(request))]).boxed();
        let before = cpu_now(&[UsageWho::RUSAGE_SELF, UsageWho::RUSAGE_CHILDREN]);
        let started = Instant::now();
        let mut stream = self
            .engine
            .upload_pack(self.repo, 2, "bench", body)
            .await
            .unwrap();
        let mut bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            bytes += chunk.unwrap().len() as u64;
        }
        let wall = started.elapsed();
        drop(stream);
        let cpu =
            cpu_now(&[UsageWho::RUSAGE_SELF, UsageWho::RUSAGE_CHILDREN]).saturating_sub(before);
        Served { cpu, wall, bytes }
    }

    /// `git upload-pack --stateless-rpc` serves `request` from the bare
    /// repository directly, as `git http-backend` would run it.
    pub fn git(&self, request: &[u8]) -> Served {
        let before = cpu_now(&[UsageWho::RUSAGE_CHILDREN]);
        let started = Instant::now();
        let mut child = Command::new("git")
            .args(["upload-pack", "--stateless-rpc"])
            .arg(&self.src)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_PROTOCOL", "version=2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let request = request.to_vec();
        let feed = std::thread::spawn(move || stdin.write_all(&request));
        let mut stdout = child.stdout.take().unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut bytes = 0u64;
        loop {
            let n = stdout.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            bytes += n as u64;
        }
        assert!(child.wait().unwrap().success(), "git upload-pack failed");
        feed.join().unwrap().unwrap();
        let wall = started.elapsed();
        let cpu = cpu_now(&[UsageWho::RUSAGE_CHILDREN]).saturating_sub(before);
        Served { cpu, wall, bytes }
    }
}

/// Builds the repository and commits it to a fresh index and bucket.
pub async fn replay() -> Replay {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("tf10k.git");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/build-fixture.sh");
    let status = Command::new(script)
        .arg("tf10k")
        .arg(&src)
        .arg(COMMITS.to_string())
        .status()
        .unwrap();
    assert!(status.success(), "build-fixture.sh failed");
    git(&src, &["repack", "-adbq"]);
    let main = oid(&src, "refs/heads/main");
    let tag = oid(&src, "refs/tags/v1.0");

    let index = Arc::new(
        SqliteIndex::open(&dir.path().join("index.sqlite"))
            .await
            .unwrap(),
    );
    let bucket = Arc::new(LocalBucket::open(&dir.path().join("bucket")).unwrap());
    let repo = Repo {
        id: RepoId::generate(),
        namespace: "bench".to_owned(),
        name: "tf10k".to_owned(),
        private: false,
        labels: vec!["model".to_owned()],
        created_at: 1_761_400_000,
        full_repack_gen: 0,
    };
    index.create_repo(&repo).await.unwrap();

    let packs = src.join("objects/pack");
    let name = std::fs::read_dir(&packs)
        .unwrap()
        .find_map(|e| {
            let file = e.unwrap().file_name().into_string().unwrap();
            file.strip_suffix(".pack").map(str::to_owned)
        })
        .unwrap();
    let hex = RepoIdHex::parse(&repo.id.to_string()).unwrap();
    for ext in [PackExt::Pack, PackExt::Idx, PackExt::Rev, PackExt::Bitmap] {
        let path = packs.join(format!("{name}.{}", ext.as_str()));
        if !path.exists() {
            assert!(
                !matches!(ext, PackExt::Pack | PackExt::Idx | PackExt::Bitmap),
                "{} is missing",
                path.display()
            );
            continue;
        }
        let key = Key::Pack {
            repo: hex,
            name: PackName::parse(&name).unwrap(),
            ext,
        };
        bucket
            .put(&key, Bytes::from(std::fs::read(&path).unwrap()))
            .await
            .unwrap();
    }
    let pack = Pack {
        id: PackId::generate(),
        repo: repo.id,
        name: name.clone(),
        state: PackState::Pending,
        created_at: 1_761_400_000,
        bytes: std::fs::metadata(packs.join(format!("{name}.pack")))
            .unwrap()
            .len(),
        midx_gen: None,
    };
    index.insert_pack(&pack).await.unwrap();
    let set = |name: &str, new: Oid| RefUpdate {
        name: name.to_owned(),
        old: None,
        new: Some(new),
    };
    index
        .commit(CommitTx {
            repo: repo.id,
            updates: &[set("refs/heads/main", main), set("refs/tags/v1.0", tag)],
            pack: Some(pack.id),
            full_repack_gen: 0,
            actor: "bench",
            request: "bench",
        })
        .await
        .unwrap();

    let engine = Engine::start(index, bucket, GitConfig::new(dir.path().join("git-cache")))
        .await
        .unwrap();
    Replay {
        engine,
        repo: repo.id,
        src,
        main,
        tag,
        _dir: dir,
    }
}

/// The median of `values`.
pub fn median(values: &mut [Duration]) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}
