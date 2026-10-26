"""LFS leg (ADR 0003): Git repositories on Gitea, their LFS objects on
openxet through `lfs.url`, pushed with `git-xet` and pulled with `git-lfs`.

Each clone is configured with what `openxet setup git` (M6) will write
(ADR 0003 decision 4): `lfs.url`, and
`lfs.<the LFS endpoint git-xet derives from the remote>.access=none`, with
which `git-xet` calls the capability `href` without a credential; the
openxet token is in the credential helper under the openxet host only.
Gitea itself runs from compat/gitea/compose.yaml.
"""

import secrets
from dataclasses import dataclass
from pathlib import Path
from subprocess import CompletedProcess

import pytest

from conftest import MiB, sha256, write_noise
from git_harness import Credential, Git, commit_file
from hub_harness import hub_server

pytestmark = pytest.mark.lfs

# git-xet refreshes its CAS token when it expires within 30 s (S7 answer 3,
# `utils/src/auth.rs` at git-xet-v0.2.1), so a token this short makes it
# call the capability `href` as soon as the upload starts.
SHORT_TOKEN_TTL = '[hub]\ntoken_ttl = "5s"\n'

CAPABILITY_ROUTE_PREFIX = "/v1/capabilities/"
BATCH_ROUTE_SUFFIX = "/info/lfs/objects/batch"
GATEWAY_ROUTE_PREFIX = "/v1/transfer/files/"


@pytest.fixture
def lfs_hub(openxet_bin, tmp_path, lfs_clients):
    with hub_server(openxet_bin, tmp_path / "openxet", SHORT_TOKEN_TTL) as hub:
        yield hub


def git_for(hub, gitea, root) -> Git:
    alice = hub.users["alice"]
    return Git(
        root,
        [
            Credential(hub.endpoint.split("://", 1)[1], "openxet", "OPENXET_COMPAT_TOKEN"),
            gitea.credential(),
        ],
        {"OPENXET_COMPAT_TOKEN": alice.token, "OPENXET_COMPAT_GITEA_PASSWORD": gitea.password},
    )


def set_up_clone(git: Git, clone, remote: str, lfs_url: str, xet: bool) -> None:
    """The clone's git config, as `openxet setup git` will write it, and
    `git-xet` registered the way `git xet install` registers it (S7)."""
    git("lfs", "install", "--local", cwd=clone)
    git("config", "lfs.url", lfs_url, cwd=clone)
    git("config", f"lfs.{remote}/info/lfs.access", "none", cwd=clone)
    if xet:
        for key, value in (("path", "git-xet"), ("args", "transfer"), ("concurrent", "true")):
            git("config", f"lfs.customtransfer.xet.{key}", value, cwd=clone)


@dataclass
class Pushed:
    git: Git
    repo: str  # alice's repository on openxet, and the Gitea repository's name
    remote: str
    lfs_url: str
    file: Path
    push: CompletedProcess


def push_lfs_file(hub, gitea, tmp_path, base: str, xet: bool) -> Pushed:
    """A new Gitea repository with one LFS file, pushed from a clone set
    up for openxet's `alice/{name}`. The name is unique, so one Gitea
    serves any number of runs."""
    name = f"{base}-{secrets.token_hex(4)}"
    git = git_for(hub, gitea, tmp_path / "git")
    remote = gitea.create_repo(name)
    lfs_url = f"{hub.endpoint}/alice/{name}.git/info/lfs"
    work = tmp_path / "work"
    git("init", "-q", str(work))
    git("remote", "add", "origin", remote, cwd=work)
    set_up_clone(git, work, remote, lfs_url, xet)
    git("lfs", "track", "*.bin", cwd=work)
    git("add", ".gitattributes", cwd=work)
    big = write_noise(tmp_path / "big.bin", 3 * MiB + 5, 41)
    commit_file(git, work, "big.bin", big.read_bytes(), "add big.bin")
    push = git.run("push", "origin", "main", cwd=work)
    return Pushed(git, f"alice/{name}", remote, lfs_url, big, push)


