//! The records the fixture's other crates encode and report on.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub workspace: String,
    pub units: u32,
    pub bytes: u64,
}

impl Build {
    #[must_use]
    pub fn total_bytes(builds: &[Self]) -> u64 {
        builds.iter().map(|build| build.bytes).sum()
    }
}
