"""Fixtures and helpers for driving unmodified HF clients against a target.

Nothing here reads or writes the developer's real ~/.cache/huggingface: every
client run gets an isolated HF_HOME and HF_XET_CACHE under pytest's tmp_path.
"""

import hashlib
import importlib.metadata
import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import pytest

DEFAULT_ENDPOINT = "https://huggingface.co"
DEFAULT_NAME = "huggingface.co"
NOT_INSTALLED = "not installed"
DOWNLOAD_ATTEMPTS = 3


@dataclass(frozen=True)
class Target:
    endpoint: str
    name: str


@dataclass(frozen=True)
class HubFile:
    repo_id: str
    repo_type: str
    revision: str
    filename: str
    sha256: str
    xet_hash: str

    def resolve_url(self, endpoint: str) -> str:
        return (
            f"{endpoint}/{self.repo_type}s/{self.repo_id}/resolve/"
            f"{self.revision}/{self.filename}"
        )


# Pinned reference file from xet-team/xet-spec-reference-files; the same file
# is used by the Rust golden tests.
REFERENCE_FILE = HubFile(
    repo_id="xet-team/xet-spec-reference-files",
    repo_type="dataset",
    revision="c4aa3a3f15b1395fff5ce934784bf6c8f2d62de8",
    filename="Electric_Vehicle_Population_Data_20250917.csv",
    sha256="f41255b252f776125f2b657136654e4d4d5d2ccf8ef4db0ec186bd1981b69734",
    xet_hash="118a53328412787fee04011dcf82fdc4acf3a4a1eddec341c910d30a306aaf97",
)

# The hook wraps the call boundary huggingface_hub uses to hand a file to
# hf_xet and records the Xet hash it was given, so a plain-HTTP fallback can
# never be mistaken for a Xet download.
_DOWNLOAD_SCRIPT = """
import json, os, sys
import huggingface_hub.file_download as fd
from huggingface_hub import hf_hub_download

real_xet_get = fd.xet_get

def xet_get(*args, **kwargs):
    with open(os.environ["OPENXET_COMPAT_XET_CALLS"], "a") as f:
        f.write(json.dumps({"file_hash": kwargs["xet_file_data"].file_hash}) + "\\n")
    return real_xet_get(*args, **kwargs)

fd.xet_get = xet_get
print(hf_hub_download(sys.argv[1], sys.argv[2], repo_type=sys.argv[3], revision=sys.argv[4]))
"""

_PASSTHROUGH_ENV = (
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    *(
        name
        for base in ("HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY")
        for name in (base, base.lower())
    ),
)


