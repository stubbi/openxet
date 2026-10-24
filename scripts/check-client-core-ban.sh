#!/usr/bin/env bash
# Proves that deny.toml's ADR 0006 ban bites: in a scratch copy of the
# workspace, a server crate (openxet-cas) gains a direct dependency on each
# client-core crate in turn, and `cargo deny check bans` must reject it.
# The real workspace is never modified.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

for crate in xet-data xet-client xet-runtime; do
  rm -rf "$scratch/ws"
  mkdir "$scratch/ws"
  (cd "$root" && tar -cf - Cargo.toml Cargo.lock deny.toml rust-toolchain.toml crates) |
    tar -xf - -C "$scratch/ws"
  manifest="$scratch/ws/crates/cas/Cargo.toml"
  awk -v dep="$crate = \"=1.7.0\"" '{ print } /^\[dependencies\]$/ { print dep }' \
    "$manifest" > "$manifest.new"
  mv "$manifest.new" "$manifest"

  if out="$(cd "$scratch/ws" && cargo deny --log-level error check bans 2>&1)"; then
    echo "cargo deny accepted $crate as a dependency of openxet-cas; the ADR 0006 ban in deny.toml is not working" >&2
    exit 1
  fi
  if ! grep -q "banned" <<<"$out" || ! grep -q "$crate" <<<"$out"; then
    echo "cargo deny failed for another reason than the $crate ban:" >&2
    echo "$out" >&2
    exit 1
  fi
  echo "ok: $crate in openxet-cas is banned"
done
