#!/usr/bin/env python3
"""Probe storage building blocks with disposable synthetic data, not OpenXet features."""

import argparse
import ctypes
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import threading


def run(args, cwd=None, data=None):
    env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    return subprocess.run(
        [str(arg) for arg in args], cwd=cwd, input=data, capture_output=True,
        check=True, timeout=45, env=env,
    ).stdout


def git(root, *args, data=None):
    return run(["git", "-C", root, *args], data=data)


def git_restore(root):
    source, work, restored = root / "source", root / "work", root / "restored"
    run(["git", "init", "-q", "-b", "main", source])
    git(source, "config", "user.name", "OpenXet probe")
    git(source, "config", "user.email", "probe@example.invalid")
    (source / "state.txt").write_text("base\n")
    (source / "binary.bin").write_bytes(bytes(range(256)))
    (source / ".gitignore").write_text("*.keep\n")
    (source / "run.sh").write_text("#!/bin/sh\nexit 0\n")
    (source / "run.sh").chmod(0o755)
    (source / "link").symlink_to("state.txt")
    git(source, "add", ".")
    git(source, "commit", "-q", "-m", "base")
    git(source, "worktree", "add", "-q", "-b", "agent/probe", work)
    (work / "committed.txt").write_text("unpublished commit\n")
    git(work, "add", "committed.txt")
    git(work, "commit", "-q", "-m", "unpublished")
    (work / "state.txt").write_text("staged\n")
    (work / "binary.bin").write_bytes(bytes(reversed(range(256))))
    git(work, "add", "state.txt", "binary.bin")
    (work / "state.txt").write_text("unstaged\n")
    (work / "run.sh").chmod(0o644)
    (work / "untracked.txt").write_text("untracked work\n")
    (work / "selected.keep").write_text("selected ignored work\n")

    expected_head = git(work, "rev-parse", "HEAD")
    expected_index = git(work, "ls-files", "--stage", "-z")
    expected_status = git(work, "status", "--porcelain=v1", "-z", "--ignored")
    staged = git(work, "diff", "--cached", "--binary", "--full-index")
    unstaged = git(work, "diff", "--binary", "--full-index")
    bundle = root / "snapshot.bundle"
    git(work, "bundle", "create", bundle, "HEAD")
    git(work, "bundle", "verify", bundle)

    run(["git", "init", "-q", restored])
    git(restored, "fetch", "-q", bundle, "HEAD")
    git(restored, "checkout", "-q", "-b", "agent/probe", "FETCH_HEAD")
    git(restored, "apply", "--index", "--binary", "-", data=staged)
    git(restored, "apply", "--binary", "-", data=unstaged)
    for name in ["untracked.txt", "selected.keep"]:
        shutil.copy2(work / name, restored / name)

    assert (work / ".git").is_file()
    assert git(restored, "rev-parse", "HEAD") == expected_head
    assert git(restored, "symbolic-ref", "--short", "HEAD").strip() == b"agent/probe"
    assert git(restored, "ls-files", "--stage", "-z") == expected_index
    assert git(restored, "status", "--porcelain=v1", "-z", "--ignored") == expected_status
    assert git(restored, "diff", "--cached", "--binary", "--full-index") == staged
    assert git(restored, "diff", "--binary", "--full-index") == unstaged
    assert os.readlink(restored / "link") == "state.txt"
    for name in ["state.txt", "binary.bin", "run.sh", "untracked.txt", "selected.keep"]:
        assert (restored / name).read_bytes() == (work / name).read_bytes()
    assert (restored / "run.sh").stat().st_mode & 0o777 == 0o644
    return {
        "status": "passed", "fixture": "linked worktree restored into an independent repository",
        "preserved": ["unpushed commit", "branch", "index", "staged binary change",
                      "unstaged edit", "untracked file", "selected ignored file", "mode", "symlink"],
        "scope": "same-host file-state round trip; no crash, live-writer or cross-OS proof",
    }