def sha256(path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def _target_from_env() -> Target:
    endpoint = os.environ.get("OPENXET_COMPAT_TARGET", DEFAULT_ENDPOINT).rstrip("/")
    name = DEFAULT_NAME if endpoint == DEFAULT_ENDPOINT else endpoint
    return Target(endpoint=endpoint, name=name)


def _tool_version(*cmd: str) -> str:
    try:
        out = subprocess.run(
            cmd, capture_output=True, text=True, timeout=10, stdin=subprocess.DEVNULL
        )
    except (OSError, subprocess.TimeoutExpired):
        return NOT_INSTALLED
    text = (out.stdout or out.stderr).strip()
    return text.splitlines()[0] if out.returncode == 0 and text else NOT_INSTALLED


def _package_version(name: str) -> str:
    try:
        return importlib.metadata.version(name)
    except importlib.metadata.PackageNotFoundError:
        return NOT_INSTALLED


def pytest_report_header(config):
    return [
        f"compat target: {_target_from_env().name} ({_target_from_env().endpoint})",
        f"huggingface_hub: {_package_version('huggingface_hub')}",
        f"hf_xet: {_package_version('hf_xet')}",
        f"git: {_tool_version('git', '--version')}",
        f"git-lfs: {_tool_version('git', 'lfs', 'version')}",
        f"git-xet: {_tool_version('git', 'xet', '--version')}",
    ]


@pytest.fixture(scope="session")
def target() -> Target:
    return _target_from_env()


def build_hf_env(target: Target, root: Path) -> dict[str, str]:
    """Environment for a client subprocess: anonymous, isolated, aimed at the target.

    Built from an allowlist, not by filtering os.environ: huggingface_hub also
    honours legacy names (HUGGINGFACE_HUB_CACHE, HUGGING_FACE_HUB_TOKEN,
    HUGGINGFACE_ASSETS_CACHE) and XDG_CACHE_HOME, and a denylist would miss the
    next one.
    """
    env = {k: os.environ[k] for k in _PASSTHROUGH_ENV if k in os.environ}
    env.update(
        HOME=str(root / "home"),
        XDG_CACHE_HOME=str(root / "xdg-cache"),
        HF_HOME=str(root / "hf"),
        HF_HUB_CACHE=str(root / "hf" / "hub"),
        HF_XET_CACHE=str(root / "xet"),
        HF_ENDPOINT=target.endpoint,
        HF_HUB_DISABLE_TELEMETRY="1",
        OPENXET_COMPAT_XET_CALLS=str(root / "xet-calls.jsonl"),
    )
    return env


@pytest.fixture
def hf_env(tmp_path, target) -> dict[str, str]:
    return build_hf_env(target, tmp_path)


@pytest.fixture(scope="session")
def openxet_bin() -> str:
    raw = os.environ.get("OPENXET_BIN")
    if not raw:
        pytest.fail(
            "OPENXET_BIN is not set. Build the CLI and point at it, e.g. "
            "`cargo build && OPENXET_BIN=$PWD/target/debug/openxet "
            "uv run --directory compat pytest -m agent`.",
            pytrace=False,
        )
    path = Path(raw).resolve()
    if not path.is_file():
        pytest.fail(
            f"OPENXET_BIN={raw} does not exist (resolved to {path}). "
            "Run `cargo build` first, or fix the path.",
            pytrace=False,
        )
    return str(path)


@pytest.fixture(autouse=True)
def _agent_tests_need_binary(request):
    if request.node.get_closest_marker("agent"):
        request.getfixturevalue("openxet_bin")


def run_hub_download(target: Target, hf_env: dict[str, str], file: HubFile) -> Path:
    """Download with stock huggingface_hub in a fresh process.

    A subprocess is required: huggingface_hub reads HF_HOME at import time, so
    in-process use would hit the real cache.
    """
    for attempt in range(1, DOWNLOAD_ATTEMPTS + 1):
        out = subprocess.run(
            [
                sys.executable,
                "-c",
                _DOWNLOAD_SCRIPT,
                file.repo_id,
                file.filename,
                file.repo_type,
                file.revision,
            ],
            env=hf_env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=300,
        )
        if out.returncode == 0:
            return Path(out.stdout.strip().splitlines()[-1])
        if attempt < DOWNLOAD_ATTEMPTS:
            time.sleep(5 * attempt)
    pytest.fail(f"download failed after {DOWNLOAD_ATTEMPTS} attempts:\n{out.stderr}")


def assert_xet_download(hf_env: dict[str, str], file: HubFile) -> None:
    """Prove hf_xet was handed `file`, not the plain HTTP fallback.

    Primary: the call-boundary hook in the download subprocess recorded the
    Xet hash passed to huggingface_hub's xet_get. Secondary: hf_xet 1.7.0
    logged reconstructing that hash under $HF_XET_CACHE/logs, which shows the
    native code ran too.
    """
    calls_path = Path(hf_env["OPENXET_COMPAT_XET_CALLS"])
    calls = (
        [json.loads(line)["file_hash"] for line in calls_path.read_text().splitlines()]
        if calls_path.exists()
        else []
    )
    assert file.xet_hash in calls, (
        f"huggingface_hub never handed {file.xet_hash} to hf_xet (saw {calls}): "
        "the download did not use the Xet path. Is HF_HUB_DISABLE_XET set, or "
        "hf_xet missing?"
    )

    logs = sorted((Path(hf_env["HF_XET_CACHE"]) / "logs").glob("*.log"))
    reconstructed = set()
    for log in logs:
        for line in log.read_text().splitlines():
            try:
                fields = json.loads(line).get("fields", {})
            except json.JSONDecodeError:
                continue
            if fields.get("message") == "Reconstructing file to disk":
                reconstructed.add(fields.get("file_hash"))
    assert file.xet_hash in reconstructed, (
        f"hf_xet logged no reconstruction of {file.xet_hash} under "
        f"{hf_env['HF_XET_CACHE']}/logs (saw {sorted(reconstructed)}); if hf_xet "
        "changed its log format, update this check"
    )
