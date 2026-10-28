#!/usr/bin/env bash
# Measures disk use and wall time of concurrent Cargo builds in N sibling Git
# worktrees of one repository, for the agent workspaces success test
# (docs/superpowers/specs/2026-10-27-agent-workspaces-design.md §2, §6.4).
# Results and method: docs/research/S16-workspace-disk.md.
#
# The worktrees come from a `git clone --local` inside a scratch directory, so
# the measured repository never gains worktrees. Volume use is sampled with
# `df` once a second; any run is killed the moment free space drops under the
# floor, and the scratch directory is always removed, on success or failure.
set -euo pipefail
export LC_ALL=C

usage() {
  cat <<'EOF'
usage: scripts/bench-workspaces.sh MODE [--align-mtimes] [--inspect CMD] <repo> <n>

MODE (one per run):
  --plain         build all n worktrees concurrently from empty target/ dirs
  --seeded        build the first worktree, then create the others, clone its
                  target/ into them (APFS clonefile, Linux reflink) and build
                  them concurrently
  --remap         as --seeded, with RUSTFLAGS="--remap-path-prefix=<own path>=/ws"
                  in every worktree
  --remap-shared  as --seeded, with one identical RUSTFLAGS in every worktree
                  that remaps each worktree's path to /ws

Options:
  --align-mtimes  after seeding, give every tracked file the first worktree's
                  mtime (the sources are identical by blob)
  --inspect CMD   before cleanup, run CMD with every worktree's target/ as
                  arguments (for example a dedup analysis)

Environment:
  BENCH_SCRATCH     scratch dir, must not exist (default $TMPDIR/s16-bench)
  BENCH_RESULTS     results dir, kept (default $TMPDIR/s16-bench-results)
  BENCH_FLOOR_GIB   abort when free space falls under this (default 20)
  BENCH_CARGO_ARGS  cargo arguments (default "build --workspace --locked")

Exit codes: 0 done, 1 a build failed, 2 usage or setup error, 3 aborted at
the free-space floor.
EOF
}

die() { echo "error: $*" >&2; exit 2; }

mode="" align=0 inspect=""
while [ $# -gt 0 ]; do
  case "$1" in
    --plain | --seeded | --remap | --remap-shared)
      [ -z "$mode" ] || die "give one MODE per run"
      mode=${1#--} ;;
    --align-mtimes) align=1 ;;
    --inspect) [ $# -ge 2 ] || die "--inspect needs a command"; inspect=$2; shift ;;
    -h | --help) usage; exit 0 ;;
    --*) usage >&2; die "unknown option $1" ;;
    *) break ;;
  esac
  shift
done
[ -n "$mode" ] && [ $# -eq 2 ] || { usage >&2; exit 2; }
repo=$(cd "$1" && pwd) || die "no such repository: $1"
n=$2
case "$n" in '' | *[!0-9]*) die "n must be a positive integer" ;; esac
[ "$n" -ge 1 ] || die "n must be at least 1"
[ "$mode" = plain ] || [ "$n" -ge 2 ] || die "--$mode needs n >= 2"
[ "$mode" != plain ] || [ "$align" -eq 0 ] || die "--align-mtimes needs a seeded mode"

tmp=${TMPDIR:-/tmp}
tmp=${tmp%/}
scratch=${BENCH_SCRATCH:-$tmp/s16-bench}
results=${BENCH_RESULTS:-$tmp/s16-bench-results}
floor_gib=${BENCH_FLOOR_GIB:-20}
read -r -a cargo_args <<<"${BENCH_CARGO_ARGS:-build --workspace --locked}"
floor_kib=$((floor_gib * 1024 * 1024))

[ ! -e "$scratch" ] || die "$scratch exists; remove it or set BENCH_SCRATCH"
rev=$(git -C "$repo" rev-parse HEAD) || die "$repo is not a Git repository"

volume() { df -Pk "$1" | awk 'NR == 2 { print $1 }'; }
used_kib() { df -Pk "$scratch" | awk 'NR == 2 { print $3 }'; }
free_kib() { df -Pk "$scratch" | awk 'NR == 2 { print $4 }'; }
gib() { awk -v k="$1" 'BEGIN { printf "%.2f", k / 1048576 }'; }

mkdir -p "$results"
mkdir "$scratch"
src=$scratch/src
pids=$scratch/pids
abort_flag=$scratch/aborted
seed_marker=$scratch/seeded-at
: >"$pids"
sampler_pid=""

kill_builds() {
  local pid
  while read -r pid; do
    kill -TERM -- "-$pid" 2>/dev/null || true
  done <"$pids"
  sleep 2
  while read -r pid; do
    kill -KILL -- "-$pid" 2>/dev/null || true
  done <"$pids"
}

# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
  local status=$? w
  trap - EXIT INT TERM
  [ -z "$sampler_pid" ] || kill "$sampler_pid" 2>/dev/null || true
  [ ! -f "$pids" ] || kill_builds
  wait 2>/dev/null || true
  if [ -d "$src/.git" ]; then
    for w in "$scratch"/ws*; do
      [ -d "$w" ] || continue
      git -C "$src" worktree remove --force "$w" 2>/dev/null || true
    done
    git -C "$src" worktree prune
  fi
  rm -rf "$scratch"
  echo "cleanup: removed $scratch; free $(gib "$(df -Pk "$tmp" | awk 'NR == 2 { print $4 }')") GiB"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

[ "$(volume "$repo")" = "$(volume "$scratch")" ] ||
  die "$scratch is not on the same volume as $repo; set BENCH_SCRATCH"
[ "$(free_kib)" -ge "$floor_kib" ] ||
  die "only $(gib "$(free_kib)") GiB free, under the ${floor_gib} GiB floor"

# Each background build is its own process group (job control), so an abort
# reaches its rustc children too.
set -m

stamp=$(date +%Y%m%dT%H%M%S)
tag="$mode$([ "$align" -eq 0 ] || echo -aligned)-n$n-$stamp"
tsv=$results/$tag.df.tsv
events=$results/$tag.events.tsv
times=$results/$tag.builds.tsv
printf 'epoch\tused_kib\tfree_kib\trustc_all\trustc_ours\n' >"$tsv"
printf 'epoch\tevent\n' >"$events"
printf 'workspace\tstart\tend\tseconds\tstatus\tunits_compiled\n' >"$times"
event() { printf '%s\t%s\n' "$(date +%s)" "$1" >>"$events"; }

sampler() {
  local used free counts
  while :; do
    read -r used free < <(df -Pk "$scratch" | awk 'NR == 2 { print $3, $4 }')
    # rustc processes on the machine, and those building in the scratch dir:
    # other sessions' builds show up as the difference.
    counts=$(ps -Ao args= | awk -v s="$scratch" '
      { exe = $1; sub(/.*\//, "", exe) }
      exe == "rustc" { all++; if (index($0, s)) ours++ }
      END { printf "%d\t%d", all, ours }')
    printf '%s\t%s\t%s\t%s\n' "$(date +%s)" "$used" "$free" "$counts" >>"$tsv"
    if [ "$free" -lt "$floor_kib" ]; then
      : >"$abort_flag"
      event "abort: free $(gib "$free") GiB under the ${floor_gib} GiB floor"
      kill_builds
      return
    fi
    sleep 1
  done
}

add_worktree() {
  git -C "$src" worktree add --quiet --detach "$scratch/ws$1" "$rev"
}

rustflags_for() {
  local i
  case "$mode" in
    remap) printf -- '--remap-path-prefix=%s=/ws' "$scratch/ws$1" ;;
    remap-shared)
      for i in $(seq 1 "$n"); do printf -- '--remap-path-prefix=%s=/ws ' "$scratch/ws$i"; done ;;
  esac
}

build() {
  local i=$1 start end status flags
  flags=$(rustflags_for "$i")
  start=$(date +%s)
  if (cd "$scratch/ws$i" && if [ -n "$flags" ]; then export RUSTFLAGS="$flags"; fi &&
    exec cargo "${cargo_args[@]}") >"$results/$tag.ws$i.log" 2>&1; then
    status=ok
  else
    status=failed
  fi
  end=$(date +%s)
  printf 'ws%s\t%s\t%s\t%s\t%s\t%s\n' "$i" "$start" "$end" "$((end - start))" "$status" \
    "$(grep -c '^ *Compiling ' "$results/$tag.ws$i.log" || true)" >>"$times"
  [ "$status" = ok ]
}

# Builds workspaces $1..$2 concurrently; returns non-zero if any failed.
build_concurrently() {
  local i pid failed=0 started=""
  for i in $(seq "$1" "$2"); do
    build "$i" &
    echo "$!" >>"$pids"
    started="$started $!"
  done
  for pid in $started; do wait "$pid" || failed=1; done
  return "$failed"
}

seed() {
  local from=$scratch/ws1/target to=$scratch/ws$1/target
  # A clone, never a copy: a filesystem without clones fails here.
  case "$(uname -s)" in
    Darwin) cp -c -pR "$from" "$to" ;;
    *) cp -a --reflink=always "$from" "$to" ;;
  esac
}

# Allocated KiB of the files a workspace's build left in its target/: all of
# it for a build from empty, only files newer than the seed for a seeded one
# (the clone keeps mtimes, so untouched files are older). Unlike df, other
# processes on the volume do not move it; unlike the df peak, it is the end
# state.
written_kib() {
  local t=$scratch/ws$1/target
  if [ ! -d "$t" ]; then
    echo 0
  elif [ "$mode" = plain ] || [ "$1" -eq 1 ]; then
    du -sk "$t" | awk '{ print $1 }'
  else
    find "$t" -type f -newer "$seed_marker" -exec du -k {} + | awk '{ s += $1 } END { print s + 0 }'
  fi
}

