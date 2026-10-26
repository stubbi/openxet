"""Git leg: stock `git` over smart HTTP against `openxet serve` (ADR 0004,
ruling M2-R11).

The token is the Basic password, supplied by the harness's credential
helper after the server's `401`, as a user's credential helper would.
Several tests run two servers on one data directory, each with its own
Git cache, as two nodes behind one endpoint share a bucket and index.
"""

import random

import pytest

from conftest import MiB, openxet_serve
from git_harness import commit_file, openxet_git
from hub_harness import UPLOAD_FILE

pytestmark = pytest.mark.git


def node_config(root, name: str):
    """A dev-mode node whose Git cache is its own (docs/reference/config.md,
    `git.cache_dir`: one process holds a cache directory at a time)."""
    config = root / f"{name}.toml"
    config.write_text(f'[git]\ncache_dir = "{root / f"cache-{name}"}"\n')
    return config


def head_of(git, url: str, branch: str = "main") -> str:
    out = git("ls-remote", url, f"refs/heads/{branch}")
    return out.split()[0] if out else ""


def objects(git, repo) -> list[str]:
    return sorted(git("rev-list", "--objects", "--all", cwd=repo).splitlines())


def test_clone_push_pull_https(openxet_server, tmp_path):
    server = openxet_server
    git = openxet_git(tmp_path / "git", server.token, server.endpoint)
    url = f"{server.endpoint}/dev/hello.git"
    work = tmp_path / "work"
    git("init", "-q", str(work))
    commit_file(git, work, "README.md", b"hello\n", "first")
    # Large enough that git compresses the request body (ruling M2-R11).
    commit_file(git, work, "data/blob.bin", random.Random(31).randbytes(2 * MiB), "data")
    git("remote", "add", "origin", url, cwd=work)

    # The first push creates the repository (ruling M2-R11), with the
    # `model` label the Hub's default view needs.
    git("push", "-q", "origin", "main", cwd=work)
    assert head_of(git, url) == git("rev-parse", "HEAD", cwd=work).strip()

    clones = {}
    for protocol, address in (("2", url), ("0", url), ("2", url.removesuffix(".git"))):
        clone = tmp_path / f"clone-v{protocol}-{len(clones)}"
        git("-c", f"protocol.version={protocol}", "clone", "-q", address, str(clone))
        assert objects(git, clone) == objects(git, work), (protocol, address)
        clones[(protocol, address)] = clone

    clone = clones[("2", url)]
    pushed = commit_file(git, clone, "CHANGELOG.md", b"second\n", "second")
    git("push", "-q", "origin", "main", cwd=clone)
    git("pull", "-q", "--ff-only", "origin", "main", cwd=work)
    assert git("rev-parse", "HEAD", cwd=work).strip() == pushed
    assert (work / "CHANGELOG.md").read_bytes() == b"second\n"


def test_hub_commit_and_git_push_share_history(hub, tmp_path):
    alice = hub.users["alice"]
    repo = "alice/shared"
    (tmp_path / "from-hub.txt").write_text("from the Hub\n")
    hub.ok(alice, "api.create_repo(args['repo'])", repo=repo)
    hub_commit = hub.ok(
        alice,
        UPLOAD_FILE,
        repo_id=repo,
        path=str(tmp_path / "from-hub.txt"),
        path_in_repo="from-hub.txt",
        message="from hub",
    )

    git = openxet_git(tmp_path / "git", alice.token, hub.endpoint)
    clone = tmp_path / "clone"
    git("clone", "-q", f"{hub.endpoint}/{repo}.git", str(clone))
    assert git("rev-parse", "HEAD", cwd=clone).strip() == hub_commit
    assert (clone / "from-hub.txt").read_text() == "from the Hub\n"
    git_commit = commit_file(git, clone, "from-git.txt", b"from git\n", "from git")
    git("push", "-q", "origin", "main", cwd=clone)

    history = hub.ok(
        alice,
        "result = [[c.commit_id, c.title] for c in api.list_repo_commits(args['repo'])]",
        repo=repo,
    )
    assert [title for _, title in history] == ["from git", "from hub", "initial commit"]
    assert [commit for commit, _ in history[:2]] == [git_commit, hub_commit]

    # And back: a Hub commit on top of the pushed one fast-forwards git.
    (tmp_path / "again.txt").write_text("again\n")
    again = hub.ok(
        alice,
        UPLOAD_FILE,
        repo_id=repo,
        path=str(tmp_path / "again.txt"),
        path_in_repo="again.txt",
        message="again",
    )
    git("pull", "-q", "--ff-only", "origin", "main", cwd=clone)
    assert git("rev-parse", "HEAD~1", cwd=clone).strip() == git_commit
    assert git("rev-parse", "HEAD", cwd=clone).strip() == again


