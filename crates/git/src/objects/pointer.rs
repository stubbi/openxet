//! Git LFS pointers (ADR 0005 item 9; git-lfs `docs/spec.md`): openxet
//! writes only the canonical three keys, and reads pointers with keys it
//! doesn't know, since the spec allows them.

use std::collections::HashSet;
use std::fmt::Write as _;

/// The spec URL a pointer's `version` line names.
const SPEC: &str = "https://git-lfs.github.com/spec/v1";

/// The pre-release spec URL git-lfs still reads (`lfs/pointer.go`,
/// `v1Aliases`).
const LEGACY_SPEC: &str = "https://hawser.github.com/spec/v1";

/// "Pointer files must be less than 1024 bytes in size" (git-lfs
/// `docs/spec.md`).
pub(crate) const MAX_POINTER_BYTES: usize = 1023;

/// What an LFS pointer points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pointer {
    /// The file's SHA-256.
    pub sha256: [u8; 32],
    /// The file's size in bytes.
    pub size: u64,
}

impl Pointer {
    /// The SHA-256 in lowercase hex, as the pointer's `oid` and the Hub's
    /// `lfs.oid` write it.
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        self.sha256
            .iter()
            .fold(String::with_capacity(64), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
    }
}

/// The canonical pointer for a file (ADR 0005 item 9): exactly
/// `version https://git-lfs.github.com/spec/v1`, `oid sha256:<hex>` and
/// `size <n>`, in that order, each line `\n`-terminated, and nothing else,
/// so every tool that writes the file gets the same blob id.
#[must_use]
pub fn canonical_pointer(sha256: &[u8; 32], size: u64) -> Vec<u8> {
    let pointer = Pointer {
        sha256: *sha256,
        size,
    };
    format!(
        "version {SPEC}\noid sha256:{}\nsize {size}\n",
        pointer.sha256_hex()
    )
    .into_bytes()
}

/// Reads `blob` as an LFS pointer: under 1024 bytes of UTF-8 lines
/// `key value`, `version` first naming the v1 spec, one `oid sha256:<64
/// lowercase hex>` and one `size <decimal>`, and any other keys made of
/// lowercase letters, digits, `.` and `-`, each at most once. Unknown keys
/// are tolerated (never written back: openxet doesn't rewrite stored
/// pointers). The last line's `\n` may be missing. `None` for anything else.
#[must_use]
pub fn parse_pointer(blob: &[u8]) -> Option<Pointer> {
    if blob.len() > MAX_POINTER_BYTES {
        return None;
    }
    let text = std::str::from_utf8(blob).ok()?;
    let mut lines = text.strip_suffix('\n').unwrap_or(text).split('\n');
    let version = lines.next()?.strip_prefix("version ")?;
    if version != SPEC && version != LEGACY_SPEC {
        return None;
    }
    let (mut sha256, mut size) = (None, None);
    let mut seen = HashSet::new();
    for line in lines {
        let (key, value) = line.split_once(' ')?;
        let key_ok = !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
        if !key_ok || value.is_empty() || key == "version" || !seen.insert(key) {
            return None;
        }
        match key {
            "oid" => sha256 = Some(parse_sha256(value)?),
            "size" => size = Some(parse_size(value)?),
            _ => {}
        }
    }
    Some(Pointer {
        sha256: sha256?,
        size: size?,
    })
}

fn parse_sha256(value: &str) -> Option<[u8; 32]> {
    let hex = value.strip_prefix("sha256:")?.as_bytes();
    if hex.len() != 64 {
        return None;
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.chunks_exact(2)) {
        *byte = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(out)
}

fn parse_size(value: &str) -> Option<u64> {
    if value.bytes().all(|b| b.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{canonical_pointer, parse_pointer};

    #[test]
    fn round_trips_and_legacy_version() {
        let blob = canonical_pointer(&[1; 32], 0);
        let p = parse_pointer(&blob).unwrap_or_else(|| panic!("canonical parses"));
        assert_eq!((p.sha256, p.size), ([1; 32], 0));
        let legacy = String::from_utf8_lossy(&blob).replace("git-lfs", "hawser");
        assert_eq!(parse_pointer(legacy.as_bytes()), Some(p));
        let no_newline = String::from_utf8_lossy(&blob).trim_end().to_owned();
        assert_eq!(parse_pointer(no_newline.as_bytes()), Some(p));
        assert_eq!(
            parse_pointer(b"version https://git-lfs.github.com/spec/v1\n"),
            None
        );
    }
}
