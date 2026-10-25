//! What the Hub's tests share: a Hub over a temporary index and bucket,
//! composed as `openxet serve` composes it ([`TestHub`]), and comparisons
//! against spike S3's huggingface.co fixtures ([`assert_read_keys`]). Only
//! with the `test-support` feature.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use openxet_auth::{
    Action, Authenticator, Authorizer, Glob, Rule, RulesAuthorizer, StaticToken, TokenSigner,
    UserProfile, generate_keypair,
};
use openxet_git::{Engine, GitConfig};
use openxet_http::BodyDeadline;
use openxet_index::SqliteIndex;
use openxet_storage::{Bucket, LocalBucket};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt as _;
use url::Url;

use crate::{HubConfig, HubState};

/// The scheme of every test credential. A constant, so the repository's
/// secret scanner can tell test tokens from leaks.
pub const BEARER: &str = "Bearer";

/// The public URL of every [`TestHub`].
pub const PUBLIC_URL: &str = "http://127.0.0.1:8080";

/// One exchange of `compat/fixtures/hub/`, by file name.
#[must_use]
pub fn fixture(name: &str) -> Value {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// The JSON response body of the fixture `name`.
#[must_use]
pub fn fixture_json(name: &str) -> Value {
    fixture(name)["response"]["body"]["json"].clone()
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/fixtures/hub")
}

/// Asserts that `actual` has every key path in `keys`, each holding a JSON
/// value of the same kind as in `expected`, huggingface.co's answer: the
/// fields S3 lists as read by the client (`docs/research/S3-hub-api-shapes.md`).
///
/// A path is dot-separated keys; `name[]` steps into every element of the
/// array `name` (the first element of `expected` gives the kind), so
/// `orgs[].name` checks each org's `name`. An empty array in `actual`
/// checks nothing below it, as the client reads nothing there.
pub fn assert_read_keys(expected: &Value, actual: &Value, keys: &[&str]) {
    for key in keys {
        let segments: Vec<&str> = key.split('.').collect();
        check_path(expected, actual, &segments, key);
    }
}

fn check_path(expected: &Value, actual: &Value, segments: &[&str], key: &str) {
    let Some((first, rest)) = segments.split_first() else {
        assert_eq!(
            kind(actual),
            kind(expected),
            "{key}: expected a {} like huggingface.co's {expected}, got {actual}",
            kind(expected)
        );
        return;
    };
    let (name, each) = match first.strip_suffix("[]") {
        Some(name) => (name, true),
        None => (*first, false),
    };
    let expected = expected
        .get(name)
        .unwrap_or_else(|| panic!("{key}: the fixture has no {name:?}; fix the key path"));
    let actual = actual
        .get(name)
        .unwrap_or_else(|| panic!("{key}: the answer has no {name:?}: {actual}"));
    if !each {
        check_path(expected, actual, rest, key);
        return;
    }
    let first_expected = expected
        .as_array()
        .and_then(|a| a.first())
        .unwrap_or_else(|| panic!("{key}: the fixture's {name:?} is no non-empty array"));
    let actual = actual
        .as_array()
        .unwrap_or_else(|| panic!("{key}: the answer's {name:?} is no array: {actual}"));
    for element in actual {
        check_path(first_expected, element, rest, key);
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A user token for `subject`, carrying `orgs` as its `orgs` claim.
#[must_use]
pub fn user_token(token: &str, subject: &str, orgs: &[&str]) -> StaticToken {
    let mut claims = BTreeMap::new();
    if !orgs.is_empty() {
        claims.insert(
            "orgs".to_owned(),
            orgs.iter().map(|&o| o.to_owned()).collect(),
        );
    }
    StaticToken {
        sha256: StaticToken::digest(token),
        subject: subject.to_owned(),
        cas: None,
        user: Some(UserProfile {
            name: None,
            email: None,
            claims,
        }),
    }
}

/// A rule letting `subjects` do `actions` on `repos` (`*`, `org/*`, …);
/// `anonymous` as a subject grants everyone.
#[must_use]
pub fn rule(subjects: &[&str], repos: &[&str], actions: &[Action]) -> Rule {
    Rule {
        subjects: subjects.iter().map(|&s| s.to_owned()).collect(),
        claims: BTreeMap::new(),
        repos: repos.iter().map(|r| Glob::new(r).unwrap()).collect(),
        actions: actions.to_vec(),
    }
}

/// Every action: read, write and create.
pub const ALL: &[Action] = &[Action::Read, Action::Write, Action::Create];

/// A Hub over a fresh index and bucket in a temporary directory, composed
/// as `openxet serve` composes it.
pub struct TestHub {
    /// The composed router.
    pub router: Router,
    /// The Hub's state.
    pub state: HubState,
    /// Where the index and the bucket live; removed on drop.
    pub dir: TempDir,
}

impl TestHub {
    /// A Hub accepting `tokens` and applying `rules`.
    pub async fn start(tokens: Vec<StaticToken>, rules: Vec<Rule>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bucket = Arc::new(LocalBucket::open(&dir.path().join("bucket")).unwrap());
        Self::start_with(dir, bucket, tokens, rules).await
    }

    /// [`TestHub::start`] over `bucket`, with the index in `dir`.
    pub async fn start_with(
        dir: TempDir,
        bucket: Arc<dyn Bucket>,
        tokens: Vec<StaticToken>,
        rules: Vec<Rule>,
    ) -> Self {
        Self::start_custom(dir, bucket, tokens, Arc::new(RulesAuthorizer::new(rules))).await
    }

    /// [`TestHub::start_with`] deciding with `authz` instead of rules.
    pub async fn start_custom(
        dir: TempDir,
        bucket: Arc<dyn Bucket>,
        tokens: Vec<StaticToken>,
        authz: Arc<dyn Authorizer>,
    ) -> Self {
        Self::start_config(dir, bucket, tokens, authz, Self::config()).await
    }

    /// The configuration of every [`TestHub`]: the defaults, at
    /// [`PUBLIC_URL`].
    #[must_use]
    pub fn config() -> HubConfig {
        let public_url = Url::parse(PUBLIC_URL).unwrap();
        HubConfig {
            public_url: public_url.clone(),
            cas_url: public_url,
            lfs_threshold: HubConfig::DEFAULT_LFS_THRESHOLD,
            max_commit_bytes: HubConfig::DEFAULT_MAX_COMMIT_BYTES,
            token_ttl: HubConfig::DEFAULT_TOKEN_TTL,
            page_size: HubConfig::MAX_PAGE_SIZE,
        }
    }

    /// [`TestHub::start_custom`] with `cfg`: a Git engine with its cache
    /// in `dir`, and a fresh CAS token key whose tokens the authenticator
    /// accepts, as `openxet serve` sets them up.
    pub async fn start_config(
        dir: TempDir,
        bucket: Arc<dyn Bucket>,
        tokens: Vec<StaticToken>,
        authz: Arc<dyn Authorizer>,
        cfg: HubConfig,
    ) -> Self {
        let index = Arc::new(
            SqliteIndex::open(&dir.path().join("index.sqlite"))
                .await
                .unwrap(),
        );
        let engine = Engine::start(
            Arc::clone(&index) as _,
            Arc::clone(&bucket),
            GitConfig::new(dir.path().join("git-cache")),
        )
        .await
        .unwrap();
        let signer = TokenSigner::from_pem(generate_keypair().unwrap().private_pem()).unwrap();
        let auth = Authenticator::new(
            tokens,
            vec![signer.public_key()],
            Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        )
        .unwrap();
        let state = HubState::new(
            index,
            bucket,
            Arc::new(auth),
            authz,
            cfg,
            Arc::new(engine),
            Arc::new(signer),
        );
        Self::with_state(dir, state)
    }

    /// A Hub serving `state`, whose index and bucket live in `dir`.
    #[must_use]
    pub fn with_state(dir: TempDir, state: HubState) -> Self {
        let router =
            openxet_http::compose(vec![crate::mounted(state.clone())], BodyDeadline::DEFAULT);
        Self { router, state, dir }
    }

    /// The index database's path, for tests that read its tables.
    #[must_use]
    pub fn index_path(&self) -> PathBuf {
        self.dir.path().join("index.sqlite")
    }

    /// Sends `method path` with `token` as the `Bearer` credential, if any,
    /// and `body` as JSON, if any.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> Reply {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("{BEARER} {token}"));
        }
        let body = match body {
            Some(json) => {
                request = request.header("content-type", "application/json");
                Body::from(serde_json::to_vec(json).unwrap())
            }
            None => Body::empty(),
        };
        self.send(request.body(body).unwrap()).await
    }

    /// Sends `method path` with `token` as the `Bearer` credential, if any,
    /// and `body` as it is, of `content_type`.
    pub async fn call_raw(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        content_type: &str,
        body: impl Into<Body>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", content_type);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("{BEARER} {token}"));
        }
        self.send(request.body(body.into()).unwrap()).await
    }

    /// Posts `lines` as a commit body to `{prefix}/commit/{rev}`, each
    /// line as `huggingface_hub` writes it: JSON with Python's separators.
    pub async fn commit(&self, token: &str, prefix: &str, rev: &str, lines: &[Value]) -> Reply {
        let body: String = lines
            .iter()
            .flat_map(|l| [python_json(l), "\n".to_owned()])
            .collect();
        self.call_raw(
            Method::POST,
            &format!("{prefix}/commit/{rev}"),
            Some(token),
            "application/x-ndjson",
            body,
        )
        .await
    }

    /// Sends `request` as it is.
    pub async fn send(&self, request: Request<Body>) -> Reply {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        Reply {
            status,
            headers,
            body,
        }
    }

    /// Creates `{namespace}/{name}` of `kind` (`None` for the untyped
    /// create) with `token`, and asserts it succeeded.
    pub async fn create(&self, token: &str, namespace: &str, name: &str, kind: Option<&str>) {
        let mut body = serde_json::json!({ "name": name, "organization": namespace });
        if let Some(kind) = kind {
            body["type"] = kind.into();
        }
        let reply = self
            .call(Method::POST, "/api/repos/create", Some(token), Some(&body))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "create: {}", reply.text());
    }
}

