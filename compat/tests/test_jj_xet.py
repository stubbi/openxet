"""jj-xet's contract with openxet, replayed over HTTP against `openxet serve`.

jj-xet (a Jujutsu extension storing large files in Xet) tests itself
against a mock of openxet (`tests/openxet.rs` in jj-xet). Task 14
transcribed that mock's expectations into
`compat/fixtures/lfs/jj-xet-expectations.json`, which `crates/hub/tests/
lfs.rs` replays in process; this test replays the same file against the
real server, so the mock, the server and this file can't drift apart.

The file's format:

    {"source": "<where the expectations come from>",
     "cases": [{"name": "...",
                "request": {"method": "GET" | "POST",
                            "path": "<relative to the LFS URL>",
                            "auth": "basic" | "wrong-basic" | "none",
                            "json": <body, optional>},
                "response": {"status": 200,
                             "json": {"<dotted.path>": <expected>, ...}}}]}

The LFS URL is `{endpoint}/{namespace}/{name}.git/info/lfs`. In a request
body, `"{oid}"`, `"{size}"` and `"{unknown_oid}"` stand for the sha256 and
size of a file uploaded to the repository and a sha256 it never saw; in an
expected value, `"{xet_hash}"` stands for that file's Xet hash, and
`"<string>"`, `"<int>"`, `"<future-epoch>"` and `"<http-url>"` for any
value of that kind. A dotted path's numeric parts index lists.
"""

import json
import time
from pathlib import Path

import pytest

from conftest import MiB, sha256, write_noise
from hub_harness import CREATE, PATHS_INFO, UPLOAD_FILE, new_token

pytestmark = pytest.mark.lfs

EXPECTATIONS = Path(__file__).parent.parent / "fixtures" / "lfs" / "jj-xet-expectations.json"
LFS_MEDIA_TYPE = "application/vnd.git-lfs+json"


def substitute(value, values: dict):
    if isinstance(value, str) and value in values:
        return values[value]
    if isinstance(value, dict):
        return {k: substitute(v, values) for k, v in value.items()}
    if isinstance(value, list):
        return [substitute(v, values) for v in value]
    return value


def at(body, dotted: str):
    for part in dotted.split("."):
        body = body[int(part)] if isinstance(body, list) else body[part]
    return body


def matches(got, expected) -> bool:
    match expected:
        case "<string>":
            return isinstance(got, str) and bool(got)
        case "<int>":
            return isinstance(got, int) and not isinstance(got, bool)
        case "<future-epoch>":
            return isinstance(got, int) and got > time.time()
        case "<http-url>":
            return isinstance(got, str) and got.startswith(("http://", "https://"))
        case _:
            return got == expected


def test_jj_xet_expectations(hub, tmp_path):
    cases = json.loads(EXPECTATIONS.read_text())["cases"]
    assert cases, f"{EXPECTATIONS} holds no cases"
    alice = hub.users["alice"]
    path = write_noise(tmp_path / "big.payload", 2 * MiB, 51)
    hub.ok(alice, CREATE, repo_id="alice/jj")
    hub.ok(alice, UPLOAD_FILE, repo_id="alice/jj", path=str(path), path_in_repo=path.name)
    info = hub.ok(alice, PATHS_INFO, repo_id="alice/jj", paths=[path.name])[path.name]
    values = {
        "{oid}": sha256(path),
        "{size}": path.stat().st_size,
        "{unknown_oid}": "0" * 63 + "1",
        "{xet_hash}": info["xet_hash"],
    }
    wrong = new_token()
    hub.server.uses_credential(wrong)
    auth = {"basic": ("jj", alice.token), "wrong-basic": ("jj", wrong), "none": None}

    for case in cases:
        request, expected = case["request"], case["response"]
        headers = {"Accept": LFS_MEDIA_TYPE}
        if "json" in request:
            headers["Content-Type"] = LFS_MEDIA_TYPE
        answer = hub.request(
            None,
            request["method"],
            f"/alice/jj.git/info/lfs/{request['path']}",
            auth=auth[request["auth"]],
            headers=headers,
            content=json.dumps(substitute(request["json"], values)) if "json" in request else None,
        )
        assert answer.status_code == expected["status"], (case["name"], answer.text)
        body = answer.json()
        for dotted, want in expected.get("json", {}).items():
            got = at(body, dotted)
            assert matches(got, substitute(want, values)), (case["name"], dotted, got)
