//! Shard bodies and the `/v2/shards` event stream, for shard upload tests.
//!
//! [`Event`] is a copy of `hf_xet` 1.7.0's own stream type
//! (`xet-client` 1.7.0, `cas_types/mod.rs:309-343`; S10 §4), so a stream
//! that parses here parses in the stock client.

use std::path::PathBuf;

use openxet_protocol::{MerkleHash, hash_from_hex};
use serde::Deserialize;

/// The file hashes of the `hf_xet` 1.7.0 capture's shard, in shard order:
/// the empty file, `a.bin` and `b.bin` (`crates/protocol/tests/hf_xet_upload.rs`).
pub const HF_XET_FILES: [&str; 3] = [
    "0000000000000000000000000000000000000000000000000000000000000000",
    "65fef04da9e2708da8ca1acc831962d0686875eff24218d24e108362eeb420df",
    "79359c577af7d8f7e7796f289dac3525f2f7ce75d6408a4ed2ff8ec56b316dc0",
];

/// The footer-less shard body stock `hf_xet` 1.7.0 uploaded with
/// [`super::xorb::hf_xet_xorb`], captured by `scripts/capture-hf-xet-upload.py`.
pub fn hf_xet_shard() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/hf-xet-1.7.0-upload/upload.shard");
    std::fs::read(path).unwrap()
}

/// The capture's file hashes.
pub fn hf_xet_files() -> Vec<MerkleHash> {
    HF_XET_FILES
        .iter()
        .map(|h| hash_from_hex(h).unwrap())
        .collect()
}

/// `ShardUploadEvent` as `hf_xet` 1.7.0 declares it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Verification progress, in files.
    Validating {
        /// Files verified.
        verified: u64,
        /// Files in the shard.
        total: u64,
    },
    /// The durable write.
    Committing {
        /// Which step of it runs.
        stage: CommitStage,
    },
    /// Terminal success.
    Result,
    /// Terminal failure.
    Error {
        /// The only server text the user sees.
        message: String,
        /// Whether the client retries the whole upload.
        #[serde(default)]
        retryable: bool,
    },
    /// Any other `type`, which the client ignores.
    #[serde(other)]
    Unknown,
}

/// `CommitStage` as `hf_xet` 1.7.0 declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitStage {
    /// Writing the shard to the bucket.
    Uploading,
    /// Registering it in the index.
    Syncing,
}

/// One line of a `/v2/shards` stream: the raw JSON and its parse.
#[derive(Debug, Clone)]
pub struct Line {
    /// The line as JSON.
    pub json: serde_json::Value,
    /// The line as the client parses it.
    pub event: Event,
}

/// Splits a whole `/v2/shards` body into its lines, asserting each is
/// newline-terminated JSON the client parses.
pub fn lines(body: &str) -> Vec<Line> {
    assert!(body.ends_with('\n'), "the stream ends mid-line: {body:?}");
    body.lines()
        .map(|line| Line {
            json: serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}")),
            event: serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("not an hf_xet event ({e}): {line:?}")),
        })
        .collect()
}

/// The stream's terminal event, asserting it is the last line and the only
/// terminal one.
pub fn terminal(lines: &[Line]) -> &Line {
    let terminals: Vec<&Line> = lines
        .iter()
        .filter(|l| matches!(l.event, Event::Result | Event::Error { .. }))
        .collect();
    assert_eq!(terminals.len(), 1, "{lines:?}");
    let last = lines.last().unwrap();
    assert!(
        matches!(last.event, Event::Result | Event::Error { .. }),
        "{lines:?}"
    );
    last
}
