"""Captures the exact bytes stock `hf_xet` uploads to a CAS server.

Writes `testdata/hf-xet-1.7.0-upload/`: every xorb body (`<hash>.xorb`) and
the shard body (`upload.shard`) that `hf_xet` sends for three deterministic
files. The protocol crate's tests verify the shard against the xorb, so a
regression in openxet's shard checks against a real client shows up there.

Run from the repository root, with the client versions pinned in
`compat/uv.lock`:

    uv run --frozen --directory compat python -I ../scripts/capture-hf-xet-upload.py

The server below answers just enough of the CAS API
(https://huggingface.co/docs/xet/api) for an upload: global dedup lookups and
`/v2/shards` get 404 (so the client falls back to `/v1/shards`), and xorb and
shard uploads succeed. The inputs are seeded, and the client's caches point at
a fresh temporary directory, so a rerun reproduces the same bytes.
"""

import json
import os
import random
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

OUT = Path(__file__).resolve().parents[1] / "testdata" / "hf-xet-1.7.0-upload"


class CasStub(BaseHTTPRequestHandler):
    def _reply(self, code: int, body: dict) -> None:
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        print("POST", self.path, len(body), "bytes", file=sys.stderr)
        if self.path.startswith("/v1/xorbs/default/"):
            (OUT / f"{self.path.rsplit('/', 1)[1]}.xorb").write_bytes(body)
            self._reply(200, {"was_inserted": True})
        elif self.path == "/v1/shards":
            (OUT / "upload.shard").write_bytes(body)
            self._reply(200, {"result": 1})
        else:
            self._reply(404, {})

    def do_GET(self) -> None:
        print("GET", self.path, file=sys.stderr)
        self._reply(404, {})

    def log_message(self, *args) -> None:
        pass


def main() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        # hf_xet reads these at import, so set them first.
        os.environ["HF_HOME"] = f"{tmp}/hf-home"
        os.environ["HF_XET_CACHE"] = f"{tmp}/xet-cache"
        import hf_xet

        OUT.mkdir(parents=True, exist_ok=True)
        for old in OUT.iterdir():
            old.unlink()

        a = random.Random(1).randbytes(48 * 1024)
        files = {
            "a.bin": a,
            "b.bin": random.Random(2).randbytes(16 * 1024) + a,
            "empty.bin": b"",
        }
        paths = []
        for name, data in files.items():
            path = Path(tmp) / name
            path.write_bytes(data)
            paths.append(str(path))

        server = ThreadingHTTPServer(("127.0.0.1", 0), CasStub)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        endpoint = f"http://127.0.0.1:{server.server_address[1]}"
        token = ("unused", int(time.time()) + 3600)
        for name, info in zip(files, hf_xet.upload_files(paths, endpoint, token, None, None, "model")):
            print(name, "file hash", info.hash, "sha256", info.sha256, "size", info.file_size)
        server.shutdown()


if __name__ == "__main__":
    main()
