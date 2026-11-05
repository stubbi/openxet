import os, statistics, signal
from pathlib import Path
import pytest
from test_workspaces import project, processes, volume_dir, TOUCHED  # noqa
pytestmark = pytest.mark.workspaces

def test_scratch(project, processes):
    plain = project.root / "plain"
    project.git(project.root, "clone", "--quiet", "--local", str(project.repo), str(plain))
    ws = Path(project.create("timed")["path"])
    project.build(plain); project.build(ws)
    def run(label):
        for kind, prep in (("noop", lambda c: None), ("incr", lambda c: os.utime(c / TOUCHED))):
            t = {plain: [], ws: []}
            for r in range(15):
                for c in ((plain, ws) if r % 2 == 0 else (ws, plain)):
                    prep(c); t[c].append(project.build(c))
            print(f"\n{label} {kind}: plain {statistics.median(t[plain]):.4f} ws {statistics.median(t[ws]):.4f} ratio {statistics.median(t[ws])/statistics.median(t[plain]):.3f}")
    run("no daemon")
    project.start_daemon()
    import time; time.sleep(5)
    run("daemon")
