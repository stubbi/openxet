//! The time credentials are checked against.

use std::time::{SystemTime, UNIX_EPOCH};

/// The current time, in Unix seconds. Credentials are checked against it, so
/// tests can control time.
pub trait Clock: Send + Sync + 'static {
    /// Seconds since the Unix epoch.
    fn now(&self) -> u64;
}

/// The system's wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        // A clock before 1970 is a misconfigured host; treating it as the
        // epoch makes every CAS token read as not yet issued, not as valid.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}
