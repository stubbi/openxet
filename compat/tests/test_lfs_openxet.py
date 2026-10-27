"""LFS leg, OpenXet-hosted: a repository whose Git and LFS both live on
OpenXet, written with stock `git`, `git-lfs` and `git-xet` as design §12
says kernels are written, and read back with stock `huggingface_hub` and
a fresh `git clone`.

No `lfs.url` and no `access` override: `git-lfs` derives the LFS endpoint
from the OpenXet remote and authenticates to it with the token the
credential helper holds for the OpenXet host, as a user's setup after
`git xet install` does (docs/reference/lfs.md).
"""

import json
import re

import pytest

from conftest import XORB_ROUTE, MiB, sha256, write_noise
from git_harness import commit_file, openxet_git
from hub_harness import CREATE, DOWNLOAD, PATHS_INFO

pytestmark = pytest.mark.lfs

BATCH_ROUTE_SUFFIX = "/info/lfs/objects/batch"
SHARD_ROUTES = {"/shards", "/v1/shards", "/v2/shards"}
GATEWAY_ROUTE_PREFIX = "/v1/transfer/files/"
# A JWT, as the CAS tokens are (ADR 0007), and a signed URL's signature.
JWT = re.compile(r"eyJ[\w-]+\.[\w-]+\.[\w-]+")
SIGNATURE = re.compile(r"[?&]sig=")


def test_git_xet_push_to_openxet_hosted_kernel(hub, lfs_clients, tmp_path):
    alice = hub.users["alice"]
    hub.ok(alice, CREATE, repo_id="alice/kern", repo_type="kernel")

    git = openxet_git(tmp_path / "git", alice.token, hub.endpoint)
    # Both write to the isolated HOME's global config, as a user runs them;
    # `--skip-repo` keeps git-lfs out of the repository pytest runs in.
    git("lfs", "install", "--skip-repo")
    git("xet", "install")
    remote = f"{hub.endpoint}/kernels/alice/kern.git"
    work = tmp_path / "work"
    git("clone", "-q", remote, str(work))
    git("lfs", "track", "*.so", cwd=work)
    git("add", ".gitattributes", cwd=work)
    # Over the 1 MiB `hub.lfs_threshold`, so the push must take the Xet path.
    kernel = write_noise(tmp_path / "kernel.so", 3 * MiB + 7, 114)
    data = kernel.read_bytes()
    commit_file(git, work, "build/kernel.so", data, "add the kernel")
    committed = git("show", "HEAD:build/kernel.so", cwd=work)
    assert f"oid sha256:{sha256(kernel)}" in committed, "not committed as an LFS pointer"

    mark = hub.server.mark()
    push = git.run("push", "origin", "main", cwd=work)
    assert push.returncode == 0, push.stderr
    # git-lfs's pre-push asks `locks/verify` first. OpenXet has no locking
    # API, and its 404 makes git-lfs 3.7.1 disable lock verification
    # quietly (`SearchLocksVerifiable` in locking/locks.go treats 404 like
    # 501); any other refusal would print this advice on every push.
    assert "does not support the Git LFS locking API" not in push.stderr, push.stderr
    # The quiet path records itself in the clone (`disableFor` in
    # commands/lockverifier.go), so this is git-lfs's answer, not silence.
    verify = git("config", "--get", f"lfs.{remote}/info/lfs.locksverify", cwd=work)
    assert verify.strip() == "false", verify
    hub.server.settle()
    answered = hub.server.answered(mark)
    batches = [s for m, r, s in answered if m == "POST" and r.endswith(BATCH_ROUTE_SUFFIX)]
    assert batches and batches[-1] == 200, answered
    # git-xet, not a `basic` fallback, carried the bytes: a xorb and the
    # shard registering it reached the CAS.
    assert hub.server.requests("POST", XORB_ROUTE, mark) > 0, answered
    shards = [s for m, r, s in answered if m == "POST" and r in SHARD_ROUTES]
    assert shards and set(shards) == {200}, answered

    info = hub.ok(
        alice, PATHS_INFO, repo_id="alice/kern", paths=["build/kernel.so"], repo_type="kernel"
    )
    entry = info["build/kernel.so"]
    assert entry["sha256"] == sha256(kernel) and entry["size"] == len(data), entry
    assert entry["xet_hash"], entry

    download = hub.python_run(
        alice, DOWNLOAD, repo_id="alice/kern", filename="build/kernel.so", repo_type="kernel"
    )
    assert download.returncode == 0 and '"ok"' in download.stdout, download.stderr
    local = json.loads(download.stdout.strip().splitlines()[-1])["ok"]
    assert open(local, "rb").read() == data
    assert download.xet_calls("download") == [entry["xet_hash"]]

    # A fresh clone smudges the file through git-lfs: git-xet 0.2.1 can't
    # download, so the server answers `basic` with the gateway `href`
    # (ADR 0003 decision 3).
    mark = hub.server.mark()
    fresh = tmp_path / "fresh"
    git("clone", "-q", remote, str(fresh))
    assert (fresh / "build" / "kernel.so").read_bytes() == data
    hub.server.settle()
    gateway = [s for m, r, s in hub.server.answered(mark) if r.startswith(GATEWAY_ROUTE_PREFIX)]
    assert gateway and gateway[-1] in {200, 206}, hub.server.answered(mark)

    # Design §5.9 req 6. The users' tokens are the harness's; the CAS
    # tokens and capability `href`s the server handed git-lfs and git-xet
    # exist only in those processes, so the log must hold nothing shaped
    # like either.
    hub.server.assert_no_credential_logged()
    log = "\n".join(json.dumps(entry) for entry in hub.server.log())
    assert not JWT.search(log), "the server log holds a JWT"
    assert not SIGNATURE.search(log), "the server log holds a signed URL"
