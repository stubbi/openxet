"""Storage-only leg: stock hf_xet against `openxet serve`, with no Hub.

hf_xet talks to the CAS directly (`upload_files` / `download_files` with
an endpoint and a token, docs/research/S10-cas-wire-contract.md "Auth"),
as a platform that embeds openxet's storage does. Every client run is a
fresh process with a fresh Xet cache, so a download can only be served by
the server and a dedup hit can only come from the server's answer.
"""

import hashlib
import importlib.metadata
import json
import random
import re
import subprocess
import sys
import time
from itertools import count
from pathlib import Path

import jwt
import pytest
from cryptography.hazmat.primitives import serialization

from conftest import (
    DEDUP_ROUTE,
    HARNESS_ENV,
    MiB,
    RECONSTRUCTION_ROUTE,
    SCRUBBED_PREFIXES,
    V1_RECONSTRUCTION_ROUTE,
    XORB_ROUTE,
    isolated_env,
    openxet_serve,
    sha256,
    write_noise,
)

pytestmark = pytest.mark.storage_only

CLIENT_PATIENCE = 600

# The client side of the storage-only API, as huggingface_hub drives it.
# The token travels in the environment, never in argv or output.
_HF_XET = """
import json, os, sys
import hf_xet
request = json.loads(sys.argv[1])
token_info = (os.environ["OPENXET_COMPAT_TOKEN"], int(os.environ["OPENXET_COMPAT_TOKEN_EXP"]))
if request["op"] == "upload":
    infos = hf_xet.upload_files(request["paths"], request["endpoint"], token_info, None, None, None)
    print(json.dumps([{"hash": i.hash, "size": i.file_size, "sha256": i.sha256} for i in infos]))
else:
    files = [hf_xet.PyXetDownloadInfo(f["dest"], f["hash"], f["size"]) for f in request["files"]]
    hf_xet.download_files(files, request["endpoint"], token_info, None, None)
"""

# Static tokens never expire; hf_xet needs an integer `exp` (config.md).
STATIC_EXP = 2**62


class Clients:
    """Runs hf_xet and the openxet client, each run with a fresh cache."""

    def __init__(self, openxet_bin: str, root: Path):
        self.openxet_bin = openxet_bin
        self.root = root
        self._runs = count()

    def fresh_root(self) -> Path:
        return self.root / f"client-{next(self._runs)}"

    def hf_xet(self, server, request: dict, token=None, exp=None):
        token = token or server.token
        exp = exp or server.token_exp or STATIC_EXP
        server.uses_credential(token)
        env = isolated_env(
            self.fresh_root(),
            OPENXET_COMPAT_TOKEN=token,
            OPENXET_COMPAT_TOKEN_EXP=str(exp),
        )
        out = subprocess.run(
            [sys.executable, "-c", _HF_XET, json.dumps({**request, "endpoint": server.endpoint})],
            env=env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=CLIENT_PATIENCE,
        )
        return out.returncode, out.stdout, out.stderr.replace(token, "REDACTED")

    def hf_xet_upload(self, server, paths, token=None, exp=None) -> list[dict]:
        code, stdout, stderr = self.hf_xet(
            server, {"op": "upload", "paths": [str(p) for p in paths]}, token, exp
        )
        assert code == 0, f"hf_xet upload failed:\n{stderr}"
        return json.loads(stdout)

    def hf_xet_download(self, server, dest: Path, info: dict, token=None, exp=None) -> None:
        code, _, stderr = self.hf_xet_download_result(server, dest, info, token, exp)
        assert code == 0, f"hf_xet download failed:\n{stderr}"

    def hf_xet_download_result(self, server, dest: Path, info: dict, token=None, exp=None):
        files = [{"dest": str(dest), "hash": info["hash"], "size": info["size"]}]
        return self.hf_xet(server, {"op": "download", "files": files}, token, exp)

    def openxet(self, server, *args) -> dict:
        env = isolated_env(
            self.fresh_root(), OPENXET_ENDPOINT=server.endpoint, OPENXET_TOKEN=server.token
        )
        out = subprocess.run(
            [self.openxet_bin, *args, "--json"],
            env=env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=CLIENT_PATIENCE,
        )
        assert out.returncode == 0, out.stderr.replace(server.token, "REDACTED")
        return json.loads(out.stdout)


