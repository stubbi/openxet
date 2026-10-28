#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use openxet_workspace::{Class, Presets, classify, reconstructible_dirs};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn repo_with_gitignore(ignore: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    fs::write(dir.path().join(".gitignore"), ignore).unwrap();
    dir
}

#[test]
fn presets_are_target_node_modules_venv() {
    assert_eq!(
        Presets::default().dirs(),
        [
            PathBuf::from("target"),
            PathBuf::from("node_modules"),
            PathBuf::from(".venv"),
        ]
    );

    let repo = repo_with_gitignore("/target/\nnode_modules/\n.venv\n");
    for dir in ["target/debug", "node_modules/x", ".venv/bin"] {
        fs::create_dir_all(repo.path().join(dir)).unwrap();
    }
    let presets = Presets::load(repo.path()).unwrap();
    assert_eq!(presets, Presets::default());
    let mut found = reconstructible_dirs(repo.path(), &presets);
    found.sort();
    assert_eq!(
        found,
        [
            repo.path().join(".venv"),
            repo.path().join("node_modules"),
            repo.path().join("target"),
        ]
    );
}

#[test]
fn project_config_adds_reconstructible_dirs() {
    let repo = repo_with_gitignore("target/\nbuild/\n");
    fs::create_dir_all(repo.path().join(".openxet")).unwrap();
    fs::write(
        repo.path().join(".openxet/workspace.toml"),
        "reconstructible = [\"build/cache\", \"missing\"]\n",
    )
    .unwrap();
    fs::create_dir_all(repo.path().join("target")).unwrap();
    fs::create_dir_all(repo.path().join("build/cache")).unwrap();

    let presets = Presets::load(repo.path()).unwrap();
    assert!(presets.dirs().contains(&PathBuf::from("build/cache")));
    assert!(presets.dirs().contains(&PathBuf::from("target")));
    assert_eq!(
        reconstructible_dirs(repo.path(), &presets),
        [repo.path().join("target"), repo.path().join("build/cache")]
    );
    assert_eq!(
        classify(repo.path(), Path::new("build/cache/obj.o"), &presets).unwrap(),
        Class::Reconstructible
    );

    for bad in ["\"../outside\"", "\"/abs\"", "\".git\"", "\"\""] {
        fs::write(
            repo.path().join(".openxet/workspace.toml"),
            format!("reconstructible = [{bad}]\n"),
        )
        .unwrap();
        let err = Presets::load(repo.path()).unwrap_err();
        assert_eq!(err.code(), "config", "{bad} was accepted");
    }
}

#[test]
fn ignored_path_outside_presets_is_durable() {
    let repo = repo_with_gitignore("target/\nbuild/\n");
    fs::create_dir_all(repo.path().join("build")).unwrap();
    fs::create_dir_all(repo.path().join("target/debug")).unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("build/out.bin"), "x").unwrap();
    fs::write(repo.path().join("target/debug/app"), "x").unwrap();
    fs::write(repo.path().join("target/kept.txt"), "x").unwrap();
    fs::write(repo.path().join("src/new.rs"), "x").unwrap();
    git(repo.path(), &["add", "--force", "target/kept.txt"]);
    let presets = Presets::default();

    let class = |path: &str| classify(repo.path(), Path::new(path), &presets).unwrap();
    assert_eq!(class("build/out.bin"), Class::Durable);
    assert_eq!(class("src/new.rs"), Class::Durable);
    assert_eq!(class("target/kept.txt"), Class::Durable);
    assert_eq!(class("target/debug/app"), Class::Reconstructible);
    assert_eq!(class("target/../build/out.bin"), Class::Durable);

    // A preset directory that holds a tracked file is not evictable as a
    // whole.
    assert_eq!(
        reconstructible_dirs(repo.path(), &presets),
        Vec::<PathBuf>::new()
    );
}
