#!/usr/bin/env bash
# Fills fuzz/corpus/<target>/ from the canonical fixtures in testdata/, which
# stay the single copy. Run once before fuzzing; the corpus is not committed.
set -euo pipefail

cd "$(dirname "$0")"
capture=../testdata/hf-xet-1.7.0-upload
reference=../testdata/xet-spec-reference

for target in xorb_upload xorb_chunks upload_shard verify_shard; do
  mkdir -p "corpus/$target"
done

for xorb in "$capture"/*.xorb "$reference"/*.xorb; do
  [ -f "$xorb" ] || continue
  cp "$xorb" "corpus/xorb_upload/$(basename "$xorb")"
  cp "$xorb" "corpus/xorb_chunks/$(basename "$xorb")"
done
for target in upload_shard verify_shard; do
  cp "$capture/upload.shard" "corpus/$target/hf-xet-capture.shard"
done
