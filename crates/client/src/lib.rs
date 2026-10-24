//! openxet's client: upload and download over the Xet protocol, built on
//! Hugging Face's `xet-data` 1.7.0 so that chunking, xorbs and shards are
//! byte-identical with stock clients (ADR 0006, design §10.8).
//!
//! This is the only crate that may depend on `xet-data`, `xet-runtime`
//! and `xet-client` (`deny.toml` `[bans]`). Telemetry is always off
//! (non-negotiable 6).

#![forbid(unsafe_code)]

mod endpoint;
mod error;
mod transfer;

pub use endpoint::{EXPIRY_MARGIN_SECS, Endpoint, STATIC_TOKEN_EXP};
pub use error::{ClientError, InputProblem, Problem};
pub use transfer::{OnProgress, Options, Progress, Uploaded, download, is_hash, upload};