@pytest.fixture
def clients(openxet_bin, tmp_path) -> Clients:
    return Clients(openxet_bin, tmp_path / "clients")


def run_openxet(openxet_bin: str, root: Path, *args) -> dict:
    out = subprocess.run(
        [openxet_bin, *args, "--json"],
        env=isolated_env(root),
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    return json.loads(out.stdout)


def test_child_env_scrubs_hf_xet_and_hf_hub_variables(tmp_path, monkeypatch):
    sentinel = tmp_path / "sentinel"
    for name, value in {
        "HF_XET_CACHE": str(sentinel),
        "HF_XET_HIGH_PERFORMANCE": "1",
        "HF_XET_CLIENT_READ_TIMEOUT": "1",
        "HF_HUB_OFFLINE": "1",
        "HF_HUB_CACHE": str(sentinel),
        "OPENXET_TOKEN": "not-a-token",
        "OPENXET_AUTH__MAX_TOKEN_LIFETIME": "1m",
    }.items():
        monkeypatch.setenv(name, value)

    root = tmp_path / "run"
    out = subprocess.run(
        [sys.executable, "-c", "import json, os; print(json.dumps(dict(os.environ)))"],
        env=isolated_env(root),
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    seen = json.loads(out.stdout)

    prefixed = {k for k in seen if k.startswith(SCRUBBED_PREFIXES)}
    assert prefixed <= HARNESS_ENV, sorted(prefixed - HARNESS_ENV)
    assert {"HF_XET_HIGH_PERFORMANCE", "HF_HUB_OFFLINE", "OPENXET_TOKEN"}.isdisjoint(seen)
    for name in ("HF_XET_CACHE", "HF_HUB_CACHE"):
        assert Path(seen[name]).is_relative_to(root), f"{name} escaped: {seen[name]}"


def test_server_ignores_the_developers_openxet_configuration(openxet_bin, tmp_path, monkeypatch):
    # Leaked into the server, either would change the run: an [auth] key
    # turns dev mode off, and a quieter log hides the counted requests.
    monkeypatch.setenv("OPENXET_AUTH__JWT_PUBLIC_KEYS", '["/nonexistent/issuer.pub"]')
    monkeypatch.setenv("OPENXET_SERVER__LOG_LEVEL", "error")

    with openxet_serve(openxet_bin, tmp_path / "server") as server:
        assert server.scope == "default" and server.token, "the server left dev mode"
        server.settle()
        assert server.requests("GET", "/readyz") > 0, "the debug request log is off"


def test_upload_download_round_trip(openxet_server, clients, tmp_path):
    paths = [
        write_noise(tmp_path / "small.bin", 1000, 1),
        write_noise(tmp_path / "large.bin", 3 * MiB, 2),
        write_noise(tmp_path / "empty.bin", 0, 3),
    ]

    infos = clients.hf_xet_upload(openxet_server, paths)

    assert len(infos) == len(paths)
    for path, info in zip(paths, infos):
        assert info["size"] == path.stat().st_size
        # hf_xet 1.7.0 reports 64 zeros as an empty file's sha256 (observed),
        # not the SHA-256 of no bytes. It is computed client-side and never
        # reaches the server; pinned so a client change shows up here.
        expected = "0" * 64 if info["size"] == 0 else sha256(path)
        assert info["sha256"] == expected, path.name
        dest = tmp_path / f"{path.name}.out"
        mark = openxet_server.mark()
        clients.hf_xet_download(openxet_server, dest, info)
        assert dest.read_bytes() == path.read_bytes(), path.name
        # hf_xet falls back to v1 silently if v2 fails, so a broken v2 route
        # would otherwise go unnoticed here. An empty file's only block
        # starts at EOF and answers 416, which the client reads as the end.
        openxet_server.settle()
        statuses = openxet_server.statuses("GET", RECONSTRUCTION_ROUTE, mark)
        assert statuses and set(statuses) <= {200, 416}, (path.name, statuses)
        assert (200 in statuses) == (info["size"] > 0), (path.name, statuses)
        assert openxet_server.requests("GET", V1_RECONSTRUCTION_ROUTE, mark) == 0, path.name
    assert len({info["hash"] for info in infos}) == len(paths)


def test_reupload_global_dedup(openxet_server, clients, tmp_path):
    path = write_noise(tmp_path / "model.bin", 3 * MiB, 4)
    mark = openxet_server.mark()
    [first] = clients.hf_xet_upload(openxet_server, [path])
    openxet_server.settle()
    assert openxet_server.requests("POST", XORB_ROUTE, mark) > 0, "the first upload sent no xorb"

    # A fresh client cache: only the server's dedup answer can spare the xorbs.
    mark = openxet_server.mark()
    [second] = clients.hf_xet_upload(openxet_server, [path])
    openxet_server.settle()

    assert openxet_server.requests("GET", DEDUP_ROUTE, mark) > 0, (
        "the second upload never asked the server for global dedup"
    )
    assert openxet_server.requests("POST", XORB_ROUTE, mark) == 0, (
        "the second upload sent a xorb the server already had"
    )
    assert second["hash"] == first["hash"]


def test_range_and_empty_file(openxet_server, clients, tmp_path):
    # hf_xet plans downloads in blocks, the first 256,000,000 bytes long
    # (S10 §1), so 300 MiB needs a second block, which ends past EOF and is
    # clamped. A further block that starts past EOF answers 416, which the
    # client reads as the end of the file.
    big = write_noise(tmp_path / "big.bin", 300 * MiB, 5)
    empty = write_noise(tmp_path / "empty.bin", 0, 6)
    big_info, empty_info = clients.hf_xet_upload(openxet_server, [big, empty])

    mark = openxet_server.mark()
    dest = tmp_path / "big.out"
    clients.hf_xet_download(openxet_server, dest, big_info)
    assert dest.stat().st_size == big.stat().st_size
    assert sha256(dest) == sha256(big)
    openxet_server.settle()
    statuses = openxet_server.statuses("GET", RECONSTRUCTION_ROUTE, mark)
    assert statuses.count(200) >= 2, f"one reconstruction block covered 300 MiB: {statuses}"
    assert set(statuses) <= {200, 416}, statuses

    dest = tmp_path / "empty.out"
    clients.hf_xet_download(openxet_server, dest, empty_info)
    assert dest.read_bytes() == b""


def test_cross_tool_round_trips(openxet_server, clients, tmp_path):
    ours = write_noise(tmp_path / "ours.bin", 3 * MiB, 7)
    theirs = write_noise(tmp_path / "theirs.bin", MiB + 17, 8)

    # openxet uploads, hf_xet downloads.
    [uploaded] = clients.openxet(openxet_server, "upload", str(ours))["files"]
    dest = tmp_path / "ours.hf_xet"
    clients.hf_xet_download(openxet_server, dest, uploaded)
    assert dest.read_bytes() == ours.read_bytes()

    # hf_xet uploads, openxet downloads.
    [info] = clients.hf_xet_upload(openxet_server, [theirs])
    dest = tmp_path / "theirs.openxet"
    result = clients.openxet(openxet_server, "download", info["hash"], "--output", str(dest))
    assert result["size"] == theirs.stat().st_size
    assert dest.read_bytes() == theirs.read_bytes()

    # Both name a file by the same Xet hash.
    [again] = clients.openxet(openxet_server, "upload", str(theirs))["files"]
    assert again["hash"] == info["hash"]
    [again] = clients.hf_xet_upload(openxet_server, [ours])
    assert again["hash"] == uploaded["hash"]


def _static_token(openxet_bin, root: Path, scope: str, mode: str) -> dict:
    return run_openxet(
        openxet_bin, root, "token", "generate", "--scope", scope, "--mode", mode,
        "--subject", f"compat-{scope}",
    )


def test_wrong_scope_token_fails(openxet_bin, clients, tmp_path):
    team_a = _static_token(openxet_bin, tmp_path / "gen-a", "team-a", "write")
    team_b = _static_token(openxet_bin, tmp_path / "gen-b", "team-b", "write")
    config = tmp_path / "openxet.toml"
    config.write_text(f"{team_a['config']}\n{team_b['config']}\n")
    path = write_noise(tmp_path / "a.bin", MiB, 9)

    with openxet_serve(openxet_bin, tmp_path / "server", config) as server:
        assert server.token is None, "a configured server printed a token"
        [info] = clients.hf_xet_upload(server, [path], team_a["token"], STATIC_EXP)
        dest = tmp_path / "a.out"
        clients.hf_xet_download(server, dest, info, team_a["token"], STATIC_EXP)
        assert dest.read_bytes() == path.read_bytes()

        mark = server.mark()
        dest = tmp_path / "b.out"
        code, _, _ = clients.hf_xet_download_result(
            server, dest, info, team_b["token"], STATIC_EXP
        )
        server.settle()
        statuses = server.statuses("GET", RECONSTRUCTION_ROUTE, mark)

    assert code != 0, "another scope's token downloaded team-a's file"
    assert statuses, "the wrong-scope download never reached the server"
    assert all(400 <= s < 500 for s in statuses), statuses
    assert not dest.exists() or dest.read_bytes() != path.read_bytes()


def _kid(public_key_pem: bytes) -> str:
    # docs/reference/config.md "CAS tokens": SHA-256 of the raw 32-byte key.
    raw = serialization.load_pem_public_key(public_key_pem).public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    return hashlib.sha256(raw).hexdigest()[:16]


def test_platform_minted_jwt_with_pyjwt(openxet_bin, clients, tmp_path):
    # A platform signs CAS tokens with its own JWT library (ADR 0007); only
    # the key pair comes from openxet.
    keys = run_openxet(
        openxet_bin, tmp_path / "keygen", "token", "keygen", "--key", str(tmp_path / "issuer.key")
    )
    kid = _kid(Path(keys["public_key"]).read_bytes())
    assert kid == keys["kid"]
    signing_key = serialization.load_pem_private_key(
        Path(keys["private_key"]).read_bytes(), password=None
    )
    config = tmp_path / "openxet.toml"
    config.write_text(f"[auth]\njwt_public_keys = [{json.dumps(keys['public_key'])}]\n")

    now = int(time.time())
    claims = {
        "sub": "platform-user",
        "scope": "platform",
        "mode": "write",
        "exp": now + 3600,
        "iat": now,
    }

    def mint(claims: dict) -> str:
        headers = {"kid": kid, "typ": "JWT"}
        token = jwt.encode(claims, signing_key, algorithm="EdDSA", headers=headers)
        header = jwt.get_unverified_header(token)
        assert header == {"alg": "EdDSA", "typ": "JWT", "kid": kid}, header
        return token

    token = mint(claims)
    path = write_noise(tmp_path / "p.bin", 2 * MiB, 10)
    with openxet_serve(openxet_bin, tmp_path / "server", config) as server:
        [info] = clients.hf_xet_upload(server, [path], token, claims["exp"])
        dest = tmp_path / "p.out"
        clients.hf_xet_download(server, dest, info, token, claims["exp"])
        assert dest.read_bytes() == path.read_bytes()

        # openxet can't honour `aud`, so a token carrying it is refused
        # outright rather than half-understood.
        with_aud = mint({**claims, "aud": "openxet"})
        mark = server.mark()
        other = write_noise(tmp_path / "q.bin", MiB, 11)
        code, _, _ = clients.hf_xet(
            server, {"op": "upload", "paths": [str(other)]}, with_aud, claims["exp"]
        )
        server.settle()
        answered = [
            (entry["span"]["route"], entry["fields"]["status"])
            for entry in server.log()[mark:]
            if entry.get("fields", {}).get("message") == "finished processing request"
            and entry["span"]["route"] not in ("/healthz", "/readyz")
        ]

    assert code != 0, "hf_xet uploaded with a token carrying `aud`"
    # Every CAS route refuses the token. A route openxet doesn't serve
    # answers 404 whatever the token: hf-xet 1.8.0.dev0 first tries its
    # upload-grant route, then falls back to `POST /v1/xorbs` (observed).
    assert answered and all(
        status == 401 if route != "unmatched" else status == 404 for route, status in answered
    ), answered
    assert any(route != "unmatched" for route, _ in answered), answered


@pytest.mark.pinned
def test_version_floor_recorded():
    # Ruling M1-R9: the oldest hf_xet openxet supports is the one this leg
    # pins, so the floor users read is the one CI proves.
    readme = (Path(__file__).parent.parent / "README.md").read_text()
    floors = re.findall(r"client version floor: `hf_xet==([^`]+)`", readme, re.IGNORECASE)
    assert len(floors) == 1, f"compat/README.md must state the floor once, found {floors}"
    assert importlib.metadata.version("hf_xet") == floors[0]


def test_fresh_cache_edit_bytes_recorded(openxet_server, clients, tmp_path, record_measurement):
    # Design §1a: a 1 MiB edit in the middle of a 100 MiB file, re-uploaded
    # by stock hf_xet with an empty cache. The bound (at most 2 MiB) is
    # asserted with the openxet client, on the same client core, by
    # `fresh_cache_edit_dedups_against_the_server` in
    # crates/openxet/tests/transfer_cmd.rs. The seeded input makes the bytes
    # deterministic per client version, so for the version the website
    # quotes they must equal site/measurements.json: a change fails here
    # until the fixture and the pages agree (site/test_measurements.py).
    size, edit = 100 * MiB, MiB
    original = write_noise(tmp_path / "original.bin", size, 12)
    edited = tmp_path / "edited.bin"
    data = bytearray(original.read_bytes())
    data[size // 2 : size // 2 + edit] = random.Random(13).randbytes(edit)
    edited.write_bytes(data)
    del data
    clients.hf_xet_upload(openxet_server, [original])

    mark = openxet_server.mark()
    [info] = clients.hf_xet_upload(openxet_server, [edited])
    openxet_server.settle()
    sent = openxet_server.xorb_bytes_accepted(mark)
    record_measurement("xorb bytes sent for a 1 MiB edit of 100 MiB, fresh cache", sent, "B")
    quoted = json.loads((Path(__file__).parents[2] / "site/measurements.json").read_text())["edit_upload"]
    if importlib.metadata.version("hf_xet") == quoted["client_version"]:
        assert sent == quoted["bytes"], (
            f"{sent:,} B sent, but the website quotes {quoted['bytes']:,} B: "
            "update site/measurements.json and the pages that quote it"
        )

    dest = tmp_path / "edited.out"
    clients.hf_xet_download(openxet_server, dest, info)
    assert sha256(dest) == sha256(edited)


def _messages(server) -> list[str]:
    return [entry.get("fields", {}).get("message", "") for entry in server.log()]


@pytest.mark.parametrize("killed", ["server", "client"])
def test_sigkill_mid_upload_never_blocks_reupload(openxet_bin, tmp_path, killed):
    # Lesson L10 (docs/competitive-matrix.md): a SIGKILL at any point in an
    # upload never blocks re-uploading the same content. Stock hf_xet
    # uploads a file of several xorbs. Once the server has accepted the
    # first xorb, and before any shard is registered, the server or the
    # client is SIGKILLed. On a restarted server, the same client with the
    # same Xet cache uploads the file again, and a fresh client downloads it.
    path = write_noise(tmp_path / "big.bin", 512 * MiB, 14)
    request = {"op": "upload", "paths": [str(path)]}

    def start_upload(server) -> subprocess.Popen:
        server.uses_credential(server.token)
        env = isolated_env(
            tmp_path / "client",
            OPENXET_COMPAT_TOKEN=server.token,
            OPENXET_COMPAT_TOKEN_EXP=str(server.token_exp),
        )
        return subprocess.Popen(
            [sys.executable, "-c", _HF_XET, json.dumps({**request, "endpoint": server.endpoint})],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    root = tmp_path / "server"
    with openxet_serve(openxet_bin, root) as server:
        client = start_upload(server)
        try:
            deadline = time.monotonic() + CLIENT_PATIENCE
            while "xorb upload accepted" not in _messages(server):
                assert client.poll() is None, "the upload ended before the server took a xorb"
                assert time.monotonic() < deadline, "the server never accepted a xorb"
                time.sleep(0.01)
            # A killed client leaves a xorb body half sent, so the log can't
            # settle; what it holds by now decides whether the kill was
            # mid-upload, since a dead client registers nothing later.
            (server.proc if killed == "server" else client).kill()
        finally:
            client.kill()
            client.communicate()
        assert "shard registered" not in _messages(server), (
            "the upload finished before the kill, so the kill was not mid-upload"
        )

    with openxet_serve(openxet_bin, root) as server:
        client = start_upload(server)
        out, err = client.communicate(timeout=CLIENT_PATIENCE)
        assert client.returncode == 0, err.replace(server.token, "REDACTED")
        [info] = json.loads(out)
        dest = tmp_path / "big.out"
        Clients(openxet_bin, tmp_path / "fresh").hf_xet_download(server, dest, info)

    assert info["size"] == path.stat().st_size
    assert sha256(dest) == sha256(path)
