# Git LFS for any Git host

<!-- description: Keep a Git repository on GitHub, GitLab, Gitea or any Git host and store its Git LFS files on OpenXet, deduplicated with Xet. -->

Your Git repository can stay where it is, on GitHub, GitLab, Gitea or any
other Git host, while its Git LFS files live on OpenXet. Git LFS
uploads each version of a file whole; through OpenXet and `git-xet`, a new
version uploads only the chunks that changed, and every clone still gets
complete files.

The Git host keeps the history and the small pointer files. OpenXet keeps
the file contents, in a repository of its own that holds the LFS objects.
This is the setup the compatibility suite's LFS leg runs with stock
`git-lfs` 3.7.1 and `git-xet` 0.2.1 against a Gitea server
(`compat/tests/test_lfs_gitea.py`). If you would rather host the Git
repository on OpenXet too, see [Git hosting](git-hosting.md).

## What you need

- `git` and [Git LFS](https://git-lfs.com), with `git lfs install` run once.
- [`git-xet`](https://github.com/huggingface/xet-core/releases), with
  `git xet install` run once. It uploads LFS files as Xet chunks. Clones
  that only download don't need it.
- An OpenXet access token whose [rules](../reference/config.md#rules)
  let it write to the OpenXet repository that holds the files, and create
  it if it doesn't exist yet. The first upload creates it.

The examples use `https://git.example.com/acme/weather.git` for the Git
remote and `https://openxet.example.com` for the OpenXet server, whose
repository `acme/weather` holds the files.

## Set up a clone

Run these in each clone, after `git clone` or `git remote add`:

```bash
git config lfs.url https://openxet.example.com/acme/weather.git/info/lfs
git config lfs.https://git.example.com/acme/weather.git/info/lfs.access none
printf 'protocol=https\nhost=openxet.example.com\nusername=openxet\npassword=%s\n\n' "$OPENXET_TOKEN" \
  | git credential approve
```

1. `lfs.url` sends Git LFS to OpenXet instead of the Git host. Its value
   is the repository's [LFS URL](../reference/lfs.md#the-lfs-url).
2. The `access` key names the LFS endpoint `git-xet` derives from the Git
   remote: the remote URL followed by `/info/lfs`. With it set to `none`,
   `git-xet` refreshes its upload token through the URL OpenXet hands it,
   with no credential, which is all that URL takes
   ([ADR 0003](../adr/0003-git-xet-credential-strategy.md)).
3. The token goes to your git credential helper under the OpenXet host
   only, so `git-lfs` sends it to OpenXet and never to the Git host.

You can commit `lfs.url` for every clone in a `.lfsconfig` file at the
repository's root:

```bash
git config -f .lfsconfig lfs.url https://openxet.example.com/acme/weather.git/info/lfs
git add .lfsconfig
```

`.lfsconfig` can't carry the `access` key
([ADR 0003 decision 4](../adr/0003-git-xet-credential-strategy.md)), so
each clone that uploads still sets it.

## Push

Track large files as with any Git LFS repository and push to the Git host:

```bash
git lfs track "*.bin"
git add .gitattributes model.bin
git commit -m "Add the model"
git push origin main
```

`git-lfs` runs before the push: `git-xet` chunks `model.bin`, uploads the
chunks OpenXet doesn't have yet, and then git pushes the commit, with its
pointer file, to the Git host.

Without `git-xet`, the upload is refused before anything is pushed, with
a message naming `git-xet` and `git xet install`. OpenXet stores LFS files
through Xet; uploads with plain `git-lfs` arrive with server-side chunking
in milestone M4.

## Clone and pull

`git-lfs` downloads files from OpenXet over plain HTTPS, with the token
from your credential helper, whether or not `git-xet` is installed. A
fresh clone needs `lfs.url` before its first checkout, unless the
repository has a `.lfsconfig`, so skip the download at clone time and pull
once the clone is set up:

```bash
GIT_LFS_SKIP_SMUDGE=1 git clone https://git.example.com/acme/weather.git
cd weather
git config lfs.url https://openxet.example.com/acme/weather.git/info/lfs
git lfs pull
```

## `openxet setup git`

From milestone M6, `openxet setup git` writes these settings for you:
`lfs.url`, the `access` key for the remote and the token in your
credential helper under the OpenXet host. It changes only the clone's own
configuration, never your global one, prints exactly what it changed, and
takes `--dry-run` and `--json`. Until then, the commands above are the
same settings by hand.

## When something fails

| What you see | Why | What to do |
| ------------ | --- | ---------- |
| An upload error naming `git-xet` and `git xet install` | `git-xet` isn't installed or registered. | Install it and run `git xet install`. |
| `Repository or object not found` for the OpenXet URL | The OpenXet repository doesn't exist, or your token may not read it. | Check `lfs.url` and the token's rules. |
| `git-lfs` asks for a user name and password for the OpenXet host | No token in the credential helper for that host. | Run the `git credential approve` command above. |
| `smudge filter lfs failed` during `git clone` | Git LFS asked the Git host, because the clone had no `lfs.url` yet. | Clone with `GIT_LFS_SKIP_SMUDGE=1`, set up the clone, then `git lfs pull`. |

The batch API's routes, answers and limits are in
[Git LFS routes](../reference/lfs.md).