def test_two_nodes_concurrent_push_one_winner(openxet_bin, tmp_path):
    # ADR 0004 item 3: every ref writer goes through the fenced
    # transaction, so of two pushes to one branch through two nodes exactly
    # one lands, and the other is refused, never lost or merged blindly.
    data_dir = tmp_path / "shared-data"
    with (
        openxet_serve(openxet_bin, tmp_path / "a", node_config(tmp_path, "a"), data_dir) as a,
        openxet_serve(openxet_bin, tmp_path / "b", node_config(tmp_path, "b"), data_dir) as b,
    ):
        assert a.token == b.token, "two dev-mode nodes on one data directory share its token"
        git = openxet_git(tmp_path / "git", a.token, a.endpoint, b.endpoint)
        url_a, url_b = f"{a.endpoint}/dev/race.git", f"{b.endpoint}/dev/race.git"

        seed = tmp_path / "seed"
        git("init", "-q", str(seed))
        commit_file(git, seed, "base.txt", b"base\n", "base")
        git("push", "-q", url_a, "main", cwd=seed)

        clones = {}
        for name, url in (("a", url_a), ("b", url_b)):
            clones[name] = tmp_path / f"clone-{name}"
            git("clone", "-q", url, str(clones[name]))
            commit_file(git, clones[name], f"{name}.txt", name.encode(), f"from node {name}")

        # Both start before either is waited for, so they overlap.
        pushes = {name: git.popen("push", "origin", "main", cwd=c) for name, c in clones.items()}
        results = {}
        for name, push in pushes.items():
            _, stderr = push.communicate(timeout=600)
            results[name] = (push.returncode, git.redact(stderr))
        winners = [name for name, (code, _) in results.items() if code == 0]
        assert len(winners) == 1, results
        [winner] = winners
        [loser] = {"a", "b"} - {winner}
        assert "rejected" in results[loser][1] or "failed to push" in results[loser][1], results

        landed = git("rev-parse", "HEAD", cwd=clones[winner]).strip()
        assert head_of(git, url_a) == head_of(git, url_b) == landed

        # The refused writer catches up and lands on top: nothing was lost.
        git("pull", "-q", "--rebase", "origin", "main", cwd=clones[loser])
        git("push", "-q", "origin", "main", cwd=clones[loser])
        final = git("rev-parse", "HEAD", cwd=clones[loser]).strip()
        assert head_of(git, url_a) == head_of(git, url_b) == final
        assert git("rev-parse", "HEAD~1", cwd=clones[loser]).strip() == landed


def test_cold_node_clone_byte_identical(openxet_bin, tmp_path):
    # Non-negotiable 5 and design §5.3: a node with an empty Git cache
    # serves a repository another node wrote, from the bucket alone.
    data_dir = tmp_path / "shared-data"
    git = None
    with openxet_serve(openxet_bin, tmp_path / "a", node_config(tmp_path, "a"), data_dir) as a:
        git = openxet_git(tmp_path / "git", a.token, a.endpoint)
        work = tmp_path / "work"
        git("init", "-q", str(work))
        rng = random.Random(32)
        for i in range(3):
            commit_file(git, work, f"file-{i}.bin", rng.randbytes(MiB + i), f"commit {i}")
        git("rm", "-q", "file-0.bin", cwd=work)
        git("commit", "-q", "-m", "remove file-0", cwd=work)
        git("tag", "-a", "v1", "-m", "annotated", cwd=work)
        git("push", "-q", f"{a.endpoint}/dev/cold.git", "main", "v1", cwd=work)

    with openxet_serve(openxet_bin, tmp_path / "b", node_config(tmp_path, "b"), data_dir) as b:
        assert not any((tmp_path / "cache-b").glob("**/*.pack")), "node b's cache was warm"
        git = openxet_git(tmp_path / "git-b", b.token, b.endpoint)
        clone = tmp_path / "clone"
        git("clone", "-q", f"{b.endpoint}/dev/cold.git", str(clone))
        git("fetch", "-q", "origin", "refs/tags/v1:refs/tags/v1", cwd=clone)
        git("fsck", "--full", "--strict", cwd=clone)

    assert objects(git, clone) == objects(git, work)
    for path in work.glob("file-*.bin"):
        assert (clone / path.name).read_bytes() == path.read_bytes(), path.name


@pytest.mark.parametrize("protocol", ["0", "2"])
def test_force_pushed_commit_not_fetchable_by_sha(openxet_server, tmp_path, protocol):
    # Design §5.9 req 8 (R22): an object no ref reaches is never served,
    # even to a caller who knows its id.
    server = openxet_server
    git = openxet_git(tmp_path / "git", server.token, server.endpoint)
    url = f"{server.endpoint}/dev/forced.git"
    work = tmp_path / "work"
    git("init", "-q", str(work))
    commit_file(git, work, "base.txt", b"base\n", "base")
    dropped = commit_file(git, work, "secret.txt", b"pushed by mistake\n", "oops")
    git("push", "-q", url, "main", cwd=work)
    git("reset", "-q", "--hard", "HEAD~1", cwd=work)
    kept = commit_file(git, work, "fixed.txt", b"fixed\n", "fixed")
    git("push", "-q", "--force", url, "main", cwd=work)

    fresh = tmp_path / "fresh"
    git("init", "-q", str(fresh))
    fetch = ("-c", f"protocol.version={protocol}", "fetch", "-q", url)
    refused = git.run(*fetch, dropped, cwd=fresh)
    assert refused.returncode != 0, "the force-pushed commit was served by its id"
    assert git.run("cat-file", "-e", dropped, cwd=fresh).returncode != 0
    if protocol == "2":
        # v2 serves a reachable commit by its id, so the refusal above is
        # about reachability, not fetching by id at all.
        git(*fetch, kept, cwd=fresh)
        assert git("cat-file", "-t", kept, cwd=fresh).strip() == "commit"
