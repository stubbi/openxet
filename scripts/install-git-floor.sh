#!/usr/bin/env bash
# Builds git 2.37.7, the oldest git the Git role supports (ADR 0004 item 8,
# crates/git/src/process.rs), from the kernel.org release into PREFIX, so
# the protocol tests also run against it:
#
#   scripts/install-git-floor.sh "$HOME/.cache/openxet/git-2.37.7"
#   OPENXET_TEST_GIT_237="$HOME/.cache/openxet/git-2.37.7/bin/git" cargo nextest run -p openxet-git
#
# Does nothing when PREFIX already holds that build, so CI caches PREFIX.
# Needs a C toolchain, make, and the zlib, libcurl and expat headers (on
# Debian and Ubuntu: build-essential zlib1g-dev libcurl4-openssl-dev
# libexpat1-dev).
set -euo pipefail

version=2.37.7
# From https://mirrors.edge.kernel.org/pub/software/scm/git/sha256sums.asc,
# checked on 2026-10-25.
sha256=979916049b8c60e66d6542e19c5de3b131f2cb9a6a5419307e939bbe0760f490
prefix=${1:?usage: scripts/install-git-floor.sh PREFIX}

if [ -x "$prefix/bin/git" ] && [ "$("$prefix/bin/git" --version)" = "git version $version" ]; then
  echo "git $version is already in $prefix"
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tarball="$work/git-$version.tar.xz"
curl -fsSL --retry 3 -o "$tarball" \
  "https://mirrors.edge.kernel.org/pub/software/scm/git/git-$version.tar.xz"
if command -v sha256sum >/dev/null; then
  echo "$sha256  $tarball" | sha256sum -c - >/dev/null
else
  echo "$sha256  $tarball" | shasum -a 256 -c - >/dev/null
fi
tar -xJf "$tarball" -C "$work"

# No translations, Tcl/Tk, Perl or Python: the tests run plumbing and the
# HTTP transport only. C17, because under C23, the default of newer GCC,
# the `unreachable` macro of <stddef.h> breaks git 2.37's reflog.c.
jobs=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)
make -C "$work/git-$version" -j "$jobs" prefix="$prefix" CFLAGS="-O2 -std=gnu17" \
  NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 install >/dev/null
"$prefix/bin/git" --version
