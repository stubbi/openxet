"""The Hub leg's harness: a configured `openxet serve` with user tokens and
rules (ruling M2-R3), and stock `huggingface_hub` and `hf` runs against it.

Every client run is a fresh process with its own HF_HOME and Xet cache, so
nothing a run sees can come from an earlier run's cache, and the token
travels in the environment (HF_TOKEN), never in argv, except where a test
exercises `hf auth login --token` itself.
"""

import base64
import contextlib
import hashlib
import json
import secrets
import subprocess
import sys
from dataclasses import dataclass
from itertools import count
from pathlib import Path

import httpx
import pytest

from conftest import isolated_env, openxet_serve

CLIENT_PATIENCE = 900

# The users of every configured server, and what the rules let them do:
# alice owns `alice/*` and, through her `orgs` claim, `acme/*`; bob owns
# `bob/*`; anyone, with or without a credential, may read
# `alice/public-*`. Nobody may create in `huggingface`.
RULES = """
[[auth.rules]]
subjects = ["alice"]
repos = ["alice/*"]
actions = ["read", "write", "create"]

[[auth.rules]]
claims = { orgs = ["acme"] }
repos = ["acme/*"]
actions = ["read", "write", "create"]

[[auth.rules]]
subjects = ["bob"]
repos = ["bob/*"]
actions = ["read", "write", "create"]

[[auth.rules]]
subjects = ["anonymous"]
repos = ["alice/public-*"]
actions = ["read"]
"""

CLAIMS = {"alice": '{ orgs = ["acme"] }', "bob": None}

# The scheme apart from its value keeps the repository's redaction scan
# (tests/test_fixture_redaction.py) from reading header templates as
# stored credentials.
_BEARER = "Bearer"


def new_token() -> str:
    """A token in openxet's format (docs/reference/config.md, "Token
    format"): `oxt_` and 32 random bytes, base64url, unpadded."""
    raw = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    return f"oxt_{raw}"


@dataclass(frozen=True)
class User:
    name: str
    token: str

    def headers(self) -> dict[str, str]:
        return dict(authorization=" ".join((_BEARER, self.token)))


def hub_config(users: dict[str, User], extra: str = "") -> str:
    """A server configuration with `users` as user credentials (static
    tokens without a scope, by hash) and RULES."""
    entries = [
        "[[auth.static_tokens]]\n"
        f'sha256 = "{hashlib.sha256(user.token.encode()).hexdigest()}"\n'
        f'subject = "{user.name}"\n'
        + (f"claims = {CLAIMS[user.name]}\n" if CLAIMS[user.name] else "")
        for user in users.values()
    ]
    return "\n".join(entries) + RULES + extra


# Runs `code` with `api = HfApi()`, `hub = huggingface_hub` and `args` in
# scope, and prints `result` as JSON, or the HfHubHTTPError it raised.
# Two hooks record what reached hf_xet, so a plain-HTTP fallback is never
# mistaken for the Xet path. Uploads: every file huggingface_hub hands an
# hf_xet upload commit, by its real path. `upload_file` (`_commit_api`)
# and `upload_folder` (`_upload_pipeline`, huggingface_hub 2.x) both open
# the commit on the session `utils._xet.get_xet_session` returns, so the
# hook wraps that session. Downloads: `file_download.xet_get`.
_RUNNER = """
import json, os, sys
import huggingface_hub
import huggingface_hub._upload_pipeline as pipeline
import huggingface_hub.file_download as fd
import huggingface_hub.utils._xet as xet_utils
from huggingface_hub import HfApi
from huggingface_hub.errors import HfHubHTTPError

def record(entry):
    with open(os.environ["OPENXET_COMPAT_XET_CALLS"], "a") as f:
        f.write(json.dumps(entry) + "\\n")

class UploadCommit:
    def __init__(self, inner):
        self._inner = inner
    def __getattr__(self, name):
        return getattr(self._inner, name)
    def __enter__(self):
        self._inner.__enter__()
        return self
    def __exit__(self, *exc):
        return self._inner.__exit__(*exc)
    def start_upload_file(self, path, *args, **kwargs):
        record({"upload": os.path.realpath(path)})
        return self._inner.start_upload_file(path, *args, **kwargs)
    def start_upload_bytes(self, data, *args, **kwargs):
        record({"upload": kwargs.get("name") or "<bytes>"})
        return self._inner.start_upload_bytes(data, *args, **kwargs)

class Session:
    def __init__(self, inner):
        self._inner = inner
    def __getattr__(self, name):
        return getattr(self._inner, name)
    def new_upload_commit(self, *args, **kwargs):
        return UploadCommit(self._inner.new_upload_commit(*args, **kwargs))

real_session = xet_utils.get_xet_session
def get_xet_session():
    return Session(real_session())
xet_utils.get_xet_session = pipeline.get_xet_session = get_xet_session

real_get = fd.xet_get
def xet_get(*args, **kwargs):
    record({"download": kwargs["xet_file_data"].file_hash})
    return real_get(*args, **kwargs)
fd.xet_get = xet_get

request = json.loads(sys.argv[1])
scope = {"api": HfApi(), "hub": huggingface_hub, "args": request["args"]}
try:
    exec(request["code"], scope)
except HfHubHTTPError as e:
    status = e.response.status_code if e.response is not None else None
    print(json.dumps({"error": type(e).__name__, "status": status, "message": str(e)}))
else:
    print(json.dumps({"ok": scope.get("result")}, default=str))
"""


