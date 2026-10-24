#!/usr/bin/env bash
# Fills fuzz/corpus/<target>/ from the canonical fixtures in testdata/, which
# stay the single copy, and writes fuzz/Cargo.lock. Run before fuzzing, and
# again after a dependency change; neither the corpus nor the lockfile is
# committed.
set -euo pipefail

cd "$(dirname "$0")"

# Fuzz the dependency versions the workspace ships: the fuzz workspace's
# lockfile starts from the workspace's, and cargo adds only what fuzzing
# alone needs (libfuzzer-sys and the like). It is not committed.
cp ../Cargo.lock Cargo.lock
cargo +nightly metadata --format-version 1 --manifest-path Cargo.toml > /dev/null

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

# CAS tokens, the transfer URL check and the reconstruction Range parser
# take text, so their seeds are written here. A JWT is assembled at seed
# time, so no token-shaped string is committed (compat's redaction guard).
# For cas_token and transfer_url, byte 0 picks the mode: even takes the
# rest as sent, odd signs first and then alters (see each target).
for target in cas_token transfer_url reconstruction_range; do
  mkdir -p "corpus/$target"
done
b64url() { base64 | tr '+/' '-_' | tr -d '=\n'; }
header=$(printf '{"alg":"EdDSA","typ":"JWT","kid":"%s"}' "$(printf 'k' | b64url)" | b64url)
claims=$(printf '{"sub":"agent","scope":"model/org/repo","mode":"write","exp":1800003600,"iat":1800000000,"nbf":1799999000}' | b64url)
ed25519_sig=$(head -c 64 /dev/zero | b64url)
printf '\000%s.%s.%s' "$header" "$claims" "$ed25519_sig" > corpus/cas_token/jwt
printf '\000%s.%s' "$header" "$claims" > corpus/cas_token/two-segments
printf '\000oxt_%s' "$(head -c 32 /dev/zero | b64url)" > corpus/cas_token/static
printf '\001\005agent\016model/org/repo\001\377\377\000\000' > corpus/cas_token/minted
printf '\001\005agent\004repo\000\020\000\000\001\000\012\000\001' > corpus/cas_token/minted-flipped

printf '\000\00710-2000\000\000\000\000\000\000\000\000\053AAAA\001\020bytes=10-2000' \
  > corpus/transfer_url/raw
printf '\001\012\000\000\000\000\000\000\000\144\000\000\000\000\000\000\000\001\000' \
  > corpus/transfer_url/signed-exact
printf '\001\012\000\000\000\000\000\000\000\144\000\000\000\000\000\000\000\000\001\016bytes=10-110 ' \
  > corpus/transfer_url/signed-other

# The first 8 bytes are the file size (1000, little-endian), then Range
# values, one per line.
size='\350\003\000\000\000\000\000\000'
for range in 'bytes=0-99' 'bytes=100-' 'bytes=-10' 'bytes=-0' 'bytes=5-1' \
  'BYTES = 1-2' 'bytes=0-99,200-299' 'items=0-1' $'bytes=0-1\nbytes=2-3'; do
  printf "$size%s" "$range" > "corpus/reconstruction_range/$(printf '%s' "$range" | b64url)"
done
