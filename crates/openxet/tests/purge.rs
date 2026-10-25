//! `openxet purge` (ruling M2-R13), run as the real binary against the data
//! directory of a running `openxet serve` that holds one file in two scopes
//! (`docs/reference/cli.md`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Server, json_error, json_stdout, noise, openxet, run};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A server with two static write tokens for scopes `alpha` and `beta`, and
/// one file uploaded through both, so it is registered to both scopes.
struct Fixture {
    dir: TempDir,
    config: PathBuf,
    server: Server,
    tokens: [String; 2],
    hash: String,
}

fn generate_token(scope: &str) -> (String, String) {
    let out = run(openxet().args([
        "token",
        "generate",
        "--scope",
        scope,
        "--mode",
        "write",
        "--subject",
        scope,
        "--json",
    ]));
    let generated = json_stdout(&out);
    (
        generated["token"].as_str().unwrap().to_owned(),
        generated["config"].as_str().unwrap().to_owned(),
    )
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let (alpha, alpha_entry) = generate_token("alpha");
        let (beta, beta_entry) = generate_token("beta");
        let config = dir.path().join("openxet.toml");
        std::fs::write(&config, format!("{alpha_entry}\n{beta_entry}")).unwrap();
        let server = Server::start(&dir.path().join("data"), Some(&config));
        let fixture = Self {
            config,
            server,
            tokens: [alpha, beta],
            hash: String::new(),
            dir,
        };
        let file = fixture.dir.path().join("model.bin");
        std::fs::write(&file, noise(300_000, 17)).unwrap();
        let hashes: Vec<String> = (0..2).map(|i| fixture.upload(i, &file)).collect();
        assert_eq!(hashes[0], hashes[1]);
        Self {
            hash: hashes[0].clone(),
            ..fixture
        }
    }

    fn cache(&self, name: &str) -> PathBuf {
        self.dir.path().join("caches").join(name)
    }

    fn upload(&self, token: usize, file: &Path) -> String {
        let out = run(self
            .server
            .client(
                &self.tokens[token],
                &self.cache(&format!("up-{token}-{}", unique())),
            )
            .arg("upload")
            .arg(file)
            .arg("--json"));
        json_stdout(&out)["files"][0]["hash"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Downloads `hash` with token `token` into a fresh cache.
    fn download(&self, token: usize, hash: &str) -> Output {
        let suffix = unique();
        run(self
            .server
            .client(
                &self.tokens[token],
                &self.cache(&format!("down-{token}-{suffix}")),
            )
            .args(["download", hash, "--force", "--json", "--output"])
            .arg(self.dir.path().join(format!("out-{suffix}"))))
    }

    fn assert_served_in_both_scopes(&self) {
        for token in 0..2 {
            json_stdout(&self.download(token, &self.hash));
        }
    }

    /// `openxet purge` of the fixture's file, with the server's config and
    /// data directory.
    fn purge(&self) -> Command {
        let mut cmd = openxet();
        cmd.args(["purge", &self.hash, "--config"])
            .arg(&self.config)
            .arg("--data-dir")
            .arg(&self.server.data_dir);
        cmd
    }

    fn record_path(&self) -> PathBuf {
        self.server
            .data_dir
            .join("bucket/purges")
            .join(&self.hash[..2])
            .join(&self.hash)
    }

    fn record(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.record_path()).unwrap()).unwrap()
    }
}

/// A distinct name per call within one test process.
fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn purge_dry_run_changes_nothing() {
    let f = Fixture::new();
    for _ in 0..2 {
        let out = run(f.purge().args(["--dry-run", "--json"]));
        assert_eq!(
            json_stdout(&out),
            json!({
                "schema": "openxet.purge.v1",
                "file": f.hash,
                "scopes_removed": 2,
                "dry_run": true,
            })
        );
    }
    let human = run(f.purge().arg("--dry-run"));
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("would remove") && text.contains("2 scopes"),
        "{text}"
    );

    assert!(!f.record_path().exists());
    f.assert_served_in_both_scopes();
}

