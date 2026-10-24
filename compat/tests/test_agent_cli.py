"""Agent leg: the openxet CLI must be usable by an agent (AGENTS.md, design 7.1).

Every new CLI command adds its non-interactive invocation to JSON_COMMANDS;
the Rust test `every_subcommand_is_in_the_agent_leg` fails until it does.
"""

import contextlib
import json
import os
import signal
import subprocess

import pytest

pytestmark = pytest.mark.agent

# `{tmp}` is a fresh directory per command, and `{key}` a signing key
# `token keygen` wrote there first. Commands in NEEDS_SERVER run against a
# dev-mode `openxet serve` through OPENXET_ENDPOINT and OPENXET_TOKEN, with
# `{tmp}/upload.bin` to upload and `{hash}` the Xet hash of it, uploaded
# first.
JSON_COMMANDS = [
    ["version", "--json"],
    ["config", "check", "--json"],
    ["token", "keygen", "--key", "{tmp}/signing.key", "--json"],
    ["token", "mint", "--key", "{key}", "--scope", "agent", "--mode", "read", "--ttl", "1h", "--json"],
    ["token", "generate", "--scope", "agent", "--mode", "write", "--subject", "agent", "--json"],
    ["serve", "--data-dir", "{tmp}/data", "--port", "0", "--json"],
    ["upload", "{tmp}/upload.bin", "--json"],
    ["download", "{hash}", "--output", "{tmp}/download.bin", "--json"],
]

NEEDS_SERVER = {"upload", "download"}

# Commands that run until stopped. Their first stdout line is the result;
# SIGTERM then stops them cleanly.
LONG_RUNNING = {"serve"}


def run(openxet_bin, *args, env=None):
    return subprocess.run(
        [openxet_bin, *args],
        capture_output=True,
        stdin=subprocess.DEVNULL,
        timeout=60,
        env=env,
    )


def expand(openxet_bin, args, tmp_path, env=None):
    if any("{key}" in a for a in args):
        key = tmp_path / "mint.key"
        out = run(openxet_bin, "token", "keygen", "--key", str(key), "--json")
        assert out.returncode == 0, out.stderr.decode()
        args = [a.replace("{key}", str(key)) for a in args]
    if any("{hash}" in a for a in args):
        out = run(openxet_bin, "upload", str(tmp_path / "upload.bin"), "--json", env=env)
        assert out.returncode == 0, out.stderr.decode()
        file_hash = json.loads(out.stdout)["files"][0]["hash"]
        args = [a.replace("{hash}", file_hash) for a in args]
    return [a.replace("{tmp}", str(tmp_path)) for a in args]


@contextlib.contextmanager
def dev_server(openxet_bin, tmp_path):
    """The environment for a client of a dev-mode server, while it runs."""
    proc = subprocess.Popen(
        [openxet_bin, "serve", "--data-dir", str(tmp_path / "server"), "--port", "0", "--json"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    try:
        ready = json.loads(proc.stdout.readline())
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("OPENXET_", "HF_")) and not k.lower().endswith("_proxy")
        }
        env.update(
            OPENXET_ENDPOINT=ready["endpoint"],
            OPENXET_TOKEN=ready["token"],
            HF_XET_CACHE=str(tmp_path / "xet-cache"),
        )
        yield env
    finally:
        proc.kill()
        proc.wait()


def run_long(openxet_bin, *args):
    """The first stdout line, and the exit code after SIGTERM."""
    proc = subprocess.Popen(
        [openxet_bin, *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        line = proc.stdout.readline()
        proc.send_signal(signal.SIGTERM)
        _, stderr = proc.communicate(timeout=60)
    finally:
        proc.kill()
    return line, proc.returncode, stderr


@pytest.mark.parametrize("args", JSON_COMMANDS, ids=" ".join)
def test_command_json_is_parseable_with_schema(openxet_bin, args, tmp_path):
    # stdin is closed and there is no tty: a command that prompts would hang
    # until the timeout and fail here.
    with contextlib.ExitStack() as stack:
        env = None
        if args[0] in NEEDS_SERVER:
            env = stack.enter_context(dev_server(openxet_bin, tmp_path))
            (tmp_path / "upload.bin").write_bytes(os.urandom(4096))
        args = expand(openxet_bin, args, tmp_path, env)
        if args[0] in LONG_RUNNING:
            stdout, returncode, stderr = run_long(openxet_bin, *args)
        else:
            out = run(openxet_bin, *args, env=env)
            stdout, returncode, stderr = out.stdout, out.returncode, out.stderr

    assert returncode == 0, stderr.decode()
    schema = json.loads(stdout)["schema"]
    assert schema.startswith("openxet.") and schema.endswith(".v1")


def test_usage_error_with_json_is_structured(openxet_bin):
    out = run(openxet_bin, "nope", "--json")

    assert out.returncode == 2
    error = json.loads(out.stderr)
    assert error["schema"] == "openxet.error.v1"
    assert error["hint"]
