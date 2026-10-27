# Git hosting

<!-- description: Host Git repositories on OpenXet: clone and push URLs, credentials, how pushes and hf uploads share one history, large files, and what the server refuses. -->

Every OpenXet repository is a Git repository. You clone, fetch and push it
with stock `git` over HTTPS, and the commits that `hf upload` and
`huggingface_hub` make land in the same history: a push is visible to the
next `hf download`, and an upload to the next `git pull`. Large files go
through Git LFS with `git-xet`, and are stored as Xet chunks like
everything `hf` uploads.

The [Hub quickstart](../quickstart-hub.md) shows the whole round trip on
one machine. This page explains each part. The wire contract of each
route is in [Git routes](../reference/git-routes.md).

## Server requirements

The server runs `git` 2.37 or newer from its `PATH` for the Git protocol
itself, and checks the version at start-up. Each server keeps a cache of
the repositories' packs under `git.cache_dir`; the packs themselves live
in the object-store bucket, so a node with an empty cache serves the same
history. Servers that share a data directory each need their own
`cache_dir` ([config](../reference/config.md#git)).

## URLs

A repository's Git URL is the Hub's URL, `hub.public_url` (the server's
URL unless you set it), followed by its namespace and name:

```text
https://openxet.example.com/acme/weather
https://openxet.example.com/acme/weather.git
```

Both forms work. The unprefixed URL reaches a repository whatever its
[labels](../adr/0010-two-storage-primitives.md). Under `datasets/`,
`spaces/` or `kernels/`, as on huggingface.co, the URL sees the
repository only if it carries that label:

```text
https://openxet.example.com/datasets/acme/weather
```

To a caller who may read the repository, one without the label answers
there with a hint naming the `openxet label add` command that adds it; to
anyone else it answers as a missing repository.

## Credentials

git sends the OpenXet access token as the password of HTTP Basic; the user
name is ignored. Any git credential helper can hold it. To store it with
the helper you already use:

```bash
printf 'protocol=https\nhost=openxet.example.com\nusername=openxet\npassword=%s\n\n' "$OPENXET_TOKEN" \
  | git credential approve
```

In CI, or wherever the token is in an environment variable, a helper can
answer from the variable, so the token is never written to a file:

```bash
git -c credential.helper= \
    -c credential.helper='!f() { test "$1" = get && echo username=openxet && echo "password=$OPENXET_TOKEN"; }; f' \
    clone https://openxet.example.com/acme/weather
```

The empty `credential.helper` stops the helpers of your global
configuration from storing the token. git also takes the token as a
`Bearer` header through `http.extraHeader`.

The same token works for `hf`, `huggingface_hub` and `git`. Which
repositories it may read, write and create in is decided by the server's
[rules](../reference/config.md#rules). A repository the token may not
read answers exactly as one that doesn't exist, so a failed clone never
tells a stranger that a private repository is there. A token that may
read but not write gets an error naming the repository when it pushes.

Never put the token in the remote URL: git writes the remote URL to
`.git/config` and shows it in its output.

## Create a repository by pushing

The first push to a repository that doesn't exist creates it, when the
rules let you create in the namespace and read and write the new
repository:

```bash
git init -b main weather && cd weather
git commit --allow-empty -m "Start"
git push https://openxet.example.com/acme/weather main
```

The repository gets the `model` label, or the label of the prefix you
pushed through (`datasets/` gives `dataset`), as `hf repos create` would.
Fetching or cloning a missing repository never creates it.

A repository created with `hf repos create`, `hf upload` or
`huggingface_hub` starts with an initial commit on `main` holding the same
`.gitattributes` huggingface.co writes, authored by
`openxet <noreply@openxet.com>`. A repository created by a push starts
with what you pushed.

## One history for git and hf

A Hub upload is a real Git commit on the branch it names, built from the
branch's tip. Its author is the token's user, `{user}
<{user}@users.noreply.openxet.com>`, or the `email` claim of the token
when it has one; its committer is `openxet <noreply@openxet.com>`; its
message is the upload's summary and description. Branches and tags made
with `hf repos branch create` or `hf repos tag create` (or
`create_branch` and `create_tag`) are ordinary Git refs, and a pushed branch or tag shows
in `hf`'s listings at once.

Every write, push or upload, moves refs in one transaction that checks
each ref is still where the writer last read it. So two writers to one
branch never lose a commit:

- Two `hf upload`s to one branch both land, one after the other: the
  server retries the later one on top of the earlier.
- A `git push` whose branch moved since you fetched is refused with
  `fetch first`, as any Git server does. Pull, then push again.
- A push of several refs is atomic: if one is refused, none moves.

## Branches, tags and force pushes

- Any valid branch or tag name can be pushed. `main` is the default
  branch and can't be deleted.
- Force pushes are allowed. The commits a force push leaves behind can't
  be fetched by their id afterwards, even by someone who knows it: a
  fetch is answered only with objects reachable from the repository's
  branches and tags.
- Hub pull requests and discussions are not implemented: a Hub commit
  with `create_pr=True` answers `501`.

## Large files

Store large files with Git LFS and [`git-xet`](https://github.com/huggingface/xet-core/releases),
Hugging Face's Git LFS transfer agent for Xet. With `git-xet` installed
and `git xet install` run once, track the files and push as usual:

```bash
git lfs track "*.bin"
git add .gitattributes model.bin
git commit -m "Add the model"
git push
```

`git-xet` uploads the new chunks to the server's Xet storage and git
pushes only the small pointer files. `git clone` and `git lfs pull`
download LFS files with plain `git-lfs` over HTTPS, with the same
credential, whether or not `git-xet` is installed. Files that `hf upload`
stored through Xet are LFS files of the repository too, so a clone gets
them the same way.

Plain `git-lfs` without `git-xet` can't upload yet: the push fails before
any ref moves, with a message naming `git-xet` and `git xet install`.
Uploads with plain `git-lfs` arrive with server-side chunking in milestone
M4.

A large file pushed without LFS is accepted and kept as a Git object, but
every clone downloads it again in full. When a push adds a file of 10 MiB
or more that isn't an LFS pointer, the push output names it:

```text
remote: openxet: model.bin is 120.0 MiB and stored as a plain Git object, not with Git LFS. Large files in Git history slow down every clone; track them with: git lfs track "model.bin"
```

An operator can refuse such files above a size with
`git.max_plain_blob_bytes`; one push is at most `git.max_push_bytes`
(10 GiB by default) ([config](../reference/config.md#git)).

To keep a repository on another Git host and only its large files on
OpenXet, see [Git LFS for any Git host](lfs-for-any-repo.md).

## What the server refuses, and what to do

Errors come back as `remote:` lines in git's output, each with a hint.

| What git shows | Why | What to do |
| -------------- | --- | ---------- |
| `Repository not found` | The repository doesn't exist, or your token may not read it. | Check the URL and the token. |
| `could not read Username` | The server asked for a credential and git had none. | Store the token as the password for the host. |
| `Invalid credentials in Authorization header`, `Authentication failed` | The server doesn't know the token, or it expired. | Store a current token. |
| `You don't have the rights to push to …` | Your token may read the repository but not write to it. | Ask the operator for write access in the server's rules. |
| `… exists without the dataset label` | You may read the repository, but used a typed URL whose label it lacks. | Use the unprefixed URL, or add the label. |
| `fetch first` | The branch moved since you last fetched. | `git pull`, then push again. |
| `deletion of the current branch prohibited` | A push deleting `main`. | Keep `main`. |
| `could not use the delta bases this thin pack relies on` | Your push builds on objects the server could not place in the repository's history, as a shallow or out-of-date clone can. | `git fetch`, then `git -c pack.window=0 push`. |
| `pack exceeds maximum allowed size` | The push is larger than `git.max_push_bytes`. | Track large files with Git LFS. |
| `Retry in a moment` | Too many requests in the repository at once, or a check over its time budget. | Push or fetch again; nothing was changed. |

## Not served yet

- Git over SSH, and post-receive webhooks, arrive with milestone M4.
- The dumb HTTP protocol is not served; every Git client since 1.6.6 uses
  smart HTTP.
- `openxet gc` and `openxet reindex`, which remove superseded packs and
  rebuild the index from the bucket, arrive with milestone M4. Until then
  nothing is ever deleted from the bucket.
