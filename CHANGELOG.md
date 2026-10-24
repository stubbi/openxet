# Changelog

What changes for users and operators, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

The storage-only Xet CAS (milestone M1). Stock `hf_xet` 1.7.0 and newer
upload to and download from it directly; the
[quickstart](docs/quickstart-storage-only.md) runs it in under a second
once built.

### Added

- `openxet serve`: the Xet CAS on local disk with a SQLite index. Without
  a configuration file it runs in dev mode and prints one write token;
  with one, it takes static tokens and Ed25519-signed CAS tokens
  ([config](docs/reference/config.md)).
- `openxet config check`, and `openxet token keygen`, `mint` and
  `generate` for credentials.
- `openxet upload` and `openxet download`, on the same client core as
  `hf_xet`, with `--json` output and stable exit codes
  ([command line](docs/reference/cli.md)).
- The CAS routes ([routes](docs/reference/routes.md)):
  - xorb upload with validation of every chunk
  - shard upload, v1 and the v2 progress stream, verified against the
    stored chunk hashes before any file is registered
  - global dedup, answering with HMAC-keyed shards that offer every xorb
    of the newest upload holding the matched chunk
  - reconstruction v1, v2 and batch, with transfer URLs signed for one
    byte range each
  - telemetry accepted and dropped
- Every security requirement of design §5.9 that applies to the CAS, each
  with its test ([security tests](docs/reference/security-tests.md)).

### Upgrading

- Shards registered before index migration 0002 have no list of their
  xorbs, so a dedup match in one offers only the matched xorb, not the
  rest of that upload. A xorb gets the full answer again once a shard
  that references it is registered after the migration: the same shard
  uploaded again, or a new upload of any file that uses the xorb. A
  `reindex` command that rebuilds the index from the bucket
  ([#26](https://github.com/stubbi/openxet/issues/26)) will fill in every
  shard.
