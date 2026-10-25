//! How long sending an answer may take.

use std::time::Duration;

/// How long the server may take to send a dedup or reconstruction answer
/// (rulings M1-R33, M1-R40): `base`, plus the answer's length at
/// `min_rate` bytes per second. When it passes, the answer ends with an
/// error and its memory, with its `dedup` permit or memory reservation, is
/// released, so a client that never reads holds them for at most this
/// long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendDeadline {
    base: Duration,
    min_rate: u64,
}

impl SendDeadline {
    /// The default: 30 s plus 1 MiB/s, so a full 16 MiB dedup answer may
    /// take 46 s and a 42 MB answer for 100k terms 70 s. Even at
    /// 10 Mbit/s either takes under half of that (13 s, 34 s), so only a
    /// client that stalls is cut off, and what it holds is freed well inside
    /// `hf_xet`'s 300 s read timeout (S10 §4).
    pub const DEFAULT: Self = Self {
        base: Duration::from_secs(30),
        min_rate: 1024 * 1024,
    };

    /// A deadline with these bounds, or `None` if either is zero.
    #[must_use]
    pub fn new(base: Duration, min_rate: u64) -> Option<Self> {
        (!base.is_zero() && min_rate > 0).then_some(Self { base, min_rate })
    }

    /// The time allowed to send `len` bytes.
    #[must_use]
    pub fn for_len(&self, len: u64) -> Duration {
        self.base + Duration::from_millis(len.saturating_mul(1000) / self.min_rate)
    }
}
