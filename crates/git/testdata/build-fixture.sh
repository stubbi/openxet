#!/bin/sh
# Builds a test repository with git at test time, so no binary fixture is
# ever committed. The two kinds follow docs/research/S9-git-engine.md
# ("Method", test repos), scaled by COMMITS:
#
#   tf10k  a source tree of 60 text files; the first commit adds them all,
#          each later commit rewrites 3, and an annotated tag v1.0 points at
#          the tip (S9 used 10,001 commits of huggingface/transformers).
#   lfs    200 `*.safetensors` Git LFS pointer files under a .gitattributes
#          that tracks them, 3 pointers changed per commit (S9: 500 commits).
#
# Content comes from a Park-Miller generator seeded with SEED, in integer
# arithmetic every awk computes exactly, and every date is fixed, so the same
# arguments give the same object ids on every machine.
#
# Usage: build-fixture.sh tf10k|lfs DEST [COMMITS] [SEED]
set -eu

kind=${1:?usage: build-fixture.sh tf10k|lfs DEST [COMMITS] [SEED]}
dest=${2:?usage: build-fixture.sh tf10k|lfs DEST [COMMITS] [SEED]}
commits=${3:-300}
seed=${4:-20261025}

case $kind in
tf10k | lfs) ;;
*)
	echo "build-fixture.sh: unknown kind '$kind'; use tf10k or lfs" >&2
	exit 2
	;;
esac

# The caller's git configuration must not change the result.
GIT_CONFIG_NOSYSTEM=1
GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_NOSYSTEM GIT_CONFIG_GLOBAL

git init --quiet --bare "$dest"
git -C "$dest" symbolic-ref HEAD refs/heads/main

awk -v kind="$kind" -v commits="$commits" -v seed="$seed" '
function next_rand() { x = (x * 16807) % 2147483647; return x }
function data(s) { printf "data %d\n%s\n", length(s), s }
function hex64(   s, i) {
	s = ""
	for (i = 0; i < 8; i++) s = s sprintf("%08x", next_rand())
	return s
}
function text_file(   s, l) {
	s = ""
	for (l = 0; l < 8; l++) s = s "value = " next_rand() "\n"
	return s
}
function pointer(   size) {
	size = next_rand() % 1000000000 + 1000000
	return "version https://git-lfs.github.com/spec/v1\noid sha256:" hex64() "\nsize " size "\n"
}
BEGIN {
	x = seed
	files = (kind == "lfs") ? 200 : 60
	for (c = 1; c <= commits; c++) {
		t = 1700000000 + c * 60
		print "commit refs/heads/main"
		print "mark :" c
		print "author Fixture <fixture@openxet.invalid> " t " +0000"
		print "committer Fixture <fixture@openxet.invalid> " t " +0000"
		data("commit " c)
		if (c > 1) print "from :" (c - 1)
		if (c == 1 && kind == "lfs") {
			print "M 100644 inline .gitattributes"
			data("*.safetensors filter=lfs diff=lfs merge=lfs -text")
		}
		n = (c == 1) ? files : 3
		for (k = 0; k < n; k++) {
			# Three neighbouring files, so no commit writes a path twice and
			# leaves a dangling blob behind.
			if (c > 1 && k == 0) first = next_rand() % files
			f = (c == 1) ? k : (first + k) % files
			if (kind == "lfs") {
				print "M 100644 inline weights/shard-" f ".safetensors"
				data(pointer())
			} else {
				print "M 100644 inline src/dir" (f % 10) "/file" f ".txt"
				data(text_file())
			}
		}
		print ""
	}
	if (kind == "tf10k") {
		print "tag v1.0"
		print "from :" commits
		print "tagger Fixture <fixture@openxet.invalid> " (1700000000 + commits * 60) " +0000"
		data("release 1.0")
	}
}' | git -C "$dest" fast-import --quiet --date-format=raw