align_mtimes() {
  local f ref=$scratch/ws1 dst=$scratch/ws$1
  (cd "$dst" && git ls-files -z) | while IFS= read -r -d '' f; do
    if [ -f "$dst/$f" ] && [ ! -L "$dst/$f" ]; then touch -r "$ref/$f" "$dst/$f"; fi
  done
}

echo "bench-workspaces: --$mode$([ "$align" -eq 0 ] || echo ' --align-mtimes') n=$n rev=$rev"
echo "  scratch $scratch, results $results/$tag.*"
echo "  load: $(uptime)"
git clone --quiet --local --no-checkout "$repo" "$src"
git -C "$src" cat-file -e "$rev^{commit}" 2>/dev/null ||
  die "$rev is not reachable from a branch of $repo; commit or branch it first"

if [ "$mode" = plain ]; then
  for i in $(seq 1 "$n"); do add_worktree "$i"; done
else
  add_worktree 1
fi
baseline=$(used_kib)
sampler &
sampler_pid=$!
event start

failed=0
one_build=""
if [ "$mode" = plain ]; then
  event "build ws1..ws$n"
  build_concurrently 1 "$n" || failed=1
else
  event "build ws1"
  build_concurrently 1 1 || failed=1
  one_build=$(($(used_kib) - baseline))
  if [ "$failed" -eq 0 ] && [ ! -e "$abort_flag" ]; then
    event "create and seed ws2..ws$n"
    sleep 1
    touch "$seed_marker"
    sleep 1
    for i in $(seq 2 "$n"); do
      [ ! -e "$abort_flag" ] || break
      add_worktree "$i"
      seed "$i"
      [ "$align" -eq 0 ] || align_mtimes "$i"
    done
    event "seeded"
    event "build ws2..ws$n"
    build_concurrently 2 "$n" || failed=1
  fi
fi
event end
final=$(($(used_kib) - baseline))
kill "$sampler_pid" 2>/dev/null || true
wait "$sampler_pid" 2>/dev/null || true
sampler_pid=""

peak=$(awk -F'\t' -v b="$baseline" 'NR > 1 && $2 - b > m { m = $2 - b } END { print m + 0 }' "$tsv")
other_rustc=$(awk -F'\t' 'NR > 1 && $4 - $5 > m { m = $4 - $5 } END { print m + 0 }' "$tsv")
# Plain has no standalone build to measure; one build is the final use over n.
estimated=""
if [ -z "$one_build" ]; then one_build=$((final / n)); estimated=" (final / n)"; fi
increment=$(awk -v p="$peak" -v f="$one_build" -v n="$n" \
  'BEGIN { if (n < 2 || f <= 0) print "n/a"; else printf "%.1f%%", 100 * (p - f) / (n - 1) / f }')

echo
echo "result --$mode$([ "$align" -eq 0 ] || echo ' --align-mtimes') n=$n"
echo "  one build footprint: $(gib "$one_build") GiB$estimated"
echo "  peak over baseline:  $(gib "$peak") GiB"
echo "  final over baseline: $(gib "$final") GiB"
echo "  per extra workspace: $increment of one build, at the peak"
echo "  most rustc processes outside this benchmark at once: $other_rustc"
echo "  builds (workspace, seconds, status, units compiled, GiB written):"
written_total=0
for i in $(seq 1 "$n"); do
  w=$(written_kib "$i")
  [ "$i" -gt 1 ] || first_written=$w
  written_total=$((written_total + w))
  awk -F'\t' -v ws="ws$i" -v g="$(gib "$w")" '$1 == ws { printf "    %s %ss %s %s %s\n", $1, $4, $5, $6, g }' "$times"
done
written_increment=$(awk -v t="$written_total" -v f="$first_written" -v n="$n" \
  'BEGIN { if (n < 2 || f <= 0) print "n/a"; else printf "%.1f%%", 100 * (t - f) / (n - 1) / f }')
echo "  per extra workspace by bytes written: $written_increment of the first workspace's target/"
echo "  load: $(uptime)"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$tag" "$mode" "$align" "$n" "$one_build" "$peak" "$final" \
  "$increment" "$written_total" "$written_increment" >>"$results/summary.tsv"

if [ -n "$inspect" ] && [ ! -e "$abort_flag" ]; then
  echo "inspect: $inspect"
  "$inspect" "$scratch"/ws*/target >"$results/$tag.inspect.tsv"
  cat "$results/$tag.inspect.tsv"
fi

if [ -e "$abort_flag" ]; then
  echo "aborted: free space fell under the ${floor_gib} GiB floor" >&2
  exit 3
fi
exit "$failed"
