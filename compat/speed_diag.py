"""Diagnostic, not a test: incremental build times in a plain clone and a
workspace, with the daemon off, on, and off again, and Cargo's per-unit
timings for each build."""
import json, os, re, statistics, subprocess, sys, tempfile, time
from pathlib import Path

REPO = Path(sys.argv[1])
sys.path[:0] = [str(REPO / "compat"), str(REPO / "compat/tests")]
import test_workspaces as tw  # noqa: E402

volume = Path(sys.argv[2])
rounds = int(sys.argv[3])
root = Path(tempfile.mkdtemp(dir=volume))
home = Path(tempfile.mkdtemp())
procs = tw.Processes()


def timed_build(p, checkout):
    started = time.perf_counter()
    out = subprocess.run(
        ["cargo", "build", "--offline", "--locked", "--timings"],
        cwd=checkout, env=p.env, capture_output=True, stdin=subprocess.DEVNULL,
    )
    wall = time.perf_counter() - started
    assert out.returncode == 0, out.stderr.decode()[-2000:]
    html = (checkout / "target/cargo-timings/cargo-timing.html").read_text()
    units = json.loads(re.search(r"UNIT_DATA = (\[.*?\]);", html, re.S).group(1))
    duration = float(re.search(r"DURATION = ([0-9.]+)", html).group(1))
    compiled = [
        f"{u['name']}{u['target']}@{u['start']:.3f}+{u['duration']:.3f}"
        for u in units if u["duration"] > 0
    ]
    return round(wall, 3), duration, compiled


try:
    p = tw.Project(os.environ["OPENXET_BIN"], root, home / "home", procs)
    plain = root / "plain"
    p.git(root, "clone", "--quiet", "--local", str(p.repo), str(plain))
    ws = Path(p.create("timed")["path"])
    p.build(plain)
    p.build(ws)

    def phase(name):
        t = {plain: [], ws: []}
        for i in range(rounds):
            order = (plain, ws) if i % 2 == 0 else (ws, plain)
            for c in order:
                os.utime(c / tw.TOUCHED)
                wall, duration, compiled = timed_build(p, c)
                t[c].append(wall)
                side = "plain" if c == plain else "workspace"
                print(f"{name} round {i} {side}: wall {wall:.3f} cargo {duration} {compiled}", flush=True)
        print(
            f"SUMMARY {name}: plain {sorted(t[plain])} workspace {sorted(t[ws])} "
            f"ratio {statistics.median(t[ws]) / statistics.median(t[plain]):.3f}",
            flush=True,
        )

    phase("daemon off")
    p.start_daemon()
    phase("daemon on")
    procs.stop_all()
    procs.children.clear()
    phase("daemon off again")
finally:
    procs.stop_all()