# Snippets for Hub.ok and Hub.python, which the Hub and Git legs share.
CREATE = """
result = str(api.create_repo(
    args["repo_id"], repo_type=args.get("repo_type"), private=args.get("private", False)
))
"""

UPLOAD_FOLDER = """
result = api.upload_folder(
    repo_id=args["repo_id"], folder_path=args["folder"], repo_type=args.get("repo_type"),
    commit_message=args.get("message", "upload"),
).oid
"""

UPLOAD_FILE = """
result = api.upload_file(
    path_or_fileobj=args["path"], path_in_repo=args["path_in_repo"], repo_id=args["repo_id"],
    repo_type=args.get("repo_type"), revision=args.get("revision"),
    commit_message=args.get("message", "upload"),
).oid
"""

SNAPSHOT = """
result = hub.snapshot_download(
    args["repo_id"], repo_type=args.get("repo_type"), revision=args.get("revision")
)
"""

DOWNLOAD = """
result = hub.hf_hub_download(
    args["repo_id"], args["filename"], repo_type=args.get("repo_type"),
    revision=args.get("revision"),
)
"""

PATHS_INFO = """
result = {
    entry.path: {
        "size": getattr(entry, "size", None),
        "xet_hash": getattr(entry, "xet_hash", None),
        "sha256": entry.lfs.sha256 if getattr(entry, "lfs", None) else None,
    }
    for entry in api.get_paths_info(
        args["repo_id"], args["paths"], repo_type=args.get("repo_type"), expand=True
    )
}
"""


@dataclass
class Run:
    """One client run: its output, with every token redacted, and the
    calls it handed hf_xet."""

    returncode: int
    stdout: str
    stderr: str
    root: Path

    def xet_calls(self, kind: str) -> list[str]:
        path = self.root / "xet-calls.jsonl"
        lines = path.read_text().splitlines() if path.exists() else []
        return [entry[kind] for entry in map(json.loads, lines) if kind in entry]


class Hub:
    """Stock clients against a configured server, as one of its users."""

    def __init__(self, server, users: dict[str, User], root: Path):
        self.server = server
        self.users = users
        self.root = root
        self._runs = count()

    @property
    def endpoint(self) -> str:
        return self.server.endpoint

    def fresh_root(self) -> Path:
        return self.root / f"client-{next(self._runs)}"

    def env(self, user: User | None, root: Path) -> dict[str, str]:
        env = isolated_env(
            root,
            HF_ENDPOINT=self.endpoint,
            HF_HUB_DISABLE_UPDATE_CHECK="1",
            OPENXET_COMPAT_XET_CALLS=str(root / "xet-calls.jsonl"),
        )
        if user is not None:
            env["HF_TOKEN"] = user.token
        # The server is local; a proxy from the shell would intercept it.
        return {k: v for k, v in env.items() if not k.lower().endswith("_proxy")}

    def redact(self, text: str) -> str:
        for user in self.users.values():
            text = text.replace(user.token, "REDACTED")
        return text

    def _run(self, argv: list[str], user: User | None, root: Path | None) -> Run:
        root = root or self.fresh_root()
        root.mkdir(parents=True, exist_ok=True)
        out = subprocess.run(
            argv,
            env=self.env(user, root),
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=CLIENT_PATIENCE,
        )
        return Run(out.returncode, self.redact(out.stdout), self.redact(out.stderr), root)

    def python(self, user: User | None, code: str, root: Path | None = None, **args) -> dict:
        """Runs `code` with stock huggingface_hub; `{"ok": result}` or
        `{"error", "status", "message"}` for an HfHubHTTPError."""
        run = self.python_run(user, code, root, **args)
        assert run.returncode == 0, f"the client run failed:\n{run.stderr}"
        return json.loads(run.stdout.strip().splitlines()[-1])

    def python_run(self, user: User | None, code: str, root: Path | None = None, **args) -> Run:
        request = json.dumps({"code": code, "args": args})
        return self._run([sys.executable, "-c", _RUNNER, request], user, root)

    def ok(self, user: User | None, code: str, **args):
        answer = self.python(user, code, **args)
        assert "ok" in answer, f"the client raised {answer}"
        return answer["ok"]

    def fails(self, user: User | None, code: str, **args) -> dict:
        answer = self.python(user, code, **args)
        assert "error" in answer, f"the client succeeded: {answer}"
        return answer

    def hf(self, user: User | None, *argv: str, root: Path | None = None) -> Run:
        """The `hf` CLI installed next to this interpreter."""
        return self._run([str(Path(sys.executable).parent / "hf"), *argv], user, root)

    def request(self, user: User | None, method: str, path: str, **kwargs) -> httpx.Response:
        headers = {**kwargs.pop("headers", {}), **(user.headers() if user else {})}
        return httpx.request(
            method, f"{self.endpoint}{path}", headers=headers, timeout=60, **kwargs
        )


@contextlib.contextmanager
def hub_server(openxet_bin: str, root: Path, extra_config: str = ""):
    """A configured `openxet serve` with alice and bob, and RULES."""
    users = {name: User(name, new_token()) for name in CLAIMS}
    config = root / "openxet.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(hub_config(users, extra_config))
    with openxet_serve(openxet_bin, root / "server", config) as server:
        assert server.token is None, "a configured server printed a token"
        for user in users.values():
            server.uses_credential(user.token)
        yield Hub(server, users, root / "clients")


@pytest.fixture
def hub(openxet_bin, tmp_path):
    with hub_server(openxet_bin, tmp_path) as hub:
        yield hub