def test_git_xet_push_refresh_without_authorization(lfs_hub, gitea, tmp_path):
    hub = lfs_hub
    mark = hub.server.mark()
    pushed = push_lfs_file(hub, gitea, tmp_path, "xet-push", xet=True)
    assert pushed.push.returncode == 0, pushed.push.stderr
    hub.server.settle()

    answered = hub.server.answered(mark)
    batches = [s for m, r, s in answered if m == "POST" and r.endswith(BATCH_ROUTE_SUFFIX)]
    assert batches and set(batches) <= {200, 401}, answered
    # The refresh: git-xet called the capability href, and it answered.
    refreshes = [s for m, r, s in answered if r.startswith(CAPABILITY_ROUTE_PREFIX)]
    assert refreshes and set(refreshes) == {200}, answered
    # ADR 0003 decision 2: the route records only whether a request carried
    # an Authorization header; with `access=none` none did.
    presence = [
        entry["fields"]["authorization_present"]
        for entry in hub.server.log()[mark:]
        if "authorization_present" in entry.get("fields", {})
    ]
    assert len(presence) == len(refreshes) and not any(presence), presence

    # The first push created the repository (ruling M2-R11) and registered
    # the file to it.
    alice = hub.users["alice"]
    info = hub.ok(alice, "result = api.model_info(args['repo']).id", repo=pushed.repo)
    assert info == pushed.repo
    big = pushed.file
    answer = hub.request(
        alice,
        "POST",
        f"/{pushed.repo}.git/info/lfs/objects/batch",
        headers={"Accept": "application/vnd.git-lfs+json"},
        json={
            "operation": "download",
            "transfers": ["basic"],
            "objects": [{"oid": sha256(big), "size": big.stat().st_size}],
        },
    )
    assert answer.status_code == 200, answer.text
    [entry] = answer.json()["objects"]
    assert "error" not in entry, entry


def test_git_lfs_pull_basic(lfs_hub, gitea, tmp_path):
    # ADR 0003 decision 3: downloads answer `basic` even with git-xet
    # registered, whose 0.2.1 can't download; git-lfs fetches the gateway
    # `href` with the credential it holds for the openxet host.
    hub = lfs_hub
    pushed = push_lfs_file(hub, gitea, tmp_path, "pull", xet=True)
    assert pushed.push.returncode == 0, pushed.push.stderr
    git, remote, big = pushed.git, pushed.remote, pushed.file

    clone = tmp_path / "clone"
    git("clone", "-q", remote, str(clone), env={"GIT_LFS_SKIP_SMUDGE": "1"})
    assert f"oid sha256:{sha256(big)}" in (clone / "big.bin").read_text(), "not a pointer"
    set_up_clone(git, clone, remote, pushed.lfs_url, xet=True)

    mark = hub.server.mark()
    git("lfs", "pull", cwd=clone)
    assert (clone / "big.bin").read_bytes() == big.read_bytes()
    hub.server.settle()
    downloads = [s for m, r, s in hub.server.answered(mark) if r.startswith(GATEWAY_ROUTE_PREFIX)]
    # git-lfs 3.7.1 sends an action `href` without a credential first and
    # adds the one it holds for the host after the gateway's 401 (design
    # §5.9 req 1: a gateway URL is never served without one), as it does
    # for the batch URL itself.
    assert downloads and downloads[-1] in {200, 206}, hub.server.answered(mark)
    assert set(downloads) <= {200, 206, 401}, hub.server.answered(mark)


def test_plain_git_lfs_push_gets_install_hint(lfs_hub, gitea, tmp_path):
    # Ruling M2-R10: until server-side chunking (M4), an upload without
    # git-xet is refused up front with the install command, never given a
    # `basic` href that fails later.
    hub = lfs_hub
    pushed = push_lfs_file(hub, gitea, tmp_path, "plain", xet=False)
    stderr = pushed.push.stderr
    assert pushed.push.returncode != 0, "a push without git-xet succeeded"
    assert "git-xet" in stderr and "install" in stderr, stderr
    # git-lfs's pre-push hook failed, so the Git push never happened.
    assert pushed.git("ls-remote", pushed.remote, "refs/heads/main") == ""
