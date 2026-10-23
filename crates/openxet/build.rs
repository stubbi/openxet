use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn main() {
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=OPENXET_GIT_COMMIT={commit}");

    // Rerun when HEAD moves. In a linked worktree HEAD lives in the
    // worktree's own git dir and the refs in the common dir, so ask git for
    // both rather than assuming `../../.git`. Outside git there is nothing to
    // watch, and cargo's default (rerun on any package change) applies.
    if let Some(dirs) = git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
    ]) && let [git_dir, common_dir] = dirs.lines().collect::<Vec<_>>()[..]
    {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={common_dir}/refs");
        println!("cargo:rerun-if-changed={common_dir}/packed-refs");
    }
}
