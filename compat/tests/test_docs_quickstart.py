"""The quickstarts run as written (AGENTS.md: examples in docs run in CI).

Every fenced `bash` and `python` block of each quickstart runs in order,
as one bash script in an empty directory, the way a reader pastes them
into one terminal: variables and the background server carry from block
to block, and a `python` block is fed to `python3` on stdin. Blocks
fenced as `bash install` are skipped; CI provides what they install: the
built `openxet` (OPENXET_BIN) and this environment's pinned `hf_xet`,
`huggingface_hub` and its `hf` are first on PATH. The Hub quickstart also
needs what it asks the reader to have: `git`, with a name and email.
"""

import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from conftest import assert_no_credential_in_log, isolated_env

pytestmark = pytest.mark.docs

DOCS = Path(__file__).resolve().parents[2] / "docs"

# Each quickstart, and the steps it must keep running.
QUICKSTARTS = {
    "storage-only": (
        DOCS / "quickstart-storage-only.md",
        ("openxet serve", "import hf_xet", "openxet upload", "openxet download", "--json"),
    ),
    "hub": (
        DOCS / "quickstart-hub.md",
        ("openxet serve", "hf auth whoami", "hf upload", "hf download", "git clone", "push"),
    ),
}

# The reader's git identity, which the Hub quickstart's commit needs; no
# system configuration, so a runner's global Git LFS filters don't change
# what the clone holds.
GIT_ENV = dict(
    GIT_CONFIG_NOSYSTEM="1",
    GIT_TERMINAL_PROMPT="0",
    GIT_AUTHOR_NAME="OpenXet docs",
    GIT_AUTHOR_EMAIL="docs@users.noreply.openxet.com",
    GIT_COMMITTER_NAME="OpenXet docs",
    GIT_COMMITTER_EMAIL="docs@users.noreply.openxet.com",
)

# A cap on each walkthrough without its install. Design §5.8's
# 2 minutes to first upload counts the install too; that end-to-end figure
# is measured per release, not here.
TARGET_SECONDS = 120

FENCE = re.compile(r"^```([^\n`]*)\n(.*?)^```[ \t]*$", re.MULTILINE | re.DOTALL)
HEREDOC_END = "OPENXET_DOCS_PYTHON"


def runnable_blocks(markdown: str) -> list[tuple[str, str]]:
    """The (language, body) of each block the test runs, in order."""
    blocks = [(info.strip(), body) for info, body in FENCE.findall(markdown)]
    return [(info, body) for info, body in blocks if info in ("bash", "python")]


def as_script(blocks: list[tuple[str, str]]) -> str:
    parts = ["set -euo pipefail"]
    for language, body in blocks:
        if language == "python":
            assert HEREDOC_END not in body
            parts.append(f"python3 - <<'{HEREDOC_END}'\n{body}{HEREDOC_END}")
        else:
            parts.append(body)
    return "\n".join(parts) + "\n"


def test_fences_are_parsed_as_a_reader_sees_them():
    markdown = (
        "```bash install\nnot run\n```\n"
        "```bash\necho one\n```\n"
        "```json\n{}\n```\n"
        "```python\nprint(2)\n```\n"
    )
    assert runnable_blocks(markdown) == [("bash", "echo one\n"), ("python", "print(2)\n")]


@pytest.mark.parametrize("name", sorted(QUICKSTARTS))
def test_quickstart_runs_as_written(name, openxet_bin, tmp_path, record_measurement):
    path, steps = QUICKSTARTS[name]
    blocks = runnable_blocks(path.read_text())
    script = as_script(blocks)
    for step in steps:
        assert step in script, f"the {name} quickstart no longer runs `{step}`"

    work = tmp_path / "work"
    work.mkdir()
    env = isolated_env(tmp_path / "env", **GIT_ENV)
    (tmp_path / "env" / "home").mkdir(parents=True, exist_ok=True)
    env["PATH"] = os.pathsep.join(
        [str(Path(openxet_bin).parent), str(Path(sys.executable).parent), env["PATH"]]
    )
    # The server is local; a proxy from the shell would intercept it.
    env = {k: v for k, v in env.items() if not k.lower().endswith("_proxy")}

    started = time.monotonic()
    proc = subprocess.Popen(
        ["bash", "-c", script],
        cwd=work,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=TARGET_SECONDS)
    except subprocess.TimeoutExpired:
        stdout, stderr = "", ""
    finally:
        # The quickstart stops its server; this stops anything it left.
        # An empty group is ESRCH, or EPERM on macOS once its leader is reaped.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        proc.wait()
    elapsed = time.monotonic() - started

    serve = work / "serve.json"
    token = json.loads(serve.read_text())["token"] if serve.is_file() else None
    output = stdout + stderr
    if token:
        output = output.replace(token, "REDACTED")
    assert proc.returncode == 0 and elapsed < TARGET_SECONDS, (
        f"the {name} quickstart failed (exit {proc.returncode}) or took {elapsed:.0f} s "
        f"of {TARGET_SECONDS} s:\n{output}"
    )
    record_measurement(f"{name} quickstart, start to finish", round(elapsed * 1000), "ms")

    # The doc reads the token from serve.json; nothing it runs prints it,
    # and the server never logs it.
    assert token, "the quickstart's server is not in dev mode"
    log = (work / "serve.log").read_text()
    assert_no_credential_in_log(stdout + stderr + log, {token})
