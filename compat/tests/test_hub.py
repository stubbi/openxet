"""Hub leg: stock `huggingface_hub` and `hf` against `openxet serve`.

Each test starts its own configured server (hub_harness.RULES: alice owns
`alice/*` and `acme/*`, bob owns `bob/*`, anyone reads `alice/public-*`),
and every client run is a fresh process with a fresh cache, so what a
client reads can only come from the server.
"""

import json
import random
import re
import time
from pathlib import Path

import httpx
import pytest

from conftest import RECONSTRUCTION_ROUTE, XORB_ROUTE, MiB, sha256, write_noise
from git_harness import commit_file, openxet_git
from hub_harness import CREATE, DOWNLOAD, PATHS_INFO, SNAPSHOT, UPLOAD_FILE, UPLOAD_FOLDER, User

pytestmark = pytest.mark.hub

GiB = 1 << 30


def folder_with_regular_and_xet_files(root):
    """Small files travel inside the commit; `big.payload` is over the
    1 MiB `hub.lfs_threshold` and matches no `.gitattributes` pattern, so
    only its size routes it to Xet (ruling M2-R4)."""
    root.mkdir(parents=True)
    (root / "README.md").write_text("# compat\n")
    (root / "config.json").write_text(json.dumps({"hidden_size": 8}))
    (root / "sub" / "données é").mkdir(parents=True)
    (root / "sub" / "données é" / "notes.txt").write_text("UTF-8 path, with a space\n")
    write_noise(root / "big.payload", 2 * MiB + 17, 21)
    return {
        p.relative_to(root).as_posix(): p.read_bytes()
        for p in sorted(root.rglob("*"))
        if p.is_file()
    }


def assert_tree_matches(files: dict[str, bytes], snapshot: str) -> None:
    for name, data in files.items():
        got = (Path(snapshot) / name).read_bytes()
        assert got == data, f"{name}: {len(got)} bytes differ from the {len(data)} uploaded"


def test_auth_login_whoami(hub):
    alice = hub.users["alice"]
    root = hub.fresh_root()

    login = hub.hf(None, "auth", "login", "--token", alice.token, root=root)
    assert login.returncode == 0, login.stderr
    whoami = hub.hf(None, "auth", "whoami", "--format", "json", root=root)
    assert whoami.returncode == 0, whoami.stderr
    shown = json.loads(whoami.stdout.strip().splitlines()[-1])
    assert shown == {"user": "alice", "orgs": "acme", "endpoint": hub.endpoint}

    # S3 lesson 2: every field huggingface_hub reads.
    info = hub.ok(alice, "result = api.whoami()")
    assert info["name"] == "alice"
    assert [org["name"] for org in info["orgs"]] == ["acme"]

    # S3 answer 6: an unknown token is an authentication error, not a
    # missing repository.
    hub.ok(alice, CREATE, repo_id="alice/exists")
    mallory = User("mallory", "oxt_" + "A" * 43)
    hub.server.uses_credential(mallory.token)
    refused = hub.fails(mallory, "api.model_info('alice/exists')")
    assert (refused["error"], refused["status"]) == ("HfHubHTTPError", 401), refused


def test_upload_folder_and_snapshot_download(hub, tmp_path):
    alice = hub.users["alice"]
    files = folder_with_regular_and_xet_files(tmp_path / "folder")
    hub.ok(alice, CREATE, repo_id="alice/folder")

    mark = hub.server.mark()
    upload = hub.python_run(
        alice, UPLOAD_FOLDER, repo_id="alice/folder", folder=str(tmp_path / "folder")
    )
    assert upload.returncode == 0 and '"ok"' in upload.stdout, upload.stdout + upload.stderr
    big = (tmp_path / "folder" / "big.payload").resolve()
    assert upload.xet_calls("upload") == [str(big)], "only big.payload goes through Xet"
    hub.server.settle()
    assert hub.server.requests("POST", XORB_ROUTE, mark) > 0

    info = hub.ok(alice, PATHS_INFO, repo_id="alice/folder", paths=sorted(files))
    assert info["big.payload"]["sha256"] == sha256(tmp_path / "folder" / "big.payload")
    assert info["big.payload"]["xet_hash"]
    assert all(info[name]["xet_hash"] is None for name in files if name != "big.payload")

    download = hub.python_run(alice, SNAPSHOT, repo_id="alice/folder")
    snapshot = json.loads(download.stdout.strip().splitlines()[-1])["ok"]
    assert_tree_matches(files, snapshot)
    assert download.xet_calls("download") == [info["big.payload"]["xet_hash"]]
    # Ruling M2-R5: the repository starts from huggingface.co's default.
    assert "filter=lfs" in (Path(snapshot) / ".gitattributes").read_text()


