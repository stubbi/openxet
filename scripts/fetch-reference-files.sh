#!/usr/bin/env bash
# Fetches Hugging Face's Xet reference files at a pinned revision and verifies
# them against the committed SHA256SUMS, which also lists the files to fetch.
# Dataset: https://huggingface.co/datasets/xet-team/xet-spec-reference-files
set -euo pipefail

revision=c4aa3a3f15b1395fff5ce934784bf6c8f2d62de8
base="https://huggingface.co/datasets/xet-team/xet-spec-reference-files/resolve/${revision}"
dir="$(cd "$(dirname "$0")/.." && pwd)/testdata/xet-spec-reference"

cd "$dir"
while read -r _ name; do
  if [[ ! -f "$name" ]]; then
    echo "fetching $name"
    curl -fL --retry 3 --silent --show-error -o "$name.part" "$base/$name"
    mv "$name.part" "$name"
  fi
done < SHA256SUMS

if ! shasum -a 256 -c --quiet SHA256SUMS; then
  echo "error: reference files in $dir do not match SHA256SUMS; delete the listed files and re-run $0" >&2
  exit 1
fi
