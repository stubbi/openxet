//! The node cache and per-request snapshot repositories (ADR 0004 items 1,
//! 4 and 7): which packs a snapshot exposes, where it lives, how hydration
//! pins mtimes, that paths come from ids, and how eviction counts links.

mod common;

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{
    PackFiles, World, expected_listing, fixture, fixture_repo, git, ok, pack_dir_listing,
    pack_objects, rev, set,
};
use openxet_git::{Engine, GitError};
use openxet_index::{GitStore as _, Oid, PackState, RepackPublish};
use openxet_storage::{Bucket as _, Key, MidxExt, PackExt, RepoIdHex};

fn cache_files(world: &World, repo: openxet_index::RepoId) -> PathBuf {
    world.cache_dir().join("repos").join(repo.to_string())
}

fn mtime_secs(path: &Path) -> u64 {
    ok(ok(ok(fs::metadata(path)).modified()).duration_since(UNIX_EPOCH)).as_secs()
}

fn pack_size(pack: &common::PackFiles) -> u64 {
    pack.exts()
        .iter()
        .map(|e| ok(fs::metadata(pack.path(*e))).len())
        .sum()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in ok(fs::read_dir(&d)) {
            let path = ok(entry).path();
            if ok(fs::symlink_metadata(&path)).is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// Seeds `repo` with a pack in every state no snapshot may expose (ADR
/// 0004 item 4): pending, lost, expired and recovered.
async fn seed_invisible(
    world: &World,
    repo: openxet_index::RepoId,
    src: &Path,
    out: &Path,
) -> Vec<PackFiles> {
    let pending = pack_objects(src, "main~270\n^main~280", out);
    world.pending(repo, &pending, 160).await;
    let lost = pack_objects(src, "main~280\n^main~290", out);
    let row = world.pending(repo, &lost, 170).await;
    assert!(ok(world
        .index
        .mark_pack(row.id, PackState::Pending, PackState::Lost)
        .await));
    let expired = pack_objects(src, "main~290\n^main~295", out);
    let row = world.pending(repo, &expired, 180).await;
    assert!(ok(world
        .index
        .mark_pack(row.id, PackState::Pending, PackState::Expired)
        .await));
    let recovered = pack_objects(src, "main~295\n^main~299", out);
    world.upload(repo, &recovered).await;
    // reindex is the only writer of recovered packs (ADR 0004 item 4).
    ok(sqlx::query(
        "INSERT INTO packs (id, repo, name, state, created_at, bytes) \
         VALUES (?, ?, ?, 'recovered', 190, 1)",
    )
    .bind(&openxet_index::PackId::generate().0[..])
    .bind(&repo.0[..])
    .bind(&recovered.name)
    .execute(&world.sql().await)
    .await);
    vec![pending, lost, expired, recovered]
}

/// Copies `pack` into the node cache, as a node that hydrated it once would
/// hold it.
fn place_in_cache(world: &World, repo: openxet_index::RepoId, pack: &PackFiles) {
    let cached = cache_files(world, repo);
    ok(fs::create_dir_all(&cached));
    for ext in pack.exts() {
        ok(fs::copy(
            pack.path(ext),
            cached.join(format!("{}.{}", pack.name, ext.as_str())),
        ));
    }
}

fn sorted_names(packs: &[openxet_index::Pack]) -> Vec<String> {
    let mut names: Vec<String> = packs.iter().map(|p| p.name.clone()).collect();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn snapshot_exposes_only_its_pack_set() {
    let world = World::new().await;
    let fx = fixture("tf10k", 300);
    let src = fixture_repo(&fx);
    let out = world.dir.path().join("packs");
    let repo = world.repo("org", "model").await;
    let (t100, t200, tip, side) = (
        rev(&src, "main~200"),
        rev(&src, "main~100"),
        rev(&src, "main"),
        rev(&src, "main~250"),
    );
    let base = pack_objects(&src, "main~200", &out);
    let middle = pack_objects(&src, "main~100\n^main~200", &out);
    let top = pack_objects(&src, "main\n^main~100", &out);
    let side_pack = pack_objects(&src, "main~250\n^main~260", &out);
    let main = "refs/heads/main";
    world
        .commit(repo, &base, 100, &[set(main, None, t100)])
        .await;
    world
        .commit(repo, &middle, 200, &[set(main, Some(t100), t200)])
        .await;
    world
        .commit(repo, &top, 300, &[set(main, Some(t200), tip)])
        .await;
    let side_row = world
        .commit(repo, &side_pack, 150, &[set("refs/heads/side", None, side)])
        .await;
    let invisible = seed_invisible(&world, repo, &src, &out).await;
    let engine = world.engine().await;
    for pack in &invisible {
        place_in_cache(&world, repo, pack);
    }

    let first = ok(engine.snapshot(repo).await);
    let first_set = [&base, &middle, &top, &side_pack];
    assert_eq!(
        pack_dir_listing(first.dir.path()),
        expected_listing(&first_set)
    );
    assert!(!first.dir.path().join("objects/info/alternates").exists());
    let mut want: Vec<String> = first_set.iter().map(|p| p.name.clone()).collect();
    want.sort_unstable();
    assert_eq!(sorted_names(&first.packs), want);
    assert_eq!(
        first.refs,
        vec![(main.to_owned(), tip), ("refs/heads/side".to_owned(), side)]
    );

    // A geometric repack supersedes the side pack, and eviction drops it
    // from the cache.
    let repacked = pack_objects(&src, "main~240\n^main~260", &out);
    let repacked_row = world.pending(repo, &repacked, 400).await;
    let lease = ok(world
        .index
        .acquire_lease(repo, "node-a", Duration::from_mins(1))
        .await)
    .unwrap_or_else(|| panic!("lease"));
    ok(world
        .index
        .publish_repack(RepackPublish {
            repo,
            holder: "node-a",
            epoch: lease.epoch,
            outputs: &[repacked_row.id],
            inputs: &[side_row.id],
            ref_log_high_water: None,
        })
        .await);
    for ext in side_pack.exts() {
        let file = format!("{}.{}", side_pack.name, ext.as_str());
        ok(fs::remove_file(cache_files(&world, repo).join(file)));
    }

    let second = ok(engine.snapshot(repo).await);
    assert_eq!(
        pack_dir_listing(second.dir.path()),
        expected_listing(&[&base, &middle, &top, &repacked])
    );
    assert!(!second.dir.path().join("objects/info/alternates").exists());

    // The request in flight keeps the links it took.
    assert_eq!(
        pack_dir_listing(first.dir.path()),
        expected_listing(&first_set)
    );
    for ext in side_pack.exts() {
        let file = format!("{}.{}", side_pack.name, ext.as_str());
        let linked = first.dir.path().join("objects/pack").join(file);
        assert_eq!(ok(fs::read(linked)), ok(fs::read(side_pack.path(ext))));
    }
    git(first.dir.path(), &["cat-file", "-e", &side.to_string()]);
}

#[tokio::test]
async fn snapshot_serves_its_refs() {
    let world = World::new().await;
    let fx = fixture("tf10k", 300);
    let src = fixture_repo(&fx);
    let out = world.dir.path().join("packs");
    let repo = world.repo("org", "model").await;
    let (half, tip, tag) = (
        rev(&src, "main~150"),
        rev(&src, "main"),
        rev(&src, "refs/tags/v1.0"),
    );
    let a = pack_objects(&src, "main~150", &out);
    let b = pack_objects(&src, "refs/tags/v1.0\n^main~150", &out);
    world
        .commit(repo, &a, 100, &[set("refs/heads/main", None, half)])
        .await;
    world
        .commit(
            repo,
            &b,
            200,
            &[
                set("refs/heads/main", Some(half), tip),
                set("refs/tags/v1.0", None, tag),
            ],
        )
        .await;
    let engine = world.engine().await;

    let snap = ok(engine.snapshot(repo).await);
    let dir = snap.dir.path();
    assert_eq!(
        git(dir, &["for-each-ref", "--format=%(objectname) %(refname)"]),
        format!("{tip} refs/heads/main\n{tag} refs/tags/v1.0\n")
    );
    assert_eq!(git(dir, &["rev-parse", "HEAD"]).trim(), tip.to_string());
    assert_eq!(
        git(dir, &["rev-parse", "v1.0^{commit}"]).trim(),
        tip.to_string()
    );
    assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]).trim(), "300");
    git(dir, &["fsck", "--strict", "--no-dangling"]);
}