def test_one_gib_upload_takes_xet_path(hub, tmp_path, record_measurement):
    # S3 lesson 1: a file of at least 1 GiB uploads through Xet. Its name
    # matches no `.gitattributes` pattern, so `preupload` must route it by
    # size alone.
    alice = hub.users["alice"]
    path = write_noise(tmp_path / "one-gib.payload", GiB, 22)
    hub.ok(alice, CREATE, repo_id="alice/big")

    mark = hub.server.mark()
    started = time.monotonic()
    upload = hub.python_run(
        alice, UPLOAD_FILE, repo_id="alice/big", path=str(path), path_in_repo=path.name
    )
    record_measurement("1 GiB hf upload, debug server", int(time.monotonic() - started), "s")
    assert '"ok"' in upload.stdout, upload.stdout + upload.stderr
    assert upload.xet_calls("upload") == [str(path.resolve())], (
        "huggingface_hub never handed it to hf_xet"
    )
    hub.server.settle()
    assert hub.server.xorb_bytes_accepted(mark) >= GiB, "the file's bytes never reached the CAS"

    info = hub.ok(alice, PATHS_INFO, repo_id="alice/big", paths=[path.name])[path.name]
    assert info["size"] == GiB and info["sha256"] == sha256(path)


def test_upload_dedups_across_repos(hub, tmp_path):
    # Review Focus 1: a file alice already uploaded to one repository is
    # sent to another as a shard alone, and the commit there succeeds
    # because the shard registers the file to the second repository too.
    alice = hub.users["alice"]
    path = write_noise(tmp_path / "shared.payload", 16 * MiB, 23)
    for repo in ("alice/first", "alice/second"):
        hub.ok(alice, CREATE, repo_id=repo)
    hub.ok(alice, UPLOAD_FILE, repo_id="alice/first", path=str(path), path_in_repo=path.name)

    mark = hub.server.mark()
    hub.ok(alice, UPLOAD_FILE, repo_id="alice/second", path=str(path), path_in_repo=path.name)
    hub.server.settle()
    assert hub.server.xorb_bytes_accepted(mark) < 2_000_000, "the second upload resent the file"

    local = hub.ok(alice, DOWNLOAD, repo_id="alice/second", filename=path.name)
    assert sha256(local) == sha256(path)


def test_branches_tags_refs_commits(hub, tmp_path):
    alice = hub.users["alice"]
    repo = "alice/refs"
    (tmp_path / "a.txt").write_text("on main\n")
    (tmp_path / "b.txt").write_text("on dev\n")
    hub.ok(alice, CREATE, repo_id=repo)
    first = hub.ok(
        alice,
        UPLOAD_FILE,
        repo_id=repo,
        path=str(tmp_path / "a.txt"),
        path_in_repo="a.txt",
        message="add a",
    )
    hub.ok(alice, "api.create_branch(args['repo'], branch='dev')", repo=repo)
    hub.ok(alice, "api.create_tag(args['repo'], tag='v1', tag_message='first')", repo=repo)
    second = hub.ok(
        alice,
        UPLOAD_FILE,
        repo_id=repo,
        path=str(tmp_path / "b.txt"),
        path_in_repo="b.txt",
        revision="dev",
        message="add b",
    )

    refs = hub.ok(
        alice,
        """
refs = api.list_repo_refs(args["repo"])
result = {
    "branches": {b.name: b.target_commit for b in refs.branches},
    "tags": {t.name: t.target_commit for t in refs.tags},
}
""",
        repo=repo,
    )
    assert refs["branches"] == {"main": first, "dev": second}
    # As on huggingface.co (S3, fixture 14), a tag's `targetCommit` is the
    # annotated tag object, which `create_tag` with a message writes; the
    # commit it names is the one `v1`'s history starts from, below.
    assert set(refs["tags"]) == {"v1"} and refs["tags"]["v1"] != first, refs

    titles = """
result = [c.title for c in api.list_repo_commits(args["repo"], revision=args["rev"])]
"""
    assert hub.ok(alice, titles, repo=repo, rev="dev") == ["add b", "add a", "initial commit"]
    assert hub.ok(alice, titles, repo=repo, rev="v1") == ["add a", "initial commit"]
    tagged = "result = api.list_repo_commits(args['repo'], revision='v1')[0].commit_id"
    assert hub.ok(alice, tagged, repo=repo) == first

    conflict = hub.fails(alice, "api.create_branch(args['repo'], branch='dev')", repo=repo)
    assert conflict["status"] == 409, conflict

    local = hub.ok(alice, DOWNLOAD, repo_id=repo, filename="b.txt", revision="dev")
    assert open(local).read() == "on dev\n"

    hub.ok(alice, "api.delete_tag(args['repo'], tag='v1')", repo=repo)
    hub.ok(alice, "api.delete_branch(args['repo'], branch='dev')", repo=repo)
    refs = hub.ok(
        alice,
        "r = api.list_repo_refs(args['repo']); result = [[b.name for b in r.branches], r.tags]",
        repo=repo,
    )
    assert refs == [["main"], []]


