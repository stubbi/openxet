"""Agent leg: the openxet CLI must be usable by an agent (AGENTS.md, design 7.1).

Every new CLI command adds its non-interactive invocation to JSON_COMMANDS;
the Rust test `every_subcommand_is_in_the_agent_leg` fails until it does.
"""

import json
import subprocess

import pytest

pytestmark = pytest.mark.agent

JSON_COMMANDS = [
    ["version", "--json"],
]


def run(openxet_bin, *args):
    return subprocess.run(
        [openxet_bin, *args],
        capture_output=True,
        stdin=subprocess.DEVNULL,
        timeout=30,
    )


@pytest.mark.parametrize("args", JSON_COMMANDS, ids=" ".join)
def test_command_json_is_parseable_with_schema(openxet_bin, args):
    # run() closes stdin and has no tty: a command that prompts would hang
    # until the timeout and fail here.
    out = run(openxet_bin, *args)

    assert out.returncode == 0, out.stderr.decode()
    schema = json.loads(out.stdout)["schema"]
    assert schema.startswith("openxet.") and schema.endswith(".v1")


def test_usage_error_with_json_is_structured(openxet_bin):
    out = run(openxet_bin, "nope", "--json")

    assert out.returncode == 2
    error = json.loads(out.stderr)
    assert error["schema"] == "openxet.error.v1"
    assert error["hint"]