def clone_isolation(root):
    if platform.system() != "Darwin":
        return {"status": "not_run", "reason": "this probe exercises the macOS clonefile API"}
    source, cloned = root / "base.bin", root / "clone.bin"
    source.write_bytes(bytes(range(256)) * 32768)
    original = hashlib.sha256(source.read_bytes()).hexdigest()
    lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    lib.clonefile.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int]
    lib.clonefile.restype = ctypes.c_int
    result = lib.clonefile(os.fsencode(source), os.fsencode(cloned), 0)
    if result:
        raise OSError(ctypes.get_errno(), "clonefile failed")
    assert source.stat().st_ino != cloned.stat().st_ino
    assert hashlib.sha256(cloned.read_bytes()).hexdigest() == original
    with cloned.open("r+b") as handle:
        handle.write(b"private write" * 256)
    assert hashlib.sha256(source.read_bytes()).hexdigest() == original
    assert hashlib.sha256(cloned.read_bytes()).hexdigest() != original
    return {
        "status": "passed", "source_bytes": source.stat().st_size,
        "distinct_inodes": True, "writes_are_isolated": True,
        "scope": "single-file API behavior; no allocation ratio or directory-performance claim",
    }


def selective_parquet(root):
    duckdb = shutil.which("duckdb")
    if duckdb is None:
        return {"status": "not_run", "reason": "DuckDB CLI and its httpfs extension are required"}
    parquet = root / "fixture.parquet"
    path = str(parquet).replace("'", "''")
    setup = "SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; "
    command = [duckdb, "-init", os.devnull, ":memory:"]
    run([*command, "-c", setup + (
        "COPY (SELECT i AS id, repeat(md5(i::VARCHAR), 8) AS payload, "
        "i % 10 AS category FROM range(200000) t(i)) "
        f"TO '{path}' (FORMAT PARQUET, ROW_GROUP_SIZE 2048, COMPRESSION ZSTD)"
    )])
    reads = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_HEAD(self):
            self.reply(False)

        def do_GET(self):
            self.reply(True)

        def reply(self, send_body):
            if self.path != "/fixture.parquet":
                self.send_error(404)
                return
            size = parquet.stat().st_size
            start, end, status = 0, size - 1, 200
            raw = self.headers.get("Range")
            if raw:
                match = re.fullmatch(r"bytes=(\d*)-(\d*)", raw)
                if match is None or not any(match.groups()):
                    self.send_error(400)
                    return
                left, right = match.groups()
                if left:
                    start = int(left)
                    end = min(int(right), end) if right else end
                else:
                    start = max(0, size - int(right))
                if start > end:
                    self.send_error(416)
                    return
                status = 206
            self.send_response(status)
            self.send_header("Content-Length", str(end - start + 1))
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("ETag", '"fixed-fixture"')
            if status == 206:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.end_headers()
            if send_body:
                reads.append({"status": status, "start": start, "end": end})
                with parquet.open("rb") as handle:
                    handle.seek(start)
                    self.wfile.write(handle.read(end - start + 1))

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        url = f"http://127.0.0.1:{server.server_port}/fixture.parquet"
        sql = setup + (
            "LOAD httpfs; SELECT count(*) AS rows, sum(id) AS checksum "
            f"FROM read_parquet('{url}') WHERE id BETWEEN 8192 AND 12287"
        )
        answer = json.loads(run([*command, "-json", "-c", sql]))
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    assert answer == [{"rows": 4096, "checksum": sum(range(8192, 12288))}], answer
    transferred = sum(r["end"] - r["start"] + 1 for r in reads)
    assert reads and all(r["status"] == 206 for r in reads), reads
    assert transferred < parquet.stat().st_size, (transferred, parquet.stat().st_size)
    return {
        "status": "passed", "duckdb": run([duckdb, "--version"]).decode().strip(),
        "fixture_rows": 200000, "result_rows": 4096,
        "parquet_bytes": parquet.stat().st_size, "http_body_bytes": transferred,
        "range_gets": len(reads),
        "scope": "DuckDB over a synthetic HTTP range server, not OpenXet or a mount",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="openxet-storage-probe-") as temp:
        root = Path(temp)
        result = {
            "schema": "openxet.feasibility-probes.v1",
            "environment": {"os": platform.system(), "macos": platform.mac_ver()[0],
                            "python": platform.python_version(),
                            "git": run(["git", "--version"]).decode().strip()},
            "git_restore": git_restore(root),
            "clone_isolation": clone_isolation(root),
            "selective_parquet": selective_parquet(root),
        }
    encoded = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded)
    print(encoded, end="")


if __name__ == "__main__":
    main()
