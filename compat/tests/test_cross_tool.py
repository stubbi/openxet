"""openxet's client and stock hf_xet read each other's uploads byte for byte
(non-negotiable 1; ADR 0006, Consequences), against a dev-mode openxet serve.

hf_xet runs in a subprocess with its own HF_XET_CACHE, so neither client can
satisfy a download from a cache the other filled.
"""

import json
import os
import subprocess
import sys

import pytest

pytestmark = pytest.mark.agent

_HF_XET = """
import json, sys
import hf_xet
op, endpoint, token = sys.argv[1], sys.argv[2], sys.argv[3]
token_info = (token, 2**62)
if op == "upload":
    [info] = hf_xet.upload_files([sys.argv[4]], endpoint, token_info, None, None, None)
    print(json.dumps({"hash": info.hash, "size": info.file_size}))
else:
    info = hf_xet.PyXetDownloadInfo(sys.argv[4], sys.argv[5], int(sys.argv[6]))
    hf_xet.download_files([info], endpoint, token_info, None, None)
"""


def _clean_env(tmp_path, name):
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("OPENXET_", "HF_")) and not k.lower().endswith("_proxy")
    }
    env.update(
        HOME=str(tmp_path / name / "home"),
        HF_HOME=str(tmp_path / name / "hf"),
        HF_XET_CACHE=str(tmp_path / name / "xet"),
        HF_HUB_DISABLE_TELEMETRY="1",
    )
    return env


@pytest.fixture
def server(openxet_bin, tmp_path):
    proc = subprocess.Popen(
        [openxet_bin, "serve", "--data-dir", str(tmp_path / "data"), "--port", "0", "--json"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    try:
        yield json.loads(proc.stdout.readline())
    finally:
        proc.kill()
        proc.wait()


def _openxet(openxet_bin, server, tmp_path, *args):
    env = _clean_env(tmp_path, "openxet")
    env.update(OPENXET_ENDPOINT=server["endpoint"], OPENXET_TOKEN=server["token"])
    out = subprocess.run(
        [openxet_bin, *args, "--json"],
        env=env,
        capture_output=True,
        stdin=subprocess.DEVNULL,
        timeout=120,
    )
    assert out.returncode == 0, out.stderr.decode()
    return json.loads(out.stdout)


def _hf_xet(server, tmp_path, *args):
    out = subprocess.run(
        [sys.executable, "-c", _HF_XET, args[0], server["endpoint"], server["token"], *args[1:]],
        env=_clean_env(tmp_path, "hf_xet"),
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=120,
    )
    assert out.returncode == 0, out.stderr
    return out.stdout


def test_cross_tool_round_trip_with_hf_xet(openxet_bin, server, tmp_path):
    # Two chunks' worth and more, so each file spans several chunks.
    ours = tmp_path / "ours.bin"
    ours.write_bytes(os.urandom(3 << 20))
    theirs = tmp_path / "theirs.bin"
    theirs.write_bytes(os.urandom((1 << 20) + 17))

    # openxet uploads, hf_xet downloads.
    [uploaded] = _openxet(openxet_bin, server, tmp_path, "upload", str(ours))["files"]
    got = tmp_path / "ours.hf_xet"
    _hf_xet(server, tmp_path, "download", str(got), uploaded["hash"], str(uploaded["size"]))
    assert got.read_bytes() == ours.read_bytes()

    # hf_xet uploads, openxet downloads; both name the file by the same hash.
    info = json.loads(_hf_xet(server, tmp_path, "upload", str(theirs)))
    got = tmp_path / "theirs.openxet"
    result = _openxet(
        openxet_bin, server, tmp_path, "download", info["hash"], "--output", str(got)
    )
    assert result["size"] == theirs.stat().st_size
    assert got.read_bytes() == theirs.read_bytes()
    [again] = _openxet(openxet_bin, server, tmp_path, "upload", str(theirs))["files"]
    assert again["hash"] == info["hash"]
