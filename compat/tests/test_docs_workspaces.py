"""The agent workspaces guide runs as written (AGENTS.md: examples in docs run in CI).

Its fenced `bash` blocks run in order, as one bash script, inside a
scratch repository standing in for the reader's: a commit pushed to a
local `origin`, `target/` ignored, and build outputs in the main
worktree for `create` to seed from. Blocks fenced `bash install` are
skipped (CI provides the built `openxet`); a block fenced `bash fails`
must exit non-zero, as the refusals the guide shows do.

Nothing touches the real machine's state: HOME (so `~/.claude` and
`~/.config/openxet`), the workspace registry and Git's configuration are
all under pytest's tmp_path, and the daemon is off, since no command in
the guide needs it and the test must leave no process behind.
"""

import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

import pytest

from conftest import isolated_env

pytestmark = pytest.mark.docs

GUIDE = Path(__file__).resolve().parents[2] / "docs" / "guides" / "agent-workspaces.md"

# The commands the guide must keep running.
STEPS = (
    "openxet workspace install claude --user",
    "openxet workspace hook claude-create",
    "openxet workspace hook claude-remove",
    "openxet workspace create",
    "openxet workspace list",
    "openxet workspace remove",
    "--discard",
    "openxet workspace adopt --scan",
    "workspace.toml",
    "openxet workspace status",
)

GIT_ENV = dict(
    GIT_CONFIG_NOSYSTEM="1",
    GIT_TERMINAL_PROMPT="0",
    GIT_AUTHOR_NAME="OpenXet docs",
    GIT_AUTHOR_EMAIL="docs@users.noreply.openxet.com",
    GIT_COMMITTER_NAME="OpenXet docs",
    GIT_COMMITTER_EMAIL="docs@users.noreply.openxet.com",
)

TIMEOUT_SECONDS = 120

FENCE = re.compile(r"^```([^\n`]*)\n(.*?)^```[ \t]*$", re.MULTILINE | re.DOTALL)


def runnable_blocks(markdown: str) -> list[tuple[str, str]]:
    """The (info, body) of each block the test runs, in order."""
    blocks = [(info.strip(), body) for info, body in FENCE.findall(markdown)]
    return [(info, body) for info, body in blocks if info in ("bash", "bash fails")]


def as_script(blocks: list[tuple[str, str]]) -> str:
    parts = ["set -euo pipefail"]
    for number, (info, body) in enumerate(blocks, 1):
        if info == "bash fails":
            # Bash ignores `set -e` in an `if` condition, so the block's
            # status is its last command's: one refusal per block.
            assert body.strip().count("\n") == 0, "a `bash fails` block holds one command"
            parts.append(
                f"if ( {body.rstrip()}\n); then\n"
                f"  echo 'block {number} was meant to fail and did not' >&2; exit 1\nfi"
            )
        else:
            parts.append(body)
    return "\n".join(parts) + "\n"


def test_fences_are_parsed_as_a_reader_sees_them():
    markdown = (
        "```bash install\nnot run\n```\n"
        "```bash\necho one\n```\n"
        "```json\n{}\n```\n"
        "```bash fails\nfalse\n```\n"
    )
    blocks = runnable_blocks(markdown)
    assert blocks == [("bash", "echo one\n"), ("bash fails", "false\n")]
    ok = subprocess.run(["bash", "-c", as_script(blocks)], capture_output=True)
    assert ok.returncode == 0
    succeeds = as_script([("bash fails", "true\n")])
    assert subprocess.run(["bash", "-c", succeeds], capture_output=True).returncode == 1


def git(cwd: Path, env: dict[str, str], *args: str) -> None:
    subprocess.run(["git", *args], cwd=cwd, env=env, check=True, capture_output=True)


def readers_repository(work: Path, env: dict[str, str]) -> Path:
    """A repository with a pushed commit and a built, ignored `target/`."""
    origin = work / "origin.git"
    git(work, env, "init", "--bare", "-b", "main", str(origin))
    repo = work / "app"
    git(work, env, "init", "-b", "main", str(repo))
    (repo / "README.md").write_text("app\n")
    (repo / ".gitignore").write_text("target/\n")
    git(repo, env, "add", ".")
    git(repo, env, "commit", "-m", "start")
    git(repo, env, "remote", "add", "origin", str(origin))
    git(repo, env, "push", "-u", "origin", "main")
    out = repo / "target" / "debug" / "app"
    out.parent.mkdir(parents=True)
    out.write_bytes(os.urandom(1 << 20))
    # Older than the settle window, as a finished build's outputs are.
    past = time.time() - 60
    os.utime(out, (past, past))
    return repo


def test_agent_workspaces_guide_runs_as_written(openxet_bin, tmp_path):
    blocks = runnable_blocks(GUIDE.read_text())
    script = as_script(blocks)
    for step in STEPS:
        assert step in script, f"the agent workspaces guide no longer runs `{step}`"
    assert any(info == "bash fails" for info, _ in blocks)

    work = tmp_path / "work"
    work.mkdir()
    home = tmp_path / "env" / "home"
    home.mkdir(parents=True)
    state = tmp_path / "state"
    env = isolated_env(
        tmp_path / "env",
        OPENXET_WORKSPACE_STATE=str(state),
        OPENXET_WORKSPACE_DAEMON="off",
        **GIT_ENV,
    )
    env["PATH"] = os.pathsep.join(
        [str(Path(openxet_bin).parent), str(Path(sys.executable).parent), env["PATH"]]
    )
    repo = readers_repository(work, env)

    proc = subprocess.run(
        ["bash", "-c", script],
        cwd=repo,
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )
    assert proc.returncode == 0, (
        f"the agent workspaces guide failed (exit {proc.returncode}):\n{proc.stdout}{proc.stderr}"
    )

    # The refusal the guide shows is the documented one.
    assert "holds work that exists nowhere else" in proc.stderr
    # The hooks went into the scratch HOME, and the limits were read back.
    settings = json.loads((home / ".claude" / "settings.json").read_text())
    assert set(settings["hooks"]) == {"WorktreeCreate", "WorktreeRemove"}
    assert (home / ".config" / "openxet" / "workspace.toml").is_file()
    assert "floor 50.0 GiB" in proc.stdout

    # The guide cleans up after itself: nothing stays registered.
    listed = subprocess.run(
        [openxet_bin, "workspace", "list", "--json"],
        cwd=repo,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    assert json.loads(listed.stdout)["workspaces"] == []