#[test]
fn purge_removes_file_from_every_scope_and_cas_answers_404() {
    let f = Fixture::new();
    let other = f.dir.path().join("other.bin");
    std::fs::write(&other, noise(200_000, 18)).unwrap();
    let other_hash = f.upload(0, &other);

    let out = run(f.purge().args(["--yes", "--json", "--actor", "ops"]));
    assert_eq!(
        json_stdout(&out),
        json!({
            "schema": "openxet.purge.v1",
            "file": f.hash,
            "scopes_removed": 2,
            "dry_run": false,
        })
    );

    // The running server answers 404 in both scopes at once: nothing caches
    // a reconstruction.
    for token in 0..2 {
        let error = json_error(&f.download(token, &f.hash), 1);
        assert_eq!(error["code"], "not_found", "{error}");
    }
    json_stdout(&f.download(0, &other_hash));

    let record = f.record();
    assert_eq!(record["v"], 1);
    assert_eq!(record["file"], f.hash.as_str());
    assert_eq!(record["actor"], "ops");

    // Purging again removes nothing more and succeeds.
    let again = run(f.purge().args(["--yes", "--json"]));
    assert_eq!(json_stdout(&again)["scopes_removed"], 0);
    assert_eq!(f.record()["actor"], "ops", "the first record stays");
}

/// The bucket's purge record is written before any index row is removed
/// (design §5.3, ruling M2-R13): when it can't be written, the file stays
/// registered everywhere.
#[test]
fn purge_writes_bucket_record_first() {
    let f = Fixture::new();
    // A file where the record's prefix directory must go makes the bucket
    // write fail on every platform, whoever runs the test.
    let blocker = f.record_path().parent().unwrap().to_owned();
    std::fs::write(&blocker, b"in the way").unwrap();

    let error = json_error(&run(f.purge().args(["--yes", "--json"])), 1);
    assert_eq!(error["code"], "storage", "{error}");
    f.assert_served_in_both_scopes();
    let dry = run(f.purge().args(["--dry-run", "--json"]));
    assert_eq!(json_stdout(&dry)["scopes_removed"], 2);

    std::fs::remove_file(&blocker).unwrap();
    let out = run(f
        .purge()
        .args(["--yes", "--json"])
        .env("USER", "operator")
        .env_remove("LOGNAME")
        .env_remove("USERNAME"));
    assert_eq!(json_stdout(&out)["scopes_removed"], 2);
    assert_eq!(f.record()["actor"], "cli:operator");
}

#[test]
fn purge_json_requires_yes() {
    let f = Fixture::new();
    let error = json_error(&run(f.purge().arg("--json")), 2);
    assert_eq!(error["code"], "confirmation_required", "{error}");
    assert!(error["hint"].as_str().unwrap().contains("--yes"), "{error}");

    // Without a terminal to ask on, a person's run is refused the same way.
    let human = run(&mut f.purge());
    assert_eq!(human.status.code(), Some(2));
    let text = String::from_utf8(human.stderr).unwrap();
    assert!(text.contains("--yes"), "{text}");

    assert!(!f.record_path().exists());
    f.assert_served_in_both_scopes();
}

#[test]
fn purge_refuses_bad_input_without_touching_anything() {
    let dir = TempDir::new().unwrap();
    let error = json_error(
        &run(openxet()
            .args(["purge", "not-a-hash", "--yes", "--json", "--data-dir"])
            .arg(dir.path())),
        2,
    );
    assert_eq!(error["code"], "invalid_hash", "{error}");

    // A data directory no server has used holds no index: nothing is created.
    let missing = dir.path().join("nowhere");
    let error = json_error(
        &run(openxet()
            .args(["purge", &"0".repeat(64), "--yes", "--json", "--data-dir"])
            .arg(&missing)),
        1,
    );
    assert_eq!(error["code"], "index", "{error}");
    assert!(
        error["hint"].as_str().unwrap().contains("--data-dir"),
        "{error}"
    );
    assert!(!missing.exists());
}
