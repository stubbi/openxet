"""The flat-disk and build-speed gate for agent workspaces.

Spec 2026-10-27-agent-workspaces-design.md §2: at the peak of six concurrent
builds, each workspace beyond the first adds at most 10% of one build's
footprint, used bytes return to where they were once the workspaces are
removed, and a build in a workspace is at most 10% slower than in a plain
clone.

The project is `compat/fixtures/workspace-project`, built offline against
its vendored crates. Those stay where they are, outside every checkout, as
`~/.cargo/registry` would be; only the project's own files are committed to
the test repository the workspaces are made from.

Footprint is the used bytes of the volume (statvfs), not `du`: clones share
their blocks, and `du` counts every clone in full. Point
OPENXET_COMPAT_WORKSPACE_VOLUME at a directory on a volume nothing else
writes to (CI mounts a fresh XFS loop volume there); without it the tests
run under pytest's tmp_path, where other processes' writes land in the
numbers.
"""

import contextlib
import json
import os
import shutil
import signal
import statistics
import subprocess
import tempfile
import threading
import time
import tomllib
from pathlib import Path

import pytest

from conftest import isolated_env

pytestmark = pytest.mark.workspaces

REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "compat/fixtures/workspace-project"
VOLUME_ENV = "OPENXET_COMPAT_WORKSPACE_VOLUME"

WORKSPACES = 6
# Spec §2: each workspace beyond the first adds at most 10% of one build.
EXTRA_SHARE = 0.10
# Used bytes after removing every workspace, relative to the baseline.
RETURN_TOLERANCE = 0.01
SLOWDOWN = 1.10
ROUNDS = 5
SAMPLE_EVERY = 0.2
BUILD_TIMEOUT = 1800
# Unlinked files' blocks are freed in the background on APFS and XFS.
FREE_PATIENCE = 60.0
TOUCHED = "crates/model/src/lib.rs"

GIT_IDENTITY = dict(
    GIT_CONFIG_GLOBAL=os.devnull,
    GIT_CONFIG_NOSYSTEM="1",
    GIT_AUTHOR_NAME="agent",
    GIT_AUTHOR_EMAIL="agent@example.com",
    GIT_COMMITTER_NAME="agent",
    GIT_COMMITTER_EMAIL="agent@example.com",
)


def used_bytes(path: Path) -> int:
    stat = os.statvfs(path)
    return (stat.f_blocks - stat.f_bfree) * stat.f_frsize


class Peak:
    """The highest used bytes on `path`'s volume while the block runs,
    sampled every SAMPLE_EVERY seconds."""

    def __init__(self, path: Path):
        self.path = path
        self.peak = used_bytes(path)
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._sample, daemon=True)

    def _sample(self):
        while not self._stop.is_set():
            self.peak = max(self.peak, used_bytes(self.path))
            self._stop.wait(SAMPLE_EVERY)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        self._thread.join()
        self.peak = max(self.peak, used_bytes(self.path))