/// A response, read whole.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Its status.
    pub status: StatusCode,
    /// Its headers.
    pub headers: HeaderMap,
    /// Its body.
    pub body: Bytes,
}

impl Reply {
    /// The body as text.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON.
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|err| panic!("not JSON ({err}): {}", self.text()))
    }

    /// A header's value as text, if present.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }

    /// Status, headers and body, without the headers that differ per
    /// request (`X-Request-Id`, `Date`): what two answers must share to be
    /// indistinguishable (ruling M2-R6).
    #[must_use]
    pub fn comparable(&self) -> (StatusCode, Vec<(String, Vec<u8>)>, Bytes) {
        let mut headers: Vec<(String, Vec<u8>)> = self
            .headers
            .iter()
            .filter(|(name, _)| !matches!(name.as_str(), "x-request-id" | "date"))
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect();
        headers.sort();
        (self.status, headers, self.body.clone())
    }
}

/// `value` as Python's `json.dumps` writes it by default: `", "` between
/// items and `": "` after keys, non-ASCII escaped (`_commit_api.py` line
/// 1055, S3).
#[must_use]
pub fn python_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        python_json(&Value::String(k.clone())),
                        python_json(v)
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(python_json).collect();
            format!("[{}]", items.join(", "))
        }
        Value::String(text) => {
            let mut out = String::from("\"");
            for c in text.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
                    c => {
                        let mut units = [0u16; 2];
                        for unit in c.encode_utf16(&mut units) {
                            let _ = write!(out, "\\u{unit:04x}");
                        }
                    }
                }
            }
            out.push('"');
            out
        }
        other => other.to_string(),
    }
}
