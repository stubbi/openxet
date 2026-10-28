"""Diagnostic, not a test: the gate's build-speed sequence (daemon started
before the first full builds, then 5 no-op and 5 incremental rounds),
repeated with the daemon on and off, with Cargo's per-unit timings and the
daemon's deduplicated bytes around each workspace build."""
import json, os, re, statistics, subprocess, sys, tempfile, time
from pathlib import Path

REPO = Path(sys.argv[1])
sys.path[:0] = [str(REPO / "compat"), str(REPO / "compat/tests")]
import test_workspaces as tw  # noqa: E402

volume = Path(sys.argv[2])
repeats = int(sys.argv[3])


def timed_build(p, checkout):
    started = time.perf_counter()
    out = subprocess.run(
        ["cargo", "build", "--offline", "--locked", "--message-format",
         "json-render-diagnostics", "--timings"],
        cwd=checkout, env=p.env, capture_output=True, stdin=subprocess.DEVNULL,
    )
    wall = time.perf_counter() - started
    assert out.returncode == 0, out.stderr.decode()[-2000:]
    html = (checkout / "target/cargo-timings/cargo-timing.html").read_text()
    units = json.loads(re.search(r"UNIT_DATA = (\[.*?\]);", html, re.S).group(1))
    compiled = [
        f"{u['name']}{u['target']}@{u['start']:.2f}+{u['duration']:.2f}"
        for u in units if u["duration"] > 0
    ]
    return wall, compiled


def run(mode: str, label: str):
    root = Path(tempfile.mkdtemp(dir=volume))
    home = Path(tempfile.mkdtemp())
    procs = tw.Processes()
    try:
        p = tw.Project(os.environ["OPENXET_BIN"], root, home / "home", procs)
        if mode == "on-state-elsewhere":
            # The registry off the measured volume, on the runner's own disk.
            p.env["OPENXET_WORKSPACE_STATE"] = str(home / "state")
        plain = root / "plain"
        p.git(root, "clone", "--quiet", "--local", str(p.repo), str(plain))
        ws = Path(p.create("timed")["path"])
        if mode in ("on", "on-state-elsewhere"):
            p.start_daemon()
        elif mode == "on-nice":
            log = (p.logs / "daemon.log").open("ab")
            procs.start(["nice", "-n", "19", p.openxet_bin, "workspace", "daemon"],
                        env=p.env, cwd="/", stdout=log, stderr=log)
            time.sleep(2)
            assert p.openxet("workspace", "status")["daemon_running"]
        elif mode == "inotifywait":
            procs.start(["inotifywait", "-m", "-r", "-q", "-e", "open,modify,close_write,create,delete,moved_to,moved_from,attrib", str(ws)],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(2)
        t0 = time.perf_counter()
        p.build(plain)
        p.build(ws)
        import sqlite3

        def dedup():
            db = sqlite3.connect(f"file:{root / 'state/registry.sqlite3'}?mode=ro", uri=True)
            try:
                return db.execute("select count(*) from canonical_files").fetchone()[0]
            finally:
                db.close()
        def cpu():
            if mode == "off" or not procs.children:
                return 0.0
            fields = Path(f"/proc/{procs.children[0].pid}/stat").read_text().rsplit(")", 1)[1].split()
            return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")

        for kind, prepare in (("no-op", lambda c: None), ("incremental", lambda c: os.utime(c / tw.TOUCHED))):
            t = {plain: [], ws: []}
            cpu_before = cpu()
            for i in range(5):
                order = (plain, ws) if i % 2 == 0 else (ws, plain)
                for c in order:
                    prepare(c)
                    before = dedup()
                    at = time.perf_counter() - t0
                    wall, compiled = timed_build(p, c)
                    after = dedup()
                    t[c].append(round(wall, 3))
                    side = "plain" if c == plain else "workspace"
                    print(f"{label} {kind} round {i} {side}: at {at:.2f}s wall {wall:.3f} "
                          f"hashed +{after - before} files {compiled}", flush=True)
            print(f"SUMMARY {label} {kind}: plain {t[plain]} workspace {t[ws]} ratio "
                  f"{statistics.median(t[ws]) / statistics.median(t[plain]):.3f} "
                  f"watcher cpu {cpu() - cpu_before:.2f}s", flush=True)
    finally:
        procs.stop_all()


for r in range(repeats):
    for mode in ("on", "on-state-elsewhere", "inotifywait", "off"):
        run(mode, f"{mode}#{r}")