def test_list_tree_paths_info(hub, tmp_path):
    alice = hub.users["alice"]
    files = folder_with_regular_and_xet_files(tmp_path / "folder")
    hub.ok(alice, CREATE, repo_id="alice/tree")
    hub.ok(alice, UPLOAD_FOLDER, repo_id="alice/tree", folder=str(tmp_path / "folder"))

    listed = hub.ok(alice, "result = sorted(api.list_repo_files(args['repo']))", repo="alice/tree")
    assert listed == sorted([*files, ".gitattributes"])

    top = hub.ok(
        alice,
        "result = sorted((e.path, type(e).__name__) for e in api.list_repo_tree(args['repo']))",
        repo="alice/tree",
    )
    assert top == [
        [".gitattributes", "RepoFile"],
        ["README.md", "RepoFile"],
        ["big.payload", "RepoFile"],
        ["config.json", "RepoFile"],
        ["sub", "RepoFolder"],
    ]

    deep = hub.ok(
        alice,
        """
result = {
    e.path: getattr(e, "size", None)
    for e in api.list_repo_tree(args["repo"], path_in_repo="sub", recursive=True, expand=True)
}
""",
        repo="alice/tree",
    )
    notes = "sub/données é/notes.txt"
    assert deep == {"sub/données é": None, notes: len(files[notes])}

    info = hub.ok(alice, PATHS_INFO, repo_id="alice/tree", paths=["sub", "big.payload", "nope"])
    assert set(info) == {"sub", "big.payload"}, "paths-info lists only paths that exist"
    assert info["big.payload"]["size"] == len(files["big.payload"])


def test_hf_download_cli(hub, tmp_path):
    alice = hub.users["alice"]
    files = folder_with_regular_and_xet_files(tmp_path / "folder")
    up = hub.hf(alice, "upload", "alice/cli", str(tmp_path / "folder"), ".")
    assert up.returncode == 0, up.stderr

    mark = hub.server.mark()
    out = tmp_path / "out"
    down = hub.hf(
        alice, "download", "alice/cli", "big.payload", "README.md", "--local-dir", str(out)
    )
    assert down.returncode == 0, down.stderr
    for name in ("big.payload", "README.md"):
        assert (out / name).read_bytes() == files[name], name
    hub.server.settle()
    assert hub.server.requests("GET", RECONSTRUCTION_ROUTE, mark) > 0, (
        "hf download fetched big.payload without hf_xet"
    )


def labels(hub, user, repo: str) -> list[str]:
    answer = hub.request(user, "GET", f"/api/openxet/v1/repos/{repo}/labels")
    assert answer.status_code == 200, answer.text
    return answer.json()["labels"]


