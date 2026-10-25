# 0011: Use hybrid post-quantum key exchange by default, and keep signatures agile

<!-- description: Hybrid post-quantum key exchange by default for TLS and SSH, algorithm-agile token verification with a path to ML-DSA, and no change to storage hashes. -->

- Status: accepted
- Date: 2026-10-24
- Scope: `transfer`, `client`, `server`, `ssh`, `auth`, `protocol`, `docs`

## Context

A cryptographically relevant quantum computer would break today's
elliptic-curve key exchange and signatures (Shor's algorithm) and weaken
symmetric keys and hashes only by a square root (Grover's algorithm).
Traffic recorded now can be decrypted later, so key exchange matters
today; a signature matters only while a forgery can still be used. NIST
published ML-KEM (FIPS 203) and ML-DSA (FIPS 204) in August 2024.

What was checked on 2026-10-24, and against which versions:

**TLS.**
- rustls supports the hybrid `X25519MLKEM768` with its `aws-lc-rs`
  provider since 0.23.22, offered at the lowest priority, and prefers it
  since 0.23.27, which added `prefer-post-quantum` to its default
  features (rustls release notes for v/0.23.22 and v/0.23.27; source of
  0.23.46, `src/crypto/aws_lc_rs/mod.rs`, `DEFAULT_KX_GROUPS`).
- openxet's `Cargo.lock` has rustls 0.23.46, hyper-rustls 0.27.10 and
  reqwest 0.13.5 with the `aws-lc-rs` provider. reqwest depends on
  rustls without its default features, so `cargo tree -e features -i
  rustls` shows `prefer-post-quantum` off in openxet's build: the hybrid
  is offered last, after X25519. Cargo unifies features, so enabling it
  once in the workspace applies to every rustls client openxet builds.
- The server speaks plain HTTP and leaves TLS to a proxy
  ([configuration](../reference/config.md)). OpenSSL 3.5.0 (2025-04-08)
  offers `X25519MLKEM768` and `X25519` as its default key shares (OpenSSL
  3.5 release notes), and Go 1.24's `crypto/tls` enables `X25519MLKEM768`
  by default (Go 1.24 release notes). Proxies built on either negotiate
  it unless their config overrides the groups.

**SSH** (M4, design §10.1).
- OpenSSH made `sntrup761x25519-sha512@openssh.com` its default key
  exchange in 9.0 (2022-04-08), added `mlkem768x25519-sha256` in 9.9
  (2024-09-19) and made it the default in 10.0 (2025-04-09). 10.1 warns
  when a connection negotiates a key exchange that isn't post-quantum
  (OpenSSH release notes).
- `russh` 0.64.1 (2026-10-05) implements `mlkem768x25519-sha256` and
  puts it first in its default order (`SAFE_KEX_ORDER`,
  `src/negotiation.rs`, read in the crate source). It does not
  implement `sntrup761x25519-sha512`. M4 has not chosen its SSH library
  yet (S9 names `russh` as an example).

**Tokens and signed URLs.**
- CAS tokens are Ed25519 JWTs (ADR 0007). The verifier accepts exactly
  the header `{"alg":"EdDSA","typ":"JWT","kid":"…"}` and picks the key by
  `kid` (M1-R18). Ed25519 falls to Shor: a quantum attacker could derive
  the private key from a published public key. Because tokens live at
  most 24 h by default (M1-R19), a recorded token is useless later; only an attacker who can
  forge in real time matters, and that needs a quantum computer that
  exists while the key is still trusted.
- Signed transfer URLs and dedup answers use HMAC-SHA256 with 256-bit
  keys (§5.5, §5.4). Grover leaves about 2^128 work against such a key,
  so HMAC-SHA256 stays secure.
- ML-DSA for JOSE and COSE is published as RFC 9964 (May 2026, Standards
  Track; it was `draft-ietf-cose-dilithium`), with `alg` values
  `ML-DSA-44`, `ML-DSA-65` and `ML-DSA-87` and the `AKP` key type.
  `aws-lc-rs` 1.18.2 (in openxet's lock file) exposes ML-DSA verification
  in its stable `signature` module; RustCrypto's `ml-dsa` is at 0.1.1.
  Support in the JWT libraries platforms sign with was not surveyed.

**Storage hashes.** Xet chunk, xorb and file hashes are 256-bit keyed
BLAKE3 (`xet-core-structures` 1.7.0, `merklehash`), and LFS oids are
SHA-256. Grover leaves about 2^128 work for a preimage, and quantum
collision search is not cheaper than classical search once its hardware
cost is counted (D. J. Bernstein, "Cost analysis of hash collisions:
will quantum computers make SHARCS obsolete?", 2009). Their margins are
adequate. Changing them would also break protocol exactness
(non-negotiable 1).

## Decision

1. **TLS.** Hybrid post-quantum key exchange (`X25519MLKEM768`) is on by
   default wherever openxet makes or terminates TLS. The workspace
   enables rustls's `prefer-post-quantum`, and a test checks that the
   client and the server's outbound clients negotiate it (issue #66, M3).
   The operator docs recommend a TLS-terminating proxy that negotiates it.
2. **SSH.** M4's SSH server offers `mlkem768x25519-sha256` first, then
   `curve25519-sha256`. `russh` 0.64.1 supports this; if M4 chooses it,
   its order is pinned explicitly. Whether to add
   `sntrup761x25519-sha512` for OpenSSH 9.0–9.8 clients is decided in the
   M4 plan (issue #67, M4).
3. **Tokens stay Ed25519 for now, with agile verification.** Each key in
   the key set is bound to one `alg`. A token is accepted only if its
   `kid` names a known key and its header `alg` equals that key's; any
   other `alg` is rejected, so no token can choose its own algorithm.
   Adding ML-DSA (RFC 9964, `ML-DSA-65` first) is then a new allowed
   `alg` and a key rotation, not a format change. It takes its own ADR
   once a reviewed Rust implementation is chosen and the platforms'
   signing libraries support it.
4. **Key rotation is drilled.** M3 runs a rotation drill for the CAS
   token keys and the transfer HMAC key under load (issue #65, M3), so
   switching algorithm is routine when it is needed.
5. **No change to HMAC-SHA256, BLAKE3 or SHA-256.**

## Consequences

- The client and every outbound connection resist recorded-traffic
  decryption from M3, at the cost of larger TLS handshakes (rustls's
  own measurements, linked from its 0.23.22 notes).
- Self-hosters behind an older proxy don't get post-quantum TLS on the
  client-to-proxy leg; the docs say which proxies do.
- The token verifier gains a per-key `alg` binding. ADR 0007's format is
  unchanged: today every key is `EdDSA`, and the existing
  `jwt_wrong_alg_rejected` test keeps holding.
- Tests named above: the negotiated-group test (#66), the SSH key
  exchange compatibility test (#67), and the rotation drill (#65).

## Alternatives considered

- **Wait for a quantum computer.** Rejected: recorded TLS and SSH
  traffic stays decryptable, and the switch costs a feature flag now.
- **ML-DSA tokens now.** Rejected for now: platforms mint their own
  tokens (ADR 0007), and they need ML-DSA in their JWT libraries first.
  Signatures on short-lived tokens are not exposed to recording.
- **Pure ML-KEM key exchange without X25519.** Rejected: the hybrid keeps
  classical security if ML-KEM is broken, and it is what OpenSSL, Go,
  OpenSSH and rustls default to.
