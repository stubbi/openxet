"""Fixtures and helpers for driving unmodified HF clients against a target.

Nothing here reads or writes the developer's real ~/.cache/huggingface: every
client run gets an isolated HF_HOME and HF_XET_CACHE under pytest's tmp_path.
"""

import contextlib
import hashlib
import importlib.metadata
import json
import os
import random
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path

import httpx
import pytest

DEFAULT_ENDPOINT = "https://huggingface.co"
DEFAULT_NAME = "huggingface.co"
NOT_INSTALLED = "not installed"
DOWNLOAD_ATTEMPTS = 3
MiB = 1 << 20

# The Hub, Git and LFS legs' harnesses, whose fixtures every test module sees.
pytest_plugins = ["hub_harness", "git_harness"]


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


def write_noise(path: Path, size: int, seed: int) -> Path:
    """Seeded pseudo-random bytes: incompressible, reproducible, never committed."""
    rng = random.Random(seed)
    with path.open("wb") as f:
        left = size
        while left:
            n = min(left, 16 * MiB)
            f.write(rng.randbytes(n))
            left -= n
    return path


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


# The only HF_XET_*, HF_HUB_* and OPENXET_* variables a child process sees:
# the ones the harness sets itself. Any other one in the developer's shell
# (HF_XET_HIGH_PERFORMANCE, HF_XET_CLIENT_READ_TIMEOUT, HF_HUB_OFFLINE,
# OPENXET_AUTH__JWT_PUBLIC_KEYS, ...) could change chunking, caching,
# timeouts or the server's configuration, so it never reaches a child.
HARNESS_ENV = frozenset(
    {
        "HF_HUB_CACHE",
        "HF_HUB_DISABLE_TELEMETRY",
        "HF_HUB_DISABLE_UPDATE_CHECK",
        "HF_XET_CACHE",
        "OPENXET_COMPAT_GITEA_PASSWORD",
        "OPENXET_COMPAT_TOKEN",
        "OPENXET_COMPAT_TOKEN_EXP",
        "OPENXET_COMPAT_XET_CALLS",
        "OPENXET_ENDPOINT",
        "OPENXET_SERVER__LOG_LEVEL",
        "OPENXET_TOKEN",
    }
)
SCRUBBED_PREFIXES = ("HF_XET_", "HF_HUB_", "OPENXET_")


def assert_scrubbed(env: dict[str, str]) -> None:
    leaked = sorted(
        k for k in env if k.startswith(SCRUBBED_PREFIXES) and k not in HARNESS_ENV
    )
    assert not leaked, (
        f"the child environment inherits {leaked}; only variables the harness "
        "sets itself belong in HARNESS_ENV"
    )


def isolated_env(root: Path, **harness: str) -> dict[str, str]:
    """Environment for a child process: anonymous, with every cache under `root`.

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
        HF_HUB_DISABLE_TELEMETRY="1",
    )
    env.update(harness)
    assert_scrubbed(env)
    return env


def build_hf_env(target: Target, root: Path) -> dict[str, str]:
    """Environment for a huggingface_hub run aimed at the target."""
    return isolated_env(
        root,
        HF_ENDPOINT=target.endpoint,
        OPENXET_COMPAT_XET_CALLS=str(root / "xet-calls.jsonl"),
    )


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
            "uv run --frozen --directory compat pytest -m agent`.",
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
def _openxet_tests_need_binary(request):
    if any(
        request.node.get_closest_marker(m)
        for m in ("agent", "storage_only", "docs", "hub", "git", "lfs")
    ):
        request.getfixturevalue("openxet_bin")


# Debug-log routes (axum route templates) that tests count.
XORB_ROUTE = "/v1/xorbs/{prefix}/{hash}"
DEDUP_ROUTE = "/v1/chunks/{prefix}/{hash}"
RECONSTRUCTION_ROUTE = "/v2/reconstructions/{hash}"
V1_RECONSTRUCTION_ROUTE = "/v1/reconstructions/{hash}"

SERVE_PATIENCE = 120.0


def assert_no_credential_in_log(log: str, credentials: set[str]) -> None:
    """Fails if `log` holds any of `credentials` (design §5.9 req 6). A JWT's
    signature segment counts on its own: the server must not log any part
    that authenticates a request."""
    fragments = set(credentials) | {
        c.rsplit(".", 1)[1] for c in credentials if c.count(".") == 2
    }
    leaked = sum(1 for fragment in fragments if fragment in log)
    assert not leaked, (
        f"the server log holds {leaked} of the {len(fragments)} credentials "
        "or JWT signatures the test sent"
    )


class OpenxetServer:
    """A running `openxet serve` and its JSON debug log.

    `server.log_level = "debug"` logs a `started processing request` and a
    `finished processing request` line (with `status`) per request, under a
    span naming the method and the matched route, so requests are counted
    from the server's own log, with no endpoint that exists only for tests.
    """

    def __init__(self, proc: subprocess.Popen, ready: dict, data_dir: Path, lines: list):
        self.proc = proc
        self.endpoint: str = ready["endpoint"]
        self.token: str | None = ready["token"]
        self.token_exp: int | None = ready["token_exp"]
        self.scope: str | None = ready["scope"]
        self.data_dir = data_dir
        self._lines = lines
        self._credentials: set[str] = {self.token} if self.token else set()

    def uses_credential(self, credential: str) -> None:
        """Registers a credential a client sends, so teardown can prove the
        server never logged it (design §5.9)."""
        self._credentials.add(credential)

    def assert_no_credential_logged(self) -> None:
        assert_no_credential_in_log("".join(self._lines), self._credentials)

    def log(self) -> list[dict]:
        entries = []
        for line in list(self._lines):
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError:
                continue
        return entries

    def mark(self) -> int:
        """A position in the log, for counting only what happens after it."""
        self.settle()
        return len(self.log())

    def _requests(self, message: str, method: str, route: str, since: int) -> list[dict]:
        return [
            entry
            for entry in self.log()[since:]
            if entry.get("fields", {}).get("message") == message
            and entry.get("span", {}).get("method") == method
            and entry.get("span", {}).get("route") == route
        ]

    def requests(self, method: str, route: str, since: int = 0) -> int:
        return len(self._requests("started processing request", method, route, since))

    def statuses(self, method: str, route: str, since: int = 0) -> list[int]:
        return [
            entry["fields"]["status"]
            for entry in self._requests("finished processing request", method, route, since)
        ]

    def answered(self, since: int = 0) -> list[tuple[str, str, int]]:
        """`(method, route template, status)` of every request answered
        after `since`, for routes whose parameters tests can't predict."""
        return [
            (entry["span"]["method"], entry["span"]["route"], entry["fields"]["status"])
            for entry in self.log()[since:]
            if entry.get("fields", {}).get("message") == "finished processing request"
        ]

    def settle(self) -> None:
        """Waits until the log holds every request answered before this call.

        Matching `started` and `finished` counts alone could pass while the
        drain thread has yet to read a request's lines, and a missed
        `POST /v1/xorbs` would make a "no xorb sent" check pass falsely. So a
        barrier `GET /healthz` goes last: once its `finished` line is read,
        every line written before it has been read too.
        """
        before = len(self._requests("finished processing request", "GET", "/healthz", 0))
        assert httpx.get(f"{self.endpoint}/healthz", timeout=5).status_code == 200
        deadline = time.monotonic() + SERVE_PATIENCE
        while True:
            messages = [entry.get("fields", {}).get("message") for entry in self.log()]
            barrier = len(self._requests("finished processing request", "GET", "/healthz", 0))
            if barrier > before and messages.count(
                "finished processing request"
            ) >= messages.count("started processing request"):
                return
            assert time.monotonic() < deadline, "the server log never settled"
            time.sleep(0.02)

    def xorb_bytes_accepted(self, since: int) -> int:
        """Stored size of every xorb accepted after `since`: the bodies the
        clients sent plus each stored footer, so never less than the wire."""
        xorbs = self.data_dir / "bucket" / "xorbs" / "default"
        return sum(
            (xorbs / entry["fields"]["xorb"][:2] / entry["fields"]["xorb"]).stat().st_size
            for entry in self.log()[since:]
            if entry.get("fields", {}).get("message") == "xorb upload accepted"
        )


