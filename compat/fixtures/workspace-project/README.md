# Workspace gate project

The Cargo workspace `compat/tests/test_workspaces.py` builds in six agent
workspaces at once. Three small crates (`model`, `codec`, `report`) depend on
`serde` with `derive`, `serde_json` and `itoa`, so a build runs build scripts
(`serde`, `serde_core`, `serde_json`, `proc-macro2`, `quote`, `zmij`) and a
proc macro (`serde_derive` on `syn`), as real projects do. A debug build
writes about 50 MiB to `target/`.

`vendor/` is `cargo vendor` output, so the gate builds offline. The test
commits only the project's own files to its repository and points Cargo at
`vendor/` here through `CARGO_HOME`, as `~/.cargo/registry` sits outside
every checkout. `.cargo/config.toml` lets the project build in place:

```bash
cd compat/fixtures/workspace-project
cargo build --offline --locked
```

To change a dependency, edit `Cargo.toml`, then run `cargo update` and
`rm -rf vendor && cargo vendor --locked vendor` with network access, and
commit `Cargo.lock` and `vendor/` together.
