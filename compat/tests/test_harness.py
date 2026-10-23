"""The harness itself: isolation and pinned client versions."""

import importlib.metadata
import json
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

from conftest import build_hf_env

_PROBE = """
import json
from huggingface_hub import constants, get_token
print(json.dumps({
    "token": get_token(),
    "hub_cache": constants.HF_HUB_CACHE,
    "assets_cache": constants.HF_ASSETS_CACHE,
    "xet_cache": constants.HF_XET_CACHE,
}))
"""


def test_legacy_cache_and_token_variables_do_not_leak(target, tmp_path, monkeypatch):
    sentinel = tmp_path / "sentinel"
    sentinel.mkdir()
    for name in (
        "HUGGINGFACE_HUB_CACHE",
        "HUGGINGFACE_ASSETS_CACHE",
        "HF_HUB_CACHE",
        "HF_ASSETS_CACHE",
        "HF_XET_CACHE",
        "HF_HOME",
        "XDG_CACHE_HOME",
        "HF_TOKEN_PATH",
    ):
        monkeypatch.setenv(name, str(sentinel))
    for name in ("HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_TOKEN"):
        monkeypatch.setenv(name, "hf_sentinel_must_not_be_used")

    root = tmp_path / "run"
    env = build_hf_env(target, root)
    out = subprocess.run(
        [sys.executable, "-c", _PROBE],
        env=env,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    seen = json.loads(out.stdout)

    assert seen["token"] is None
    for key in ("hub_cache", "assets_cache", "xet_cache"):
        assert Path(seen[key]).is_relative_to(root), f"{key} escaped: {seen[key]}"
    assert list(sentinel.iterdir()) == []


@pytest.mark.pinned
def test_installed_client_versions_equal_the_pins():
    pyproject = tomllib.loads(
        (Path(__file__).parent.parent / "pyproject.toml").read_text()
    )
    pins = dict(dep.split("==") for dep in pyproject["project"]["dependencies"])
    assert pins, "no exact pins found in pyproject.toml"
    for name, version in pins.items():
        assert importlib.metadata.version(name) == version, name
