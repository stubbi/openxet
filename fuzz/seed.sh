#!/usr/bin/env bash
# Fills fuzz/corpus/<target>/ from the canonical fixtures in testdata/, which
# stay the single copy, and rewrites fuzz/Cargo.lock. Run it before fuzzing,
# and after a change to Cargo.lock, committing the new fuzz/Cargo.lock with
# it (CI fails if they differ). The corpus is not committed.
set -euo pipefail

cd "$(dirname "$0")"

# Fuzz the dependency versions the workspace ships: the fuzz workspace's
# lockfile starts from the workspace's, and cargo adds only what fuzzing
# alone needs, pinned exactly in fuzz/Cargo.toml.
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

# The xet:// parser's seeds are the specification's golden vectors
# (crates/uri/testdata/vectors.json, the single copy), one input per file,
# valid and malformed, legacy forms included.
mkdir -p corpus/xet_uri
python3 -I - ../crates/uri/testdata/vectors.json corpus/xet_uri <<'PY'
import json
import pathlib
import sys

vectors = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
for i, vector in enumerate(vectors["valid"] + vectors["invalid"]):
    (pathlib.Path(sys.argv[2]) / f"vector-{i:03}").write_text(vector["input"], encoding="utf-8")
PY

# pkt-line requests as git sends them: a v2 ls-refs and fetch, a v0 want
# section with capabilities and haves, a v2 command openxet refuses, and a
# bad length.
mkdir -p corpus/pkt_line
pkt() { printf '%04x%s\n' $((${#1} + 5)) "$1"; }
oid=0123456789abcdef0123456789abcdef01234567
{ pkt command=ls-refs; pkt agent=git/2.39.5; pkt object-format=sha1; printf 0001
  pkt peel; pkt symrefs; pkt unborn; pkt 'ref-prefix HEAD'; pkt 'ref-prefix refs/heads/'
  printf 0000; } > corpus/pkt_line/v2-ls-refs
{ pkt command=fetch; pkt object-format=sha1; printf 0001; pkt thin-pack; pkt ofs-delta
  pkt "want $oid"; pkt "have $oid"; pkt done; printf 0000; } > corpus/pkt_line/v2-fetch
{ pkt "want $oid multi_ack_detailed side-band-64k thin-pack ofs-delta agent=git/2.39.5"
  pkt "want $oid"; pkt 'deepen 1'; printf 0000; pkt "have $oid"; pkt done; } \
  > corpus/pkt_line/v0-wants
{ pkt command=object-info; printf 0001; pkt size; pkt "oid $oid"; printf 0000; } \
  > corpus/pkt_line/v2-object-info
printf '0003fff1zzzz' > corpus/pkt_line/bad-length

# Hub commit bodies as huggingface_hub sends them (S3 fixtures 09-…-14 and
# 10-…-02): byte 0 the regular-file limit, byte 1 the operation limit, then
# the NDJSON, Python's separators included.
mkdir -p corpus/commit_ndjson
python3 -I - ../compat/fixtures/hub corpus/commit_ndjson <<'PY'
import json
import pathlib
import sys

fixtures = pathlib.Path(sys.argv[1])
for name in ("09-upload-folder--14-POST-models-commit.json",
             "10-create-commit-delete--02-POST-models-commit.json"):
    body = json.loads((fixtures / name).read_text(encoding="utf-8"))["request"]["body"]["text"]
    (pathlib.Path(sys.argv[2]) / name).write_bytes(b"\xff\x10" + body.encode())
PY

# A receive-pack request and a hook request are written here: pkt-lines of
# commands, then a pack header (receive_request, byte 0 the chunk size), and
# the hook's token, pack and command lines (hook_request, with the token
# the target checks against).
mkdir -p corpus/receive_request corpus/hook_request
rawpkt() { printf '%04x%s' $((${#1} + 4)) "$1"; }
zero=0000000000000000000000000000000000000000
oid=1111111111111111111111111111111111111111
{
  caps='report-status side-band-64k'
  printf '\007%04x' $((97 + 1 + ${#caps} + 4))
  printf '%s %s refs/heads/main\000%s' "$zero" "$oid" "$caps"
  printf '0000PACK\000\000\000\002\000\000\000\003'
  head -c 64 /dev/zero
} > corpus/receive_request/push
{
  caps='report-status push-options'
  printf '\001%04x' $((97 + 1 + ${#caps} + 4))
  printf '%s %s refs/heads/main\000%s' "$zero" "$oid" "$caps"
  printf '0000'
  rawpkt 'an option'
  printf '0000PACK\000\000\000\002\000\000\000\001'
} > corpus/receive_request/push-options
{
  printf '\003'
  rawpkt "shallow $oid"
  rawpkt "$oid $zero refs/heads/old"
  printf '0000'
} > corpus/receive_request/delete
token=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
{
  rawpkt "token $token"
  rawpkt "pack /cache/snapshots/x/objects/pack/pack-$oid.pack"
  rawpkt "command $zero $oid refs/heads/main"
  rawpkt "repo 0192f3a4b5c67d8e9fa0b1c2d3e4f5a6"
  printf '0000'
} > corpus/hook_request/push
{
  rawpkt "token $token"
  rawpkt "command $oid $zero refs/heads/old"
  printf '0000'
} > corpus/hook_request/delete