def _first_line(pipe, patience: float) -> bytes:
    """The first line of `pipe`, then keeps draining it so the child never blocks."""
    first: list[bytes] = []
    got = threading.Event()

    def read():
        first.append(pipe.readline())
        got.set()
        for _ in pipe:
            pass

    threading.Thread(target=read, daemon=True).start()
    got.wait(patience)
    return first[0] if first else b""


@contextlib.contextmanager
def openxet_serve(
    openxet_bin: str, root: Path, config: Path | None = None, data_dir: Path | None = None
):
    """Runs `openxet serve` with its data under `root`, or in `data_dir`
    when several servers share one, until the block exits."""
    data_dir = data_dir or root / "data"
    args = [openxet_bin, "serve", "--data-dir", str(data_dir), "--port", "0"]
    args += ["--json", "--log-format", "json"]
    if config is not None:
        args += ["--config", str(config)]
    proc = subprocess.Popen(
        args,
        env=isolated_env(root / "env", OPENXET_SERVER__LOG_LEVEL="debug"),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    lines: list[str] = []

    def drain():
        for raw in proc.stderr:
            lines.append(raw.decode("utf-8", errors="replace"))

    drainer = threading.Thread(target=drain, daemon=True)
    drainer.start()
    try:
        line = _first_line(proc.stdout, SERVE_PATIENCE)
        try:
            ready = json.loads(line)
        except json.JSONDecodeError:
            proc.kill()
            proc.wait()
            drainer.join(5)
            # The server's log never holds a credential (docs/reference/config.md).
            pytest.fail(
                f"openxet serve printed no ready line (exit code {proc.returncode}); "
                "stderr:\n" + "".join(lines),
                pytrace=False,
            )
        assert ready["schema"] == "openxet.serve.v1", ready["schema"]
        deadline = time.monotonic() + SERVE_PATIENCE
        while httpx.get(f"{ready['endpoint']}/readyz", timeout=5).status_code != 200:
            assert time.monotonic() < deadline, "openxet serve never became ready"
            time.sleep(0.05)
        server = OpenxetServer(proc, ready, data_dir, lines)
        yield server
    finally:
        proc.kill()
        proc.wait()
        drainer.join(5)
    # Reached only when the test body passed, so it never masks its failure.
    server.assert_no_credential_logged()


@pytest.fixture
def openxet_server(openxet_bin, tmp_path):
    """A dev-mode `openxet serve`: one static write token for scope `default`."""
    with openxet_serve(openxet_bin, tmp_path / "server") as server:
        yield server


_MEASUREMENTS = pytest.StashKey[list[str]]()


@pytest.fixture
def record_measurement(request):
    """Records a number for the terminal summary, without asserting it."""

    def record(name: str, value: int, unit: str) -> None:
        request.config.stash.setdefault(_MEASUREMENTS, []).append(
            f"{request.node.nodeid}: {name} = {value:,} {unit}"
        )

    return record


def pytest_terminal_summary(terminalreporter, config):
    lines = config.stash.get(_MEASUREMENTS, [])
    if lines:
        terminalreporter.section("measurements (informational)")
        for line in lines:
            terminalreporter.write_line(line)


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
