//! openxet's client: upload and download over the Xet protocol, built on
//! Hugging Face's `xet-data` 1.7.0 so that chunking, xorbs and shards are
//! byte-identical with stock clients (ADR 0006, design §10.8).
//!
//! This is the only crate that may depend on `xet-data`, `xet-runtime`
//! and `xet-client` (`deny.toml` `[bans]`). Telemetry is always off
//! (non-negotiable 6).

#![forbid(unsafe_code)]

mod download;
mod endpoint;
mod error;
mod net;
mod session;
mod upload;

pub use download::{PARTIAL_SUFFIX, download, is_hash};
pub use endpoint::{EXPIRY_MARGIN_SECS, Endpoint, STATIC_TOKEN_EXP};
pub use error::{ClientError, InputProblem, Problem};
pub use session::{OnProgress, Options, Progress};
pub use upload::{Uploaded, Uploads, upload};
