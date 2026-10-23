//! The CAS test server, shared by every integration test of `openxet-cas`.
//!
//! [`TestServer::start`] serves [`openxet_cas::router`] over real HTTP/1.1 on
//! `127.0.0.1:0`, backed by a temporary `SqliteIndex` and `LocalBucket`, and
//! captures every log line the server writes ([`TestServer::logs`]).
//! [`TestServer::start_with`] swaps in another bucket, such as
//! [`FailingBucket`].
//!
//! [`ROUTES`] lists every route the CAS serves. Tests that must hold on every
//! route (credentials are never echoed, every route authenticates) iterate
//! over it, so a task that adds a route adds it there and those tests cover
//! it automatically.

#![allow(dead_code)]

pub mod xorb;

use std::cell::Cell;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use axum::body::Bytes;
use openxet_auth::{Authenticator, Mode, StaticToken, TokenSigner, UrlSigner, generate_keypair};
use openxet_cas::{AppState, CasConfig, Clock, SystemClock, router};
use openxet_index::{Index, SqliteIndex, Visibility};
use openxet_protocol::VerifyLimits;
use openxet_storage::{Bucket, Key, LocalBucket, StorageError};
use reqwest::Method;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::oneshot;
use tracing::Dispatch;
use tracing::dispatcher::{self, DefaultGuard};
use url::Url;

/// The scope of [`TestServer::write_token`] and [`TestServer::read_token`].
pub const SCOPE_A: &str = "tenant-a/repo";
/// The scope of [`TestServer::other_scope_token`].
pub const SCOPE_B: &str = "tenant-b/repo";

/// What a route requires of its caller's credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A read or write credential.
    Read,
    /// A write credential.
    Write,
}

/// One route the CAS serves, with a concrete path to request it at.
#[derive(Debug, Clone)]
pub struct Route {
    /// The HTTP method.
    pub method: Method,
    /// A concrete path, with any hash filled in.
    pub path: &'static str,
    /// The credential the route requires.
    pub access: Access,
    /// The status an authorized request with an empty body answers on a
    /// fresh server.
    pub authorized: u16,
}

/// Every route the CAS serves. Extend it whenever a route is added.
pub const ROUTES: &[Route] = &[
    Route {
        method: Method::POST,
        path: "/v1/telemetry",
        access: Access::Read,
        authorized: 204,
    },
    Route {
        method: Method::POST,
        path: XORB_PATH,
        access: Access::Write,
        // An empty body is a xorb without chunks.
        authorized: 400,
    },
    Route {
        method: Method::HEAD,
        path: XORB_PATH,
        access: Access::Read,
        authorized: 404,
    },
];

/// A xorb route's path, for a xorb no test stores.
pub const XORB_PATH: &str =
    "/v1/xorbs/default/0000000000000000000000000000000000000000000000000000000000000000";

/// A xorb path under a prefix other than `default`, the only one served
/// (S10 "Conventions for every route").
pub const OTHER_PREFIX_XORB_PATH: &str =
    "/v1/xorbs/other/0000000000000000000000000000000000000000000000000000000000000000";

/// Paths no route serves, each of which must answer `404` with the error
/// body. Some are deliberate: stock clients treat `404` as "not here" and
/// fall back (S10 "Status codes and retries"; ruling M1-R6).
pub const UNSERVED: &[(Method, &str)] = &[
    (Method::GET, "/no/such/route"),
    (Method::POST, XORB_GRANT_PATH),
    (Method::GET, FILE_CHUNK_HASHES_PATH),
    (Method::POST, OTHER_PREFIX_XORB_PATH),
];

/// The xorb-grant route of `hf_xet` 1.8 pre-releases. Its `404` makes the
/// client fall back to `POST /v1/xorbs` (S10 "Status codes and retries").
pub const XORB_GRANT_PATH: &str =
    "/v1/xorb-grants/default/0000000000000000000000000000000000000000000000000000000000000000";

/// `GET /v2/file-chunk-hashes/{h}`, which comes in M6 (ruling M1-R6).
pub const FILE_CHUNK_HASHES_PATH: &str =
    "/v2/file-chunk-hashes/0000000000000000000000000000000000000000000000000000000000000000";

/// A configuration with `visibility` and default verification limits.
pub fn config(visibility: Visibility) -> CasConfig {
    CasConfig {
        public_url: Url::parse("http://replaced.invalid").unwrap(),
        visibility,
        verify_limits: VerifyLimits::default(),
    }
}

/// The `Authorization` value for `token`. The scheme is a constant so the
/// repository's secret scanner, which flags any literal bearer value, can
/// tell test tokens from leaks.
pub fn bearer(token: &str) -> String {
    const SCHEME: &str = "Bearer";
    format!("{SCHEME} {token}")
}

/// A clock tests set by hand. Starts at the system time.
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(SystemClock.now())))
    }

    /// Moves the clock forward by `secs` seconds.
    pub fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Every log line the server wrote, at every level.
