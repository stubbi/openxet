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

# The capture xorb is footer-less, the form xorb_chunks reads; the reference
# xorb is not, so only xorb_upload gets it.
cp "$capture"/*.xorb corpus/xorb_chunks/
for xorb in "$capture"/*.xorb "$reference"/*.xorb; do
  [ -f "$xorb" ] || continue
  cp "$xorb" "corpus/xorb_upload/$(basename "$xorb")"
done

# The reference shards: with footer, with verification entries, and the
# footer-less upload form.
for target in upload_shard verify_shard; do
  cp "$capture/upload.shard" "corpus/$target/hf-xet-capture.shard"
  for shard in "$reference"/*.shard "$reference"/*.shard.verification \
    "$reference"/*.shard.verification-no-footer; do
    [ -f "$shard" ] || continue
    cp "$shard" "corpus/$target/$(basename "$shard")"
  done
done
