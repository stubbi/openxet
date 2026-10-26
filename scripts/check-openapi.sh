#!/usr/bin/env bash
# Ruling M2-R15: docs/reference/openapi.json is the document `openxet
# openapi` prints, which `GET /openapi.json` serves byte for byte
# (crates/openxet/tests/openapi.rs). Fails, showing the difference, when a
# route changed without the checked-in copy.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
checked_in="$root/docs/reference/openapi.json"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

cargo run --locked --quiet --manifest-path "$root/Cargo.toml" -p openxet -- openapi \
  > "$scratch/openapi.json"
if ! diff -u "$checked_in" "$scratch/openapi.json" > "$scratch/diff"; then
  head -n 200 "$scratch/diff" >&2
  echo "docs/reference/openapi.json differs from \`openxet openapi\` (diff above, first 200 lines)." >&2
  echo "Regenerate it: cargo run -q -p openxet -- openapi > docs/reference/openapi.json" >&2
  exit 1
fi
echo "ok: docs/reference/openapi.json matches openxet openapi"