def test_untyped_create_is_model(hub, tmp_path):
    alice = hub.users["alice"]
    (tmp_path / "f.txt").write_text("untyped\n")

    up = hub.hf(alice, "upload", "alice/untyped", str(tmp_path / "f.txt"))
    assert up.returncode == 0, up.stderr

    assert labels(hub, alice, "alice/untyped") == ["model"]
    # A create body without `type` at all, as other clients send it.
    raw = hub.request(alice, "POST", "/api/repos/create", json={"name": "raw"})
    assert raw.status_code == 200, raw.text
    assert labels(hub, alice, "alice/raw") == ["model"]
    assert hub.ok(alice, "result = api.model_info('alice/untyped').id") == "alice/untyped"
    snapshot = hub.ok(alice, SNAPSHOT, repo_id="alice/untyped")
    assert (Path(snapshot) / "f.txt").read_text() == "untyped\n"


def test_typed_route_view_filters_by_label(hub):
    alice = hub.users["alice"]
    hub.ok(alice, CREATE, repo_id="alice/m1")
    hub.ok(alice, CREATE, repo_id="alice/d1", repo_type="dataset")
    listing = """
result = sorted(r.id for r in getattr(api, args["fn"])(author="alice"))
"""
    assert hub.ok(alice, listing, fn="list_models") == ["alice/m1"]
    assert hub.ok(alice, listing, fn="list_datasets") == ["alice/d1"]

    # A label adds a view; the repository's address doesn't change.
    added = hub.request(alice, "PUT", "/api/openxet/v1/repos/alice/m1/labels/dataset")
    assert added.status_code == 200, added.text
    assert hub.ok(alice, listing, fn="list_datasets") == ["alice/d1", "alice/m1"]
    assert hub.ok(alice, "result = api.dataset_info('alice/m1').id") == "alice/m1"


def test_wrong_type_route_is_repo_not_found(hub):
    alice, bob = hub.users["alice"], hub.users["bob"]
    hub.ok(alice, CREATE, repo_id="alice/only-model")

    for user in (alice, bob):
        refused = hub.fails(user, "api.dataset_info('alice/only-model')")
        assert refused["error"] == "RepositoryNotFoundError", (user.name, refused)
        assert refused["status"] == 404, (user.name, refused)


def test_wrong_type_with_read_access_shows_label_hint(hub):
    alice = hub.users["alice"]
    hub.ok(alice, CREATE, repo_id="alice/hinted")

    refused = hub.fails(alice, "api.dataset_info('alice/hinted')")
    assert refused["error"] == "RepositoryNotFoundError", refused
    assert "openxet label add alice/hinted dataset" in refused["message"], refused["message"]


# Per-request values, which differ between any two answers.
VOLATILE_HEADERS = {"date", "x-request-id"}


def comparable(answer: httpx.Response) -> tuple:
    headers = sorted(
        (k.lower(), v) for k, v in answer.headers.items() if k.lower() not in VOLATILE_HEADERS
    )
    return answer.status_code, headers, answer.content


def test_wrong_type_without_read_access_matches_unknown_repo(hub):
    # Design §5.9 req 2, ADR 0010 item 4: bob may not read alice's model,
    # so reading it as a dataset must tell him nothing, not even that it
    # exists under another label.
    alice, bob = hub.users["alice"], hub.users["bob"]
    hub.ok(alice, CREATE, repo_id="alice/secret")

    for path in (
        "/api/datasets/{}",
        "/api/datasets/{}/revision/main",
        "/datasets/{}/resolve/main/x",
    ):
        existing = hub.request(bob, "GET", path.format("alice/secret"))
        unknown = hub.request(bob, "GET", path.format("alice/no-such-repo"))
        assert existing.status_code == 404, (path, existing.status_code)
        assert comparable(existing) == comparable(unknown), path

    refused = hub.fails(bob, "api.dataset_info('alice/secret')")
    assert refused["error"] == "RepositoryNotFoundError", refused
    assert "label" not in refused["message"], refused["message"]


def test_create_collision_answers_409_with_url(hub):
    # ADR 0010 item 5: names are unique across labels.
    alice = hub.users["alice"]
    hub.ok(alice, CREATE, repo_id="alice/taken")

    clash = hub.request(
        alice, "POST", "/api/repos/create", json={"name": "taken", "type": "dataset"}
    )
    assert clash.status_code == 409, clash.text
    assert clash.json()["url"] == f"{hub.endpoint}/datasets/alice/taken"
    assert "openxet label add alice/taken dataset" in clash.json()["error"]

    refused = hub.fails(alice, CREATE, repo_id="alice/taken", repo_type="dataset")
    assert refused["status"] == 409, refused