#[derive(Debug, Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

thread_local! {
    /// The log capture of a runtime thread, held for the thread's lifetime.
    static THREAD_CAPTURE: Cell<Option<DefaultGuard>> = const { Cell::new(None) };
}

fn capture_on_this_thread(dispatch: &Dispatch) {
    THREAD_CAPTURE.set(Some(dispatcher::set_default(dispatch)));
}

/// A running CAS server. Dropping it shuts the server down.
pub struct TestServer {
    /// The server's origin, without a trailing slash, as clients configure
    /// it (S10 "Conventions for every route").
    pub base_url: String,
    /// A static write token for [`SCOPE_A`].
    pub write_token: String,
    /// A static read token for [`SCOPE_A`].
    pub read_token: String,
    /// A static write token for [`SCOPE_B`].
    pub other_scope_token: String,
    /// Signs CAS tokens (`EdDSA` JWTs) the server accepts.
    pub signer: TokenSigner,
    /// The clock the server checks credentials against.
    pub clock: ManualClock,
    /// A client for requests to the server.
    pub http: reqwest::Client,
    /// The server's runtime, for running code where the server's code runs.
    pub runtime: tokio::runtime::Handle,
    /// The server's index. Call it through [`TestServer::on_server`].
    pub index: Arc<dyn Index>,
    /// The server's bucket. Call it through [`TestServer::on_server`].
    pub bucket: Arc<dyn Bucket>,
    logs: Logs,
    shutdown: Option<oneshot::Sender<()>>,
    dir: TempDir,
}