class Processes:
    """Every child the test starts, each in its own process group, so the
    finalizer can stop it and everything it spawned (rustc, build scripts)."""

    def __init__(self):
        self.children: list[subprocess.Popen] = []

    def start(self, args, **kwargs) -> subprocess.Popen:
        child = subprocess.Popen(args, start_new_session=True, stdin=subprocess.DEVNULL, **kwargs)
        self.children.append(child)
        return child

    def stop_all(self):
        for child in self.children:
            if child.poll() is None:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    os.killpg(child.pid, signal.SIGTERM)
        deadline = time.monotonic() + 10
        for child in self.children:
            try:
                child.wait(timeout=max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
            # The group outlives its leader when a rustc it started is still
            # running.
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(child.pid, signal.SIGKILL)
            child.wait()


@pytest.fixture
def processes():
    tracked = Processes()
    yield tracked
    tracked.stop_all()


@pytest.fixture
def volume_dir(tmp_path):
    """A fresh directory on the measured volume, removed afterwards."""
    root = os.environ.get(VOLUME_ENV)
    if not root:
        yield tmp_path / "volume"
        return
    made = Path(tempfile.mkdtemp(prefix="workspaces-", dir=root))
    yield made
    shutil.rmtree(made, ignore_errors=True)


class Project:
    """The fixture as a Git repository with a pushed main branch, an
    isolated OpenXet state and config, and Cargo pointed at the vendored
    crates."""

    def __init__(self, openxet_bin: str, root: Path, home: Path, processes: Processes):
        self.openxet_bin = openxet_bin
        self.root = root
        self.processes = processes
        self.logs = home / "logs"
        self.logs.mkdir(parents=True)
        cargo_home = home / "cargo"
        cargo_home.mkdir()
        (cargo_home / "config.toml").write_text(
            '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
            f'[source.vendored-sources]\ndirectory = "{FIXTURE / "vendor"}"\n'
        )
        toolchain = tomllib.loads((REPO / "rust-toolchain.toml").read_text())["toolchain"]
        self.env = isolated_env(
            home,
            OPENXET_WORKSPACE_STATE=str(root / "state"),
            # Started by the test, so it can be stopped by it.
            OPENXET_WORKSPACE_DAEMON="off",
            XDG_CONFIG_HOME=str(home / "config"),
            CARGO_HOME=str(cargo_home),
            RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")),
            RUSTUP_TOOLCHAIN=toolchain["channel"],
            CARGO_TERM_COLOR="never",
            **GIT_IDENTITY,
        )
        config = home / "config" / "openxet" / "workspace.toml"
        config.parent.mkdir(parents=True)
        # The default floor (20 GiB) is more than CI's whole volume.
        config.write_text('floor = "1 GiB"\n')

        self.repo = root / "project"
        shutil.copytree(FIXTURE, self.repo, ignore=shutil.ignore_patterns("vendor", ".cargo", "target"))
        origin = root / "origin.git"
        self.git(root, "init", "--quiet", "--bare", "-b", "main", str(origin))
        self.git(root, "init", "--quiet", "-b", "main", str(self.repo))
        self.git(self.repo, "add", ".")
        self.git(self.repo, "commit", "--quiet", "-m", "fixture")
        self.git(self.repo, "remote", "add", "origin", str(origin))
        self.git(self.repo, "push", "--quiet", "-u", "origin", "main")

    def git(self, cwd: Path, *args: str):
        subprocess.run(["git", *args], cwd=cwd, env=self.env, check=True, capture_output=True)

    def openxet(self, *args: str) -> dict:
        out = subprocess.run(
            [self.openxet_bin, *args, "--json"],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            stdin=subprocess.DEVNULL,
            timeout=120,
        )
        assert out.returncode == 0, out.stderr.decode()
        return json.loads(out.stdout)

    def create(self, name: str) -> dict:
        return self.openxet(
            "workspace", "create", "--name", name, "--path", str(self.root / "worktrees" / name)
        )

    def remove(self, path: str) -> dict:
        return self.openxet("workspace", "remove", path)

    def start_daemon(self):
        log = (self.logs / "daemon.log").open("ab")
        self.processes.start(
            [self.openxet_bin, "workspace", "daemon"], env=self.env, cwd="/", stdout=log, stderr=log
        )
        deadline = time.monotonic() + 30
        while not self.openxet("workspace", "status")["daemon_running"]:
            assert time.monotonic() < deadline, (self.logs / "daemon.log").read_text()
            time.sleep(0.2)

    def start_build(self, checkout: Path) -> subprocess.Popen:
        log = (self.logs / f"build-{checkout.name}.log").open("ab")
        return self.processes.start(
            # JSON on stdout names each unit and whether it was fresh;
            # diagnostics stay human on stderr.
            ["cargo", "build", "--offline", "--locked", "--message-format", "json-render-diagnostics"],
            cwd=checkout,
            env=self.env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )

    def wait(self, build: subprocess.Popen, checkout: Path):
        assert build.wait(timeout=BUILD_TIMEOUT) == 0, (
            self.logs / f"build-{checkout.name}.log"
        ).read_text()[-4000:]

    def compiled(self, checkout: Path) -> list[str]:
        """The units Cargo compiled, rather than found fresh, in every build
        so far in `checkout`."""
        units = []
        for line in (self.logs / f"build-{checkout.name}.log").read_text().splitlines():
            if not line.startswith("{"):
                continue
            message = json.loads(line)
            if message.get("reason") == "compiler-artifact" and not message["fresh"]:
                # `registry+https://...#serde@1.0.229` or `path+file://...#model@0.1.0`
                package = message["package_id"].rpartition("#")[2].partition("@")[0]
                build_script = " (build script)" if "custom-build" in message["target"]["kind"] else ""
                units.append(package + build_script)
        return units

    def build(self, checkout: Path) -> float:
        """Wall-clock seconds of one `cargo build` in `checkout`."""
        started = time.perf_counter()
        self.wait(self.start_build(checkout), checkout)
        return time.perf_counter() - started


def mib(n: float) -> str:
    return f"{n / (1 << 20):.1f} MiB"


def settled_used(path: Path, target: int, tolerance: int) -> int:
    """Used bytes once they come within `tolerance` of `target`, or after
    FREE_PATIENCE, whichever is first."""
    deadline = time.monotonic() + FREE_PATIENCE
    while True:
        os.sync()
        now = used_bytes(path)
        if abs(now - target) <= tolerance or time.monotonic() > deadline:
            return now
        time.sleep(1)


@pytest.fixture
def project(openxet_bin, volume_dir, tmp_path, processes):
    volume_dir.mkdir(exist_ok=True)
    return Project(openxet_bin, volume_dir, tmp_path / "home", processes)


def test_six_concurrent_builds_stay_almost_flat(project):
    volume = project.root
    # The registry (SQLite and its WAL) is made on first use and stays, as it
    # does on any machine that uses workspaces; the claim is that removing
    # the workspaces leaves none of their data behind, not that OpenXet
    # keeps no state. So it exists before the baseline.
    project.openxet("workspace", "status")
    baseline = used_bytes(volume)

    first = project.create("ws1")
    before_solo = used_bytes(volume)
    with Peak(volume) as solo:
        project.build(Path(first["path"]))
    footprint = solo.peak - before_solo
    assert footprint > 0, "the solo build left no trace in the volume's used bytes"

    others = [project.create(f"ws{i}") for i in range(2, WORKSPACES + 1)]
    for created in others:
        assert created["seed_skipped"] is None and created["seeded_bytes"] > 0, (
            f"{created['path']} was not seeded ({created['seed_skipped']}): the volume at "
            f"{volume} must support clones (APFS, Btrfs, or XFS made with reflink=1)"
        )
    project.start_daemon()
    checkouts = [Path(created["path"]) for created in [first, *others]]
    with Peak(volume) as concurrent:
        builds = [(project.start_build(checkout), checkout) for checkout in checkouts]
        for build, checkout in builds:
            project.wait(build, checkout)
    grown = concurrent.peak - baseline

    for checkout in checkouts:
        project.remove(str(checkout))
    tolerance = int(baseline * RETURN_TOLERANCE)
    after = settled_used(volume, baseline, tolerance)

    budget = (1 + (WORKSPACES - 1) * EXTRA_SHARE) * footprint
    waits = ", ".join(f"{c['name']} {c['seed_waited_ms']} ms" for c in others)
    units = "\n".join(
        f"  {checkout.name}: {len(compiled)} compiled {compiled}"
        for checkout in checkouts
        for compiled in [project.compiled(checkout)]
    )
    print(f"\nworkspaces gate: seeding waited {waits}\nunits compiled per workspace:\n{units}")
    print(
        f"\nworkspaces gate: solo build {mib(footprint)}, peak of {WORKSPACES} concurrent builds "
        f"{mib(grown)} over the baseline ({grown / footprint:.3f} of one build, budget "
        f"{budget / footprint:.2f}), {mib(after - baseline)} left after removal "
        f"(baseline {mib(baseline)}, tolerance {mib(tolerance)})"
    )
    assert grown <= budget, (
        f"the peak of {WORKSPACES} concurrent builds grew used bytes by {mib(grown)}, over "
        f"{mib(budget)}: 1 + {WORKSPACES - 1} x {EXTRA_SHARE} of one build's {mib(footprint)}; "
        f"units compiled per workspace:\n{units}"
    )
    assert abs(after - baseline) <= tolerance, (
        f"after removing every workspace, used bytes are {mib(after - baseline)} off the "
        f"baseline of {mib(baseline)}, more than {RETURN_TOLERANCE:.0%}"
    )


def test_build_in_a_workspace_is_at_most_ten_percent_slower(project):
    plain = project.root / "plain"
    project.git(project.root, "clone", "--quiet", "--local", str(project.repo), str(plain))
    workspace = Path(project.create("timed")["path"])
    project.start_daemon()
    project.build(plain)
    project.build(workspace)

    def timed(prepare) -> tuple[list[float], list[float]]:
        times = {plain: [], workspace: []}
        for round_ in range(ROUNDS):
            # Alternating which goes first keeps a warm cache from favouring
            # one side.
            order = (plain, workspace) if round_ % 2 == 0 else (workspace, plain)
            for checkout in order:
                prepare(checkout)
                times[checkout].append(project.build(checkout))
        return times[plain], times[workspace]

    def touch(checkout: Path):
        os.utime(checkout / TOUCHED)

    results = {
        "no-op": timed(lambda checkout: None),
        "incremental": timed(touch),
    }
    ratios = {}
    for kind, (plain_times, workspace_times) in results.items():
        ratios[kind] = statistics.median(workspace_times) / statistics.median(plain_times)
        print(
            f"\nworkspaces gate: {kind} build, median of {ROUNDS}: plain clone "
            f"{statistics.median(plain_times):.3f} s {plain_times}, workspace "
            f"{statistics.median(workspace_times):.3f} s {workspace_times}, "
            f"ratio {ratios[kind]:.3f}"
        )
    for kind, ratio in ratios.items():
        assert ratio <= SLOWDOWN, (
            f"a {kind} build in a workspace took {ratio:.3f} times as long as in a plain clone "
            f"(median of {ROUNDS}), over {SLOWDOWN}"
        )
