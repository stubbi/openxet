#!/usr/bin/env python3
"""Measures openxet serve's peak RSS under the memory attacks of ruling M1-R40.

Each scenario starts a fresh dev-mode `openxet serve`, attacks it from
CONNECTIONS sockets with the dev token for HOLD seconds, then stops it and
takes its peak RSS from its own rusage and, on macOS, its peak physical
footprint, which also counts the memory macOS compressed:
- `reconstruct`: every socket asks for the full reconstruction of a
  100k-term file whose terms all have their own fetch descriptor (a
  42.3 MB answer, the latency gate's worst case) and never reads it;
- `upload`: every socket posts a 64 MiB xorb, sends all of it but the last
  byte, and stalls.

The 100k-term file is registered by the ignored `memory_seed` test of
openxet-cas, which this script runs first. Run from the repository root,
after `cargo build --release -p openxet`:

    python3 scripts/measure-memory-bounds.py \
        --openxet "$PWD/target/release/openxet" --scenario reconstruct --connections 16

Prints one JSON object. Everything it writes goes to a temporary directory
that is removed at the end; the token is never printed.
"""

import argparse
import ctypes
import json
import os
import platform
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from urllib.parse import urlsplit

MiB = 1 << 20
XORB_BYTES = 64 * MiB

# A constant, so the repository's secret scanner, which flags any literal
# bearer value, can tell this from a leaked token.
SCHEME = "Bearer"


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


def seed(data: Path) -> dict:
    subprocess.run(
        [
            "cargo", "nextest", "run", "--release", "-p", "openxet-cas",
            "-E", "binary(memory_seed)", "--no-capture",
        ],
        env={**os.environ, "OPENXET_SEED_DIR": str(data)},
        check=True,
        capture_output=True,
    )
    return json.loads((data / "seeded.json").read_text())


def connect(endpoint: str) -> socket.socket:
    url = urlsplit(endpoint)
    sock = socket.create_connection((url.hostname, url.port))
    # A small receive buffer, so the answer waits in the server, not here.
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 64 * 1024)
    return sock


def reconstruct(endpoint: str, token: str, file: str) -> socket.socket:
    sock = connect(endpoint)
    sock.sendall(
        f"GET /v2/reconstructions/{file} HTTP/1.1\r\nHost: openxet\r\n"
        f"Authorization: {SCHEME} {token}\r\n\r\n".encode()
    )
    return sock


def upload(endpoint: str, token: str, index: int) -> socket.socket:
    sock = connect(endpoint)
    sock.sendall(
        f"POST /v1/xorbs/default/{index:064x} HTTP/1.1\r\nHost: openxet\r\n"
        f"Authorization: {SCHEME} {token}\r\nContent-Length: {XORB_BYTES}\r\n\r\n".encode()
    )
    block = bytes(MiB)
    try:
        for _ in range(XORB_BYTES // MiB - 1):
            sock.sendall(block)
        sock.sendall(block[:-1])
    except OSError:
        # The server answered and closed: its budget was full.
        pass
    return sock


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--openxet", required=True, help="the openxet binary (a release build)")
    parser.add_argument("--scenario", choices=["reconstruct", "upload"], required=True)
    parser.add_argument("--connections", type=int, default=16)
    parser.add_argument("--hold", type=float, default=20.0, help="seconds the attack lasts")
    parser.add_argument(
        "--memory-budget", help="server.memory_budget, such as 1GiB; the server's default if unset"
    )
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        data = work / "data"
        seeded = seed(data) if args.scenario == "reconstruct" else {}
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
            endpoint, token = serve["endpoint"], serve["token"]

            sockets: list[socket.socket] = []
            lock = threading.Lock()

            def attack(i: int) -> None:
                if args.scenario == "reconstruct":
                    sock = reconstruct(endpoint, token, seeded["unmerged"])
                else:
                    sock = upload(endpoint, token, i + 1)
                with lock:
                    sockets.append(sock)

            started = time.monotonic()
            threads = [threading.Thread(target=attack, args=(i,)) for i in range(args.connections)]
            for t in threads:
                t.start()
            for t in threads:
                t.join()
            time.sleep(max(0.0, args.hold - (time.monotonic() - started)))
            for sock in sockets:
                sock.close()
            footprint = peak_footprint(server.pid)
        finally:
            server.terminate()
            _, _, usage = os.wait4(server.pid, 0)

        # ru_maxrss is in bytes on macOS and KiB on Linux.
        rss = usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024)
        print(
            json.dumps(
                {
                    "machine": f"{platform.system()} {platform.machine()}, {os.cpu_count()} CPUs",
                    "scenario": args.scenario,
                    "connections": args.connections,
                    "memory_budget": args.memory_budget or "default",
                    "hold_seconds": args.hold,
                    "server_peak_rss_mib": round(rss / MiB),
                    "server_peak_footprint_mib": footprint and round(footprint / MiB),
                },
                indent=2,
            )
        )


if __name__ == "__main__":
    main()