impl TestServer {
    /// Starts a server with `cfg`. Its `public_url` is replaced by the bound
    /// address, since the port is known only once bound.
    ///
    /// The server runs on its own thread and single-threaded runtime. The
    /// log capture is installed on that thread and on every thread the
    /// runtime starts, including `spawn_blocking` threads, and on no other
    /// thread, so tests running in parallel never see each other's logs.
    pub async fn start(cfg: CasConfig) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bucket = Arc::new(LocalBucket::open(&dir.path().join(BUCKET_DIR)).unwrap());
        Self::start_in(dir, cfg, bucket).await
    }

    /// Starts a server with `cfg` over `bucket` instead of a local one, for
    /// tests that need a bucket that fails.
    pub async fn start_with(cfg: CasConfig, bucket: Arc<dyn Bucket>) -> Self {
        Self::start_in(tempfile::tempdir().unwrap(), cfg, bucket).await
    }

    async fn start_in(dir: TempDir, cfg: CasConfig, bucket: Arc<dyn Bucket>) -> Self {
        let write_token = new_token();
        let read_token = new_token();
        let other_scope_token = new_token();
        let keys = generate_keypair().unwrap();
        let signer = TokenSigner::from_pem(keys.private_pem()).unwrap();
        let auth = Authenticator::new(
            vec![
                static_token(&write_token, SCOPE_A, Mode::Write),
                static_token(&read_token, SCOPE_A, Mode::Read),
                static_token(&other_scope_token, SCOPE_B, Mode::Write),
            ],
            vec![signer.public_key()],
            Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        )
        .unwrap();
        let urls = UrlSigner::new([7; 32], UrlSigner::DEFAULT_LIFETIME).unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let cfg = CasConfig {
            public_url: Url::parse(&base_url).unwrap(),
            ..cfg
        };

        let clock = ManualClock::new();
        let logs = Logs::default();
        let (shutdown, stop) = oneshot::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let root = dir.path().to_owned();
        let server_bucket = Arc::clone(&bucket);
        let server_clock = clock.clone();
        let server_logs = logs.clone();
        std::thread::spawn(move || {
            let dispatch = Dispatch::new(
                tracing_subscriber::fmt()
                    .with_max_level(tracing::Level::TRACE)
                    .with_ansi(false)
                    .with_writer(move || server_logs.clone())
                    .finish(),
            );
            let _guard = dispatcher::set_default(&dispatch);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .on_thread_start(move || capture_on_this_thread(&dispatch))
                .on_thread_stop(|| THREAD_CAPTURE.set(None))
                .build()
                .unwrap();
            runtime.block_on(async move {
                let index: Arc<dyn Index> =
                    Arc::new(SqliteIndex::open(&root.join("index.sqlite")).await.unwrap());
                let state = AppState::new(
                    Arc::clone(&index),
                    server_bucket,
                    Arc::new(auth),
                    Arc::new(urls),
                    cfg,
                )
                .with_clock(Arc::new(server_clock));
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                ready_tx
                    .send((tokio::runtime::Handle::current(), index))
                    .unwrap();
                axum::serve(listener, router(state))
                    .with_graceful_shutdown(async {
                        let _ = stop.await;
                    })
                    .await
                    .unwrap();
            });
        });
        let (runtime, index) =
            tokio::task::spawn_blocking(move || ready_rx.recv_timeout(Duration::from_secs(30)))
                .await
                .unwrap()
                .expect("the test server did not start");

        Self {
            base_url,
            write_token,
            read_token,
            other_scope_token,
            signer,
            clock,
            http: reqwest::Client::new(),
            runtime,
            index,
            bucket,
            logs,
            shutdown: Some(shutdown),
            dir,
        }
    }

    /// A server with global visibility and default verification limits.
    pub async fn start_default() -> Self {
        Self::start(config(Visibility::Global)).await
    }

    /// Runs `work` on the server's runtime, where the index's connections
    /// live, and returns its output.
    pub async fn on_server<T: Send + 'static>(
        &self,
        work: impl Future<Output = T> + Send + 'static,
    ) -> T {
        self.runtime.spawn(work).await.unwrap()
    }

    /// Every file in the local bucket's directory, temporary files included,
    /// relative to it. Empty for a server started with another bucket.
    pub fn bucket_files(&self) -> Vec<PathBuf> {
        let root = self.dir.path().join(BUCKET_DIR);
        let mut files = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    files.push(path.strip_prefix(&root).unwrap().to_owned());
                }
            }
        }
        files
    }

    /// The URL of `path` on this server.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// A request to `path` on this server.
    pub fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(method, self.url(path))
    }

    /// Sends `head` (a request line and headers, without the blank line)
    /// and then `body` over a fresh connection, half-closing it after the
    /// body when `close_write`, and returns everything the server answers.
    /// For requests no HTTP client will send, such as a body shorter than
    /// its `Content-Length`.
    pub async fn raw(&self, head: &str, body: &[u8], close_write: bool) -> String {
        let addr = self.base_url.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!("{head}\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        if close_write {
            stream.shutdown().await.unwrap();
        }
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut answer))
            .await
            .expect("the server did not answer within 10 s")
            .unwrap();
        String::from_utf8_lossy(&answer).into_owned()
    }

    /// Everything the server has logged so far.
    pub fn logs(&self) -> String {
        String::from_utf8_lossy(&self.logs.0.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// The local bucket's directory inside the server's temporary directory.
const BUCKET_DIR: &str = "bucket";

fn new_token() -> String {
    format!("oxt_test_{}", uuid::Uuid::new_v4().simple())
}

fn static_token(token: &str, scope: &str, mode: Mode) -> StaticToken {
    StaticToken {
        sha256: StaticToken::digest(token),
        scope: scope.to_owned(),
        mode,
        subject: format!("test-{scope}-{mode:?}"),
    }
}

/// A response's status, headers and body, read in full.
#[derive(Debug)]
pub struct Reply {
    /// The status code.
    pub status: reqwest::StatusCode,
    /// The headers.
    pub headers: reqwest::header::HeaderMap,
    /// The body.
    pub body: String,
    /// Whether the request was a `HEAD`, whose answer has no body.
    pub head: bool,
}

impl Reply {
    /// Sends `request` and reads the whole response.
    pub async fn of(request: reqwest::RequestBuilder) -> Self {
        let (client, request) = request.build_split();
        let request = request.unwrap();
        let head = request.method() == Method::HEAD;
        let response = client.execute(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.unwrap();
        Self {
            status,
            headers,
            body,
            head,
        }
    }

    /// The body as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("body is not JSON ({e}): {}", self.body))
    }

    /// Asserts this is an `openxet.error.v1` body with `status` and `code`.
    /// A `HEAD` answer carries the error's headers but no body (RFC 9110
    /// §9.3.2), so only those are checked.
    pub fn assert_error(&self, status: u16, code: &str) {
        assert_eq!(self.status.as_u16(), status, "body: {}", self.body);
        assert_eq!(
            self.headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
        );
        if self.head {
            assert!(self.body.is_empty(), "a HEAD answer has a body");
            return;
        }
        let json = self.json();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), 4, "unexpected keys: {keys:?}");
        assert_eq!(json["schema"], "openxet.error.v1");
        assert_eq!(json["code"], code, "body: {}", self.body);
        assert!(!json["message"].as_str().unwrap().is_empty());
        assert!(!json["hint"].as_str().unwrap().is_empty());
    }
}

/// A bucket whose every write fails as a full disk would, and which holds
/// nothing.
#[derive(Debug, Default)]
pub struct FailingBucket;

#[async_trait::async_trait]
impl Bucket for FailingBucket {
    async fn put(&self, _: &Key, _: Bytes) -> Result<(), StorageError> {
        Err(StorageError::Io {
            op: "write temp file",
            path: PathBuf::from("/failing-bucket"),
            source: std::io::Error::other("no space left on device"),
        })
    }

    async fn get_range(&self, key: &Key, _: u64, _: u64) -> Result<Bytes, StorageError> {
        Err(StorageError::NotFound { key: *key })
    }

    async fn size(&self, _: &Key) -> Result<Option<u64>, StorageError> {
        Ok(None)
    }
}
