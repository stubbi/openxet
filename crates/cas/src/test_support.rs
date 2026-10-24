//! Shared fixtures for the crate's unit tests.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use openxet_auth::{Authenticator, StaticToken, UrlSigner};
use openxet_index::{SqliteIndex, Visibility};
use openxet_protocol::VerifyLimits;
use openxet_storage::LocalBucket;
use url::Url;

use crate::{AppState, CasConfig};

/// State over a fresh index and bucket in `dir`, accepting `tokens`.
pub(crate) async fn state(dir: &Path, tokens: Vec<StaticToken>) -> AppState {
    AppState::new(
        Arc::new(SqliteIndex::open(&dir.join("index.sqlite")).await.unwrap()),
        Arc::new(LocalBucket::open(&dir.join("bucket")).unwrap()),
        Arc::new(
            Authenticator::new(tokens, vec![], Authenticator::DEFAULT_MAX_TOKEN_LIFETIME).unwrap(),
        ),
        Arc::new(UrlSigner::new([1; 32], UrlSigner::DEFAULT_LIFETIME).unwrap()),
        CasConfig {
            public_url: Url::parse("http://127.0.0.1:8080").unwrap(),
            visibility: Visibility::Global,
            verify_limits: VerifyLimits::default(),
            heartbeat_interval: CasConfig::DEFAULT_HEARTBEAT_INTERVAL,
            body_deadline: crate::BodyDeadline::DEFAULT,
        },
    )
}