def test_kernel_read_with_stock_client(hub, tmp_path):
    alice = hub.users["alice"]
    hub.ok(alice, CREATE, repo_id="alice/kern", repo_type="kernel")
    listed = hub.request(alice, "GET", "/api/kernels", params={"author": "alice"})
    assert listed.status_code == 200, listed.text
    assert [k["id"] for k in listed.json()] == ["alice/kern"]

    # Written over Git, through the kernels/ prefix (ruling M2-R11).
    git = openxet_git(tmp_path / "git", alice.token, hub.endpoint)
    clone = tmp_path / "clone"
    git("clone", "-q", f"{hub.endpoint}/kernels/alice/kern.git", str(clone))
    data = random.Random(24).randbytes(4096)
    commit_file(git, clone, "build/kernel.so", data, "add the kernel")
    git("push", "-q", "origin", "main", cwd=clone)

    assert hub.ok(alice, "result = api.kernel_info('alice/kern').id") == "alice/kern"
    local = hub.ok(
        alice, DOWNLOAD, repo_id="alice/kern", filename="build/kernel.so", repo_type="kernel"
    )
    assert open(local, "rb").read() == data


def test_anonymous_private_download_is_401_shape(hub, tmp_path):
    # S3 fixture 35: a request with no credential gets 401, which
    # huggingface_hub raises as RepositoryNotFoundError; a missing
    # repository answers the same (ruling M2-R6).
    alice = hub.users["alice"]
    (tmp_path / "f.txt").write_text("private\n")
    hub.ok(alice, CREATE, repo_id="alice/private", private=True)
    hub.ok(
        alice,
        UPLOAD_FILE,
        repo_id="alice/private",
        path=str(tmp_path / "f.txt"),
        path_in_repo="f.txt",
    )

    existing = hub.request(None, "HEAD", "/alice/private/resolve/main/f.txt")
    unknown = hub.request(None, "HEAD", "/alice/no-such-repo/resolve/main/f.txt")
    assert existing.status_code == 401
    assert existing.headers["www-authenticate"] == 'Bearer realm="openxet"'
    assert existing.headers["x-error-message"].startswith("Authentication required")
    assert comparable(existing) == comparable(unknown)

    # huggingface_hub 2.2.0 raises this 401 as RepositoryNotFoundError only
    # on an https endpoint (`REPO_API_REGEX` in `utils/_http.py` starts
    # with `^https://`), as it does against huggingface.co; over the
    # harness's plain HTTP it raises the 401 itself. Either way the client
    # can't tell the private repository from a missing one.
    refused = hub.fails(None, DOWNLOAD, repo_id="alice/private", filename="f.txt")
    missing = hub.fails(None, DOWNLOAD, repo_id="alice/no-such-repo", filename="f.txt")
    assert (refused["error"], refused["status"]) == ("HfHubHTTPError", 401), refused
    def shown(answer, repo):
        message = re.sub(r"Request ID: [^)]*", "", answer["message"])
        return message.replace(repo, "{repo}")

    assert shown(refused, "alice/private") == shown(missing, "alice/no-such-repo"), (
        refused,
        missing,
    )

    # An `anonymous` rule makes a repository public (ruling M2-R3).
    hub.ok(alice, CREATE, repo_id="alice/public-model")
    hub.ok(
        alice,
        UPLOAD_FILE,
        repo_id="alice/public-model",
        path=str(tmp_path / "f.txt"),
        path_in_repo="f.txt",
    )
    local = hub.ok(None, DOWNLOAD, repo_id="alice/public-model", filename="f.txt")
    assert open(local).read() == "private\n"


def test_errors_render_in_hf_cli(hub):
    # S3 lesson 12: the 403 names the namespace, and `hf` shows it from
    # X-Error-Message.
    bob = hub.users["bob"]
    refused = hub.hf(bob, "repos", "create", "huggingface/nope")
    shown = refused.stdout + refused.stderr
    assert refused.returncode != 0
    assert "403 Forbidden" in shown, shown
    assert 'under the namespace "huggingface"' in shown, shown