#[tokio::test]
async fn snapshot_dir_same_filesystem() {
    let world = World::new().await;
    let fx = fixture("tf10k", 20);
    let src = fixture_repo(&fx);
    let repo = world.repo("org", "model").await;
    let a = pack_objects(&src, "main", &world.dir.path().join("packs"));
    world
        .commit(
            repo,
            &a,
            100,
            &[set("refs/heads/main", None, rev(&src, "main"))],
        )
        .await;
    let engine = world.engine().await;

    let snap = ok(engine.snapshot(repo).await);
    let cached = cache_files(&world, repo).join(format!("{}.pack", a.name));
    let linked = snap
        .dir
        .path()
        .join("objects/pack")
        .join(format!("{}.pack", a.name));
    let (c, l) = (ok(fs::metadata(&cached)), ok(fs::metadata(&linked)));
    assert_eq!(
        (c.dev(), c.ino()),
        (l.dev(), l.ino()),
        "a hard link, not a copy"
    );
    assert_eq!(c.nlink(), 2);
    assert!(
        snap.dir
            .path()
            .starts_with(ok(fs::canonicalize(world.cache_dir())).join("snapshots"))
    );
    drop(snap);
    assert_eq!(ok(fs::metadata(&cached)).nlink(), 1);
    drop(engine);

    // A snapshot root where the probe link fails stops start-up with the
    // actionable error (ADR 0004 item 4).
    let blocked = World::new().await;
    let snapshots = blocked.cache_dir().join("snapshots");
    ok(fs::create_dir_all(&snapshots));
    ok(fs::set_permissions(
        &snapshots,
        fs::Permissions::from_mode(0o500),
    ));
    // Root ignores the mode; the probe then can't fail this way.
    if fs::write(snapshots.join("root-check"), b"").is_err() {
        let err = Engine::start(
            blocked.index.clone(),
            blocked.bucket.clone(),
            blocked.config(),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("start succeeded with an unlinkable snapshot root"));
        assert!(matches!(err, GitError::CacheFilesystem { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("hard link") && msg.contains("git.cache_dir"),
            "{msg}"
        );
    }
    ok(fs::set_permissions(
        &snapshots,
        fs::Permissions::from_mode(0o700),
    ));

    // A snapshot root on another filesystem is refused by `st_dev`, where
    // the host has one to offer (Linux's `/dev/shm`).
    let shm = Path::new("/dev/shm");
    if shm.is_dir()
        && ok(fs::metadata(shm)).dev() != ok(fs::metadata(blocked.dir.path())).dev()
        && let Ok(elsewhere) = tempfile::TempDir::new_in(shm)
    {
        let cross = World::new().await;
        ok(fs::create_dir_all(cross.cache_dir()));
        ok(std::os::unix::fs::symlink(
            elsewhere.path(),
            cross.cache_dir().join("snapshots"),
        ));
        let err = Engine::start(cross.index.clone(), cross.bucket.clone(), cross.config())
            .await
            .err()
            .unwrap_or_else(|| panic!("start succeeded across filesystems"));
        assert!(matches!(err, GitError::CacheFilesystem { .. }), "{err:?}");
    }
}

#[tokio::test]
async fn stale_snapshots_swept() {
    let world = World::new().await;
    let snapshots = world.cache_dir().join("snapshots");
    let leaked = snapshots.join("0192f5a3c1d47e6b8a9f0c1d2e3f4a5b");
    ok(fs::create_dir_all(leaked.join("objects/pack")));
    ok(fs::write(
        leaked.join("objects/pack/pack-x.pack"),
        b"leaked",
    ));
    ok(fs::write(snapshots.join(".probe-leaked"), b""));
    let repo = world.repo("org", "model").await;

    let engine = world.engine().await;
    assert_eq!(
        ok(fs::read_dir(&snapshots)).count(),
        0,
        "start-up sweeps leaked snapshots"
    );

    let snap = ok(engine.snapshot(repo).await);
    let name = ok(snap.dir.path().file_name().ok_or("no name"))
        .to_string_lossy()
        .into_owned();
    assert!(
        name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit()),
        "snapshot directories are named by a UUID: {name}"
    );
    assert_eq!(ok(fs::read_dir(&snapshots)).count(), 1);
    drop(snap);
    assert_eq!(
        ok(fs::read_dir(&snapshots)).count(),
        0,
        "a finished request leaves nothing"
    );
}

#[tokio::test]
async fn hydrated_pack_mtime_pinned() {
    let world = World::new().await;
    let fx = fixture("tf10k", 50);
    let src = fixture_repo(&fx);
    git(
        &src,
        &["-c", "pack.writeReverseIndex=true", "repack", "-adbq"],
    );
    let packs = files_under(&src.join("objects/pack"));
    let name = packs
        .iter()
        .find_map(|p| {
            let n = p.file_name()?.to_str()?;
            n.strip_suffix(".pack").map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("no pack in {packs:?}"));
    let pack = common::PackFiles {
        name,
        dir: src.join("objects/pack"),
    };
    assert_eq!(
        pack.exts(),
        [PackExt::Pack, PackExt::Idx, PackExt::Rev, PackExt::Bitmap],
        "the fixture pack carries every side file a node hydrates"
    );
    let created_at = 1_700_000_123;
    let repo = world.repo("org", "model").await;
    world
        .commit(
            repo,
            &pack,
            created_at,
            &[set("refs/heads/main", None, rev(&src, "main"))],
        )
        .await;
    let engine = world.engine().await;

    let snap = ok(engine.snapshot(repo).await);
    let cached = cache_files(&world, repo);
    for ext in pack.exts() {
        let file = format!("{}.{}", pack.name, ext.as_str());
        let path = cached.join(&file);
        assert_eq!(ok(fs::read(&path)), ok(fs::read(pack.path(ext))), "{file}");
        assert_eq!(mtime_secs(&path), created_at, "{file}");
        let linked = snap.dir.path().join("objects/pack").join(&file);
        assert_eq!(mtime_secs(&linked), created_at, "{file}");
    }
    drop(snap);

    // Anything that moves the mtime later (git freshening an object it
    // already has) is undone by the next snapshot.
    let pack_file = cached.join(format!("{}.pack", pack.name));
    ok(ok(fs::File::open(&pack_file)).set_modified(SystemTime::now()));
    assert_ne!(mtime_secs(&pack_file), created_at);
    let _again = ok(engine.snapshot(repo).await);
    assert_eq!(mtime_secs(&pack_file), created_at);
}

#[tokio::test]
async fn repo_paths_from_ids() {
    let world = World::new().await;
    let fx = fixture("tf10k", 20);
    let src = fixture_repo(&fx);
    let a = pack_objects(&src, "main", &world.dir.path().join("packs"));
    // The index stores names as written; only ids may reach a path.
    let repo = world.repo("../..", "../x").await;
    world
        .commit(
            repo,
            &a,
            100,
            &[set("refs/heads/main", None, rev(&src, "main"))],
        )
        .await;
    let engine = world.engine().await;

    let snap = ok(engine.snapshot(repo).await);
    let cache = ok(fs::canonicalize(world.cache_dir()));
    assert!(snap.dir.path().starts_with(cache.join("snapshots")));
    let repos: Vec<String> = ok(fs::read_dir(cache.join("repos")))
        .map(|e| ok(e).file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(repos, [repo.to_string()]);
    assert!(!world.dir.path().join("x").exists());
    assert!(
        !world
            .dir
            .path()
            .parent()
            .unwrap_or(world.dir.path())
            .join("x")
            .exists()
    );

    // A pack row whose name is a path is refused, not followed.
    let evil = world.repo("org", "evil").await;
    let row = openxet_index::Pack {
        id: openxet_index::PackId::generate(),
        repo: evil,
        name: "../../../outside".to_owned(),
        state: PackState::Pending,
        created_at: 1,
        bytes: 1,
        midx_gen: None,
    };
    ok(world.index.insert_pack(&row).await);
    let updates = [set("refs/heads/main", None, rev(&src, "main"))];
    ok(world
        .index
        .commit(openxet_index::CommitTx {
            repo: evil,
            updates: &updates,
            pack: Some(row.id),
            full_repack_gen: 0,
            actor: "tester",
            request: "test",
        })
        .await);
    let before = files_under(world.dir.path()).len();
    let err = engine
        .snapshot(evil)
        .await
        .err()
        .unwrap_or_else(|| panic!("a path-shaped pack name was followed"));
    assert!(matches!(err, GitError::Corrupt(_)), "{err:?}");
    assert!(!world.dir.path().join("outside").exists());
    assert_eq!(files_under(world.dir.path()).len(), before);
}

#[tokio::test]
async fn eviction_counts_links() {
    let world = World::new().await;
    let fx = fixture("tf10k", 40);
    let src = fixture_repo(&fx);
    let out = world.dir.path().join("packs");
    let a = pack_objects(&src, "main~20", &out);
    let b = pack_objects(&src, "main\n^main~20", &out);
    let held_repo = world.repo("org", "held").await;
    let free_repo = world.repo("org", "free").await;
    // The held pack is older, so only the link count can explain evicting
    // the free one first.
    world
        .commit(
            held_repo,
            &a,
            50,
            &[set("refs/heads/main", None, rev(&src, "main~20"))],
        )
        .await;
    world
        .commit(
            free_repo,
            &b,
            100,
            &[set("refs/heads/side", None, rev(&src, "main"))],
        )
        .await;
    let engine = world.engine().await;
    let size = pack_size;

    let held = ok(engine.snapshot(held_repo).await);
    drop(ok(engine.snapshot(free_repo).await));

    // Over budget by the free pack alone: it goes, and the held pack, which
    // no eviction could free now, stays.
    let report = ok(engine.evict(size(&a)).await);
    assert_eq!(report.freed_bytes, size(&b));
    assert_eq!(report.held_bytes, 0);
    assert!(
        !cache_files(&world, free_repo)
            .join(format!("{}.pack", b.name))
            .exists()
    );
    assert!(
        cache_files(&world, held_repo)
            .join(format!("{}.pack", a.name))
            .exists()
    );

    // Over budget with only a held pack left: its cache entry goes, but its
    // space isn't counted as freed while a snapshot links it.
    let report = ok(engine.evict(0).await);
    assert_eq!(report.freed_bytes, 0);
    assert_eq!(report.held_bytes, size(&a));
    assert!(
        !cache_files(&world, held_repo)
            .join(format!("{}.pack", a.name))
            .exists()
    );
    let linked = held
        .dir
        .path()
        .join("objects/pack")
        .join(format!("{}.pack", a.name));
    assert_eq!(ok(fs::read(linked)), ok(fs::read(a.path(PackExt::Pack))));
    git(held.dir.path(), &["rev-list", "--count", "HEAD"]);
}

/// The multi-pack index a geometric repack would write over `packs`, with
/// its bitmap and reverse index, written by git into a scratch repository.
fn write_midx(world: &World, packs: &[&PackFiles], tip: Oid) -> Vec<(MidxExt, PathBuf)> {
    let scratch = world.dir.path().join("midx.git");
    git(
        world.dir.path(),
        &["init", "-q", "--bare", &scratch.to_string_lossy()],
    );
    for pack in packs {
        for ext in pack.exts() {
            let file = format!("{}.{}", pack.name, ext.as_str());
            ok(fs::copy(
                pack.path(ext),
                scratch.join("objects/pack").join(file),
            ));
        }
    }
    git(
        &scratch,
        &["update-ref", "refs/heads/main", &tip.to_string()],
    );
    git(
        &scratch,
        &[
            "-c",
            "pack.writeReverseIndex=true",
            "multi-pack-index",
            "write",
            "--bitmap",
        ],
    );
    let files: Vec<(MidxExt, PathBuf)> = files_under(&scratch.join("objects/pack"))
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?;
            let ext = if name == "multi-pack-index" {
                MidxExt::MultiPackIndex
            } else {
                let ext = name.strip_prefix("multi-pack-index-")?.rsplit('.').next()?;
                [MidxExt::Bitmap, MidxExt::Rev]
                    .into_iter()
                    .find(|e| e.as_str() == ext)?
            };
            Some((ext, p))
        })
        .collect();
    assert!(
        files.iter().any(|(e, _)| *e == MidxExt::Bitmap),
        "git wrote a MIDX bitmap: {files:?}"
    );
    files
}

async fn upload_midx(world: &World, repo: openxet_index::RepoId, files: &[(MidxExt, PathBuf)]) {
    for (ext, path) in files {
        let key = Key::Midx {
            repo: ok(RepoIdHex::parse(&repo.to_string())),
            generation: 1,
            ext: *ext,
        };
        ok(world
            .bucket
            .put(&key, bytes::Bytes::from(ok(fs::read(path))))
            .await);
    }
}

#[tokio::test]
async fn midx_linked_only_when_covered() {
    let world = World::new().await;
    let fx = fixture("tf10k", 60);
    let src = fixture_repo(&fx);
    let out = world.dir.path().join("packs");
    let older = pack_objects(&src, "main~30", &out);
    let newer = pack_objects(&src, "main\n^main~30", &out);
    let (half, tip) = (rev(&src, "main~30"), rev(&src, "main"));
    let midx = write_midx(&world, &[&older, &newer], tip);

    let covered = world.repo("org", "covered").await;
    let main = "refs/heads/main";
    world
        .commit_with(covered, &older, 100, None, &[set(main, None, half)])
        .await;
    world
        .commit_with(covered, &newer, 200, Some(1), &[set(main, Some(half), tip)])
        .await;
    upload_midx(&world, covered, &midx).await;

    // The second repository commits only the newer pack: the MIDX covers a
    // pack outside its set, so linking it would expose that pack's objects.
    let partial = world.repo("org", "partial").await;
    world
        .commit_with(
            partial,
            &newer,
            200,
            Some(1),
            &[set("refs/heads/side", None, tip)],
        )
        .await;
    world.pending(partial, &older, 100).await;
    upload_midx(&world, partial, &midx).await;

    let engine = world.engine().await;
    let snap = ok(engine.snapshot(covered).await);
    let listing = pack_dir_listing(snap.dir.path());
    for (_, path) in &midx {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        assert!(
            listing.iter().any(|l| *l == name),
            "{name} missing from {listing:?}"
        );
    }
    git(snap.dir.path(), &["multi-pack-index", "verify"]);
    assert_eq!(
        git(snap.dir.path(), &["rev-list", "--count", "HEAD"]).trim(),
        "60"
    );

    let snap = ok(engine.snapshot(partial).await);
    assert_eq!(
        pack_dir_listing(snap.dir.path()),
        expected_listing(&[&newer])
    );
}

#[tokio::test]
async fn hydration_over_budget_evicts_but_spares_its_own_packs() {
    let world = World::new().await;
    let fx = fixture("tf10k", 40);
    let src = fixture_repo(&fx);
    let out = world.dir.path().join("packs");
    let a = pack_objects(&src, "main~20", &out);
    let b = pack_objects(&src, "main\n^main~20", &out);
    let older = world.repo("org", "older").await;
    let newer = world.repo("org", "newer").await;
    world
        .commit(
            older,
            &a,
            50,
            &[set("refs/heads/main", None, rev(&src, "main~20"))],
        )
        .await;
    world
        .commit(
            newer,
            &b,
            100,
            &[set("refs/heads/side", None, rev(&src, "main"))],
        )
        .await;
    let mut config = world.config();
    // Room for one pack: hydrating the second must evict the first.
    config.cache_max_bytes = pack_size(&a).max(pack_size(&b));
    let engine = ok(Engine::start(world.index.clone(), world.bucket.clone(), config).await);

    let first = ok(engine.snapshot(older).await);
    assert!(
        cache_files(&world, older)
            .join(format!("{}.pack", a.name))
            .exists()
    );
    let second = ok(engine.snapshot(newer).await);
    assert!(
        cache_files(&world, newer)
            .join(format!("{}.pack", b.name))
            .exists(),
        "a request never evicts the pack it just hydrated"
    );
    assert!(
        !cache_files(&world, older)
            .join(format!("{}.pack", a.name))
            .exists()
    );
    git(first.dir.path(), &["rev-list", "--count", "HEAD"]);
    drop(second);
}
