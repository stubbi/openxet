#!/usr/bin/env python3
"""Measures the storage-only rows of docs/competitive-matrix.md "openxet at M1".

Starts a dev-mode `openxet serve` on an empty data directory, then with
stock hf_xet (each run a fresh process and Xet cache):
- uploads SIZE MiB of incompressible noise and downloads it again, timing
  both and checking the bytes;
- asks for a 1000-byte range in the middle of the file and sums the bytes
  its reconstruction plans to fetch;
- sums what the bucket and the index hold, for storage overhead;
- takes the server's peak RSS from its own rusage after it stops;
- counts the `503 memory_busy` and other `5xx` answers in the server log.

Run from the repository root, after `cargo build --release -p openxet`:

    uv run --frozen --directory compat python ../scripts/measure-storage-only.py \
        --openxet "$PWD/target/release/openxet" --size-mib 1024

Prints one JSON object. Everything it writes goes to a temporary directory
that is removed at the end; the token is never printed.
"""

import argparse
import ctypes
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

UPLOAD = """
import json, sys, time, hf_xet
s = json.load(open(sys.argv[1]))
t = time.monotonic()
with hf_xet.XetSession().new_upload_commit(endpoint=s["endpoint"], token=s["token"], token_expiry_unix_secs=s["token_exp"]) as c:
    u = c.start_upload_file(sys.argv[2])
i = u.result().xet_info
print(json.dumps({"seconds": time.monotonic() - t, "hash": i.hash, "size": i.file_size}))
"""

DOWNLOAD = """
import json, sys, time, hf_xet
s = json.load(open(sys.argv[1]))
t = time.monotonic()
with hf_xet.XetSession().new_file_download_group(endpoint=s["endpoint"], token=s["token"], token_expiry_unix_secs=s["token_exp"]) as g:
    g.start_download_file(hf_xet.XetFileInfo(sys.argv[2], int(sys.argv[3])), sys.argv[4])
print(json.dumps({"seconds": time.monotonic() - t}))
"""

MiB = 1 << 20

# A constant, so the repository's secret scanner, which flags any literal
# bearer value, can tell this from a leaked token.
SCHEME = "Bearer"


def client(work: Path, name: str, script: str, *args: str) -> dict:
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(work / name),
        "HF_XET_CACHE": str(work / name / "xet"),
    }
    out = subprocess.run(
        [sys.executable, "-c", script, *args],
        env=env,
        capture_output=True,
        text=True,
        check=True,
        stdin=subprocess.DEVNULL,
    )
    return json.loads(out.stdout)


def peak_footprint(pid: int) -> int | None:
    """macOS only: the process's lifetime peak physical footprint, which,
    unlike RSS, counts pages the kernel compressed; None elsewhere."""
    if platform.system() != "Darwin":
        return None
    libc = ctypes.CDLL("libc.dylib")
    info = (ctypes.c_uint64 * 64)()
    # proc_pid_rusage(pid, RUSAGE_INFO_V4, &info): a 16-byte uuid, then
    # u64 fields; ri_lifetime_max_phys_footprint is the 29th.
    if libc.proc_pid_rusage(pid, 4, ctypes.byref(info)) != 0:
        return None
    return info[2 + 28]


def tree_bytes(path: Path) -> int:
    return sum(f.stat().st_size for f in path.rglob("*") if f.is_file())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--openxet", required=True, help="the openxet binary (a release build)")
    parser.add_argument("--size-mib", type=int, default=1024)
    parser.add_argument(
        "--memory-budget", help="server.memory_budget, such as 1GiB; the server's default if unset"
    )
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        data = work / "data"
        ready = work / "serve.json"
        with ready.open("w") as out, (work / "serve.log").open("w") as log:
            server = subprocess.Popen(
                [args.openxet, "serve", "--data-dir", str(data), "--port", "0", "--json"],
                env={
                    "PATH": os.environ["PATH"],
                    "HOME": str(work / "server-home"),
                    **({"OPENXET_SERVER__MEMORY_BUDGET": args.memory_budget} if args.memory_budget else {}),
                },
                stdout=out,
                stderr=log,
                stdin=subprocess.DEVNULL,
            )
        try:
            while not ready.read_text().strip():
                assert server.poll() is None, "openxet serve exited; see its log"
                time.sleep(0.05)
            serve = json.loads(ready.read_text())

            source = work / "noise.bin"
            with source.open("wb") as f:
                for _ in range(args.size_mib):
                    f.write(os.urandom(MiB))
            up = client(work, "uploader", UPLOAD, str(ready), str(source))
            dest = work / "noise.out"
            down = client(work, "downloader", DOWNLOAD, str(ready), up["hash"], str(up["size"]), str(dest))
            identical = subprocess.run(["cmp", "-s", str(source), str(dest)]).returncode == 0
            dest.unlink()

            middle = up["size"] // 2
            request = urllib.request.Request(
                f"{serve['endpoint']}/v2/reconstructions/{up['hash']}",
                headers={
                    "Authorization": f"{SCHEME} {serve['token']}",
                    "Range": f"bytes={middle}-{middle + 999}",
                },
            )
            with urllib.request.urlopen(request) as answer:
                plan = json.load(answer)
            range_fetch = sum(
                r["bytes"]["end"] - r["bytes"]["start"] + 1
                for entries in plan["xorbs"].values()
                for entry in entries
                for r in entry["ranges"]
            )
            footprint = peak_footprint(server.pid)
        finally:
            server.terminate()
            _, _, usage = os.wait4(server.pid, 0)

        log_text = (work / "serve.log").read_text()
        xorbs = tree_bytes(data / "bucket" / "xorbs")
        shards = tree_bytes(data / "bucket") - xorbs
        index = sum(f.stat().st_size for f in data.glob("index.sqlite*"))
        # ru_maxrss is in bytes on macOS and KiB on Linux.
        rss = usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024)
        size = up["size"]
        print(
            json.dumps(
                {
                    "machine": f"{platform.system()} {platform.machine()}, {os.cpu_count()} CPUs",
                    "file_bytes": size,
                    "identical": identical,
                    "upload_seconds": round(up["seconds"], 2),
                    "download_seconds": round(down["seconds"], 2),
                    "upload_mb_per_s": round(size / up["seconds"] / 1e6),
                    "download_mb_per_s": round(size / down["seconds"] / 1e6),
                    "range_1000_bytes_fetch_bytes": range_fetch,
                    "stored_xorb_bytes": xorbs,
                    "stored_shard_bytes": shards,
                    "index_bytes": index,
                    "overhead_percent": round((xorbs + shards + index - size) / size * 100, 2),
                    "server_peak_rss_mib": round(rss / MiB),
                    "server_peak_footprint_mib": footprint and round(footprint / MiB),
                    # Refusals for a full memory budget (ruling M1-R40), and
                    # other 5xx answers; each makes hf_xet back off and retry.
                    "server_memory_busy": log_text.count("memory_busy"),
                    "server_5xx": log_text.count("request failed"),
                },
                indent=2,
            )
        )


if __name__ == "__main__":
    main()
