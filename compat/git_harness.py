"""The Git and LFS legs' harness: stock `git`, `git-lfs` and `git-xet` with
an isolated configuration, and the Gitea the LFS leg hosts its repos on.

Every git run gets HOME under the test's directory and no system
configuration (GIT_CONFIG_NOSYSTEM), so neither the developer's
`~/.gitconfig` nor a system credential helper (macOS's osxkeychain) sees,
stores or supplies a credential. Credentials come from one helper the
harness writes, which answers per host from environment variables, so no
secret is in argv or in a file.
"""

import os
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit

import httpx
import pytest

from conftest import isolated_env

GIT_PATIENCE = 600


@dataclass(frozen=True)
class Credential:
    """What the helper answers for `host` (`host:port`, as git asks)."""

    host: str
    username: str
    # The variable holding the password; one of conftest.HARNESS_ENV.
    password_env: str


def _helper(credentials: list[Credential]) -> str:
    # The credential helper protocol: https://git-scm.com/docs/git-credential
    answer = "printf 'username=%s\\npassword=%s\\n'"
    arms = "".join(
        f'  {c.host!r}) {answer} {c.username!r} "${c.password_env}" ;;\n' for c in credentials
    )
    return (
        "#!/bin/sh\n"
        'test "$1" = get || exit 0\n'
        "host=\n"
        'while IFS= read -r line && test -n "$line"; do\n'
        "  case $line in host=*) host=${line#host=} ;; esac\n"
        "done\n"
        "case $host in\n"
        f"{arms}"
        "esac\n"
    )


class Git:
    """Runs git with an isolated HOME, `credentials` and `secrets` (the
    environment variables the credentials name)."""

    def __init__(self, root: Path, credentials: list[Credential], secrets: dict[str, str]):
        self.root = root
        self.secrets = secrets
        home = root / "home"
        home.mkdir(parents=True, exist_ok=True)
        helper = root / "credential-helper"
        helper.write_text(_helper(credentials))
        helper.chmod(0o700)
        (home / ".gitconfig").write_text(
            "[user]\n\tname = openxet compat\n\temail = compat@users.noreply.openxet.com\n"
            "[init]\n\tdefaultBranch = main\n"
            f"[credential]\n\thelper = {helper}\n"
            "[advice]\n\tdetachedHead = false\n"
        )
        self.env = isolated_env(root, GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0", **secrets)
        # The servers are local; a proxy from the shell would intercept them.
        self.env = {k: v for k, v in self.env.items() if not k.lower().endswith("_proxy")}
        # The isolated HOME replaces the developer's, so this is where
        # `git lfs install` writes its filters, when a leg needs them.
        self.home = home

    def redact(self, text: str) -> str:
        for secret in self.secrets.values():
            text = text.replace(secret, "REDACTED")
        return text

    def run(self, *args: str, cwd: Path | None = None, env: dict | None = None):
        out = subprocess.run(
            ["git", *args],
            cwd=cwd,
            env={**self.env, **(env or {})},
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=GIT_PATIENCE,
        )
        return subprocess.CompletedProcess(
            out.args, out.returncode, self.redact(out.stdout), self.redact(out.stderr)
        )

    def __call__(self, *args: str, cwd: Path | None = None, env: dict | None = None) -> str:
        """Runs git and returns its stdout; fails the test if git fails."""
        out = self.run(*args, cwd=cwd, env=env)
        assert out.returncode == 0, f"git {' '.join(args)} failed:\n{out.stderr}"
        return out.stdout

    def popen(self, *args: str, cwd: Path) -> subprocess.Popen:
        return subprocess.Popen(
            ["git", *args],
            cwd=cwd,
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )


def openxet_git(root: Path, token: str, *endpoints: str) -> Git:
    """git that authenticates to each of `endpoints` with `token` as the
    Basic password (ruling M2-R11)."""
    return Git(
        root,
        [Credential(urlsplit(e).netloc, "openxet", "OPENXET_COMPAT_TOKEN") for e in endpoints],
        {"OPENXET_COMPAT_TOKEN": token},
    )


def commit_file(git: Git, repo: Path, path: str, data: bytes, message: str) -> str:
    """Writes `data` to `path` in `repo`, commits it and returns the commit id."""
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
    git("add", "--", path, cwd=repo)
    git("commit", "-q", "-m", message, cwd=repo)
    return git("rev-parse", "HEAD", cwd=repo).strip()


def tool_version(*cmd: str) -> str | None:
    try:
        out = subprocess.run(
            cmd, capture_output=True, text=True, timeout=10, stdin=subprocess.DEVNULL
        )
    except OSError:
        return None
    return out.stdout.strip() if out.returncode == 0 else None


# The clients the LFS leg drives, at the versions S7 observed and CI
# installs (.github/workflows/ci.yml, `compat-lfs`).
GIT_LFS_VERSION = "3.7.1"
GIT_XET_VERSION = "0.2.1"


@pytest.fixture(scope="session")
def lfs_clients() -> None:
    lfs = tool_version("git", "lfs", "version") or ""
    xet = tool_version("git-xet", "--version") or ""
    problems = []
    if f"git-lfs/{GIT_LFS_VERSION} " not in lfs:
        problems.append(f"git-lfs {GIT_LFS_VERSION} (found: {lfs or 'none'})")
    if not re.search(rf"\b{re.escape(GIT_XET_VERSION)}\b", xet):
        problems.append(f"git-xet {GIT_XET_VERSION} (found: {xet or 'none'})")
    if problems:
        pytest.fail(
            "the LFS leg needs " + " and ".join(problems) + " on PATH; "
            "compat/README.md shows how to install them",
            pytrace=False,
        )


@dataclass(frozen=True)
class Gitea:
    """The Gitea that compat/gitea/compose.yaml runs, and its seeded user."""

    url: str
    user: str
    password: str

    @property
    def host(self) -> str:
        return urlsplit(self.url).netloc

    def credential(self) -> Credential:
        return Credential(self.host, self.user, "OPENXET_COMPAT_GITEA_PASSWORD")

    def create_repo(self, name: str) -> str:
        """Creates an empty repository and returns its clone URL."""
        created = httpx.post(
            f"{self.url}/api/v1/user/repos",
            auth=(self.user, self.password),
            json={"name": name, "private": True, "auto_init": False},
            timeout=60,
        )
        assert created.status_code == 201, f"Gitea refused to create {name}: {created.text}"
        return f"{self.url}/{self.user}/{name}.git"


@pytest.fixture(scope="session")
def gitea() -> Gitea:
    names = (
        "OPENXET_COMPAT_GITEA_URL",
        "OPENXET_COMPAT_GITEA_USER",
        "OPENXET_COMPAT_GITEA_PASSWORD",
    )
    missing = [n for n in names if not os.environ.get(n)]
    if missing:
        pytest.fail(
            f"{', '.join(missing)} not set. Start Gitea and export its variables with "
            '`eval "$(compat/gitea/seed.sh)"` (compat/README.md, "LFS leg").',
            pytrace=False,
        )
    url, user, password = (os.environ[n] for n in names)
    url = url.rstrip("/")
    try:
        version = httpx.get(f"{url}/api/v1/version", timeout=10)
    except httpx.HTTPError as e:
        pytest.fail(
            f"Gitea at {url} is unreachable ({e}); did compat/gitea/seed.sh start it?",
            pytrace=False,
        )
    assert version.status_code == 200, version.text
    return Gitea(url, user, password)
