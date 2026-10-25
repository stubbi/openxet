//! `openxet serve`: the CAS over HTTP/1.1, configured by
//! [`crate::config`].
//!
//! Startup binds first, so a taken port fails at once, then serves
//! `/healthz` (and `503 not_ready` elsewhere) while the index and the
//! bucket open, then serves the CAS and announces the endpoint on stdout.
//! `SIGTERM` or `SIGINT` stops accepting, lets in-flight requests finish
//! within `server.shutdown_grace`, and exits 0.

use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use openxet_auth::{
    Action, Authenticator, CasGrant, Glob, Mode, Rule, RulesAuthorizer, StaticToken, UrlSigner,
    UserProfile,
};
use openxet_cas::{AppState, starting_router};
use openxet_client::STATIC_TOKEN_EXP;
use openxet_http::compose;
use openxet_index::SqliteIndex;
use openxet_storage::{LocalBucket, StorageError, create_private_dir_all};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tower::ServiceExt as _;
use url::Url;
use zeroize::Zeroizing;

use crate::config::{self, Config, LogFormat, Overrides, Problem};
use crate::error::{CliError, ExitCode};
use crate::logging;
use crate::output::{HumanDisplay, Output};
use crate::{preflight, secret};

/// The dev token's scope. Its `exp` for `hf_xet` is [`STATIC_TOKEN_EXP`]:
/// static tokens never expire, and `hf_xet` requires an integer (S10
/// "Auth").
pub const DEV_SCOPE: &str = "default";
const DEV_SUBJECT: &str = "dev";

/// How long blocking work (an index or bucket call) may run on after the
/// last connection closed.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(5);

/// `openxet serve`'s options.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// TOML configuration file; without one, or without an `[auth]`
    /// section, the server runs in dev mode
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Directory for the index, the bucket and the keys (`server.data_dir`)
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Port to listen on, replacing the port of `server.bind`; 0 picks a free one
    #[arg(long, value_name = "N")]
    port: Option<u16>,
    /// Log line format on stderr (`server.log_format`)
    #[arg(long, value_name = "FORMAT")]
    log_format: Option<LogFormat>,
}

impl CliError {
    /// The configuration has `problems`; `config check` explains them.
    pub fn config(problems: &[Problem]) -> Self {
        let list: Vec<String> = problems
            .iter()
            .map(|p| format!("{} {}", p.key, p.rule))
            .collect();
        Self::new(
            "config",
            format!(
                "the configuration has {} problem(s): {}",
                problems.len(),
                list.join("; ")
            ),
            "Run `openxet config check` with the same --config and environment: it lists each \
             problem with its value, source and a fix.",
        )
    }
}

/// Runs the server until `SIGTERM` or `SIGINT`.
pub fn run(args: &ServeArgs, output: Output) -> Result<(), CliError> {
    let overrides = Overrides {
        port: args.port,
        data_dir: args.data_dir.clone(),
        log_format: args.log_format,
    };
    let loaded = config::load(args.config.as_deref(), std::env::vars_os(), &overrides);
    let config = loaded
        .config
        .ok_or_else(|| CliError::config(&loaded.problems))?;
    logging::init(config.log_format, config.log_level);
    if let Some(warning) = config::memory_budget_warning(config.memory_budget, config.host_memory) {
        tracing::warn!(
            "{warning}. Lower server.memory_budget to at most half, or leave it unset for a \
             quarter."
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            CliError::new(
                "runtime",
                format!("could not start the async runtime: {err}"),
                "Check the process's thread and file-descriptor limits.",
            )
        })?;
    let result = runtime.block_on(serve(config, output));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    result
}

async fn serve(config: Config, output: Output) -> Result<(), CliError> {
    let stop = stop_on_signal()?;
    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|err| bind_error(config.bind, &err))?;
    let addr = listener
        .local_addr()
        .map_err(|err| bind_error(config.bind, &err))?;
    let public_url = match &config.public_url {
        Some(url) => url.clone(),
        None => Url::parse(&format!("http://{addr}")).map_err(|err| {
            CliError::new(
                "bind",
                format!("the bound address {addr} is not a URL host: {err}"),
                "Set server.public_url.",
            )
        })?,
    };

    let app = Arc::new(OnceLock::new());
    let mut http = http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(config.header_read_timeout);
    let server = tokio::spawn(accept(
        listener,
        Arc::clone(&app),
        http,
        stop.clone(),
        Limits {
            connections: config.max_connections as usize,
            grace: config.shutdown_grace,
        },
    ));

    let (state, dev_token) = open(&config, public_url.clone()).await?;
    let authorizer = authorizer(&config);
    let body_deadline = state.cfg.body_deadline;
    let _ = app.set(compose(vec![openxet_cas::mounted(state)], body_deadline));
    let endpoint = public_url.as_str().trim_end_matches('/').to_owned();
    tracing::info!(
        %endpoint,
        dev_mode = dev_token.is_some(),
        rules = authorizer.rules().len(),
        "ready"
    );
    if !*stop.borrow() {
        announce(
            output,
            &config,
            &endpoint,
            dev_token.as_ref().map(|t| t.as_str()),
        );
    }
    server.await.map_err(|err| {
        CliError::new(
            "internal",
            format!("the server task failed: {err}"),
            "This is a bug; please report it with the log.",
        )
    })
}

/// A channel that turns `true` on the first `SIGTERM` or `SIGINT`. A
/// second signal exits at once with code 1, for a person who doesn't want
/// to wait out the grace period; acknowledged writes are already durable.
/// The handlers are installed before the port is bound, so a signal is
/// never missed.
fn stop_on_signal() -> Result<watch::Receiver<bool>, CliError> {
    let signal_error = |err: io::Error| {
        CliError::new(
            "signal",
            format!("could not install the shutdown signal handlers: {err}"),
            "Check that the process may handle SIGTERM and SIGINT.",
        )
    };
    let (tx, rx) = watch::channel(false);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).map_err(signal_error)?;
        let mut int = signal(SignalKind::interrupt()).map_err(signal_error)?;
        tokio::spawn(async move {
            let name = tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = int.recv() => "SIGINT",
            };
            tracing::info!(
                signal = name,
                "shutting down once in-flight requests finish; signal again to exit now"
            );
            let _ = tx.send(true);
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            tracing::warn!("second signal: exiting without waiting for in-flight requests");
            std::process::exit(i32::from(ExitCode::FAILURE));
        });
    }
    #[cfg(not(unix))]
    {
        let _ = signal_error;
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!(signal = "ctrl-c", "shutting down");
                let _ = tx.send(true);
            }
        });
    }
    Ok(rx)
}

fn bind_error(addr: SocketAddr, err: &io::Error) -> CliError {
    let port = addr.port();
    match err.kind() {
        io::ErrorKind::AddrInUse => CliError::new(
            "address_in_use",
            format!("could not listen on {addr}: the address is already in use"),
            format!(
                "Another process is listening on port {port}. Stop it, or pass `--port <N>` to \
                 listen elsewhere (`--port 0` picks a free port)."
            ),
        ),
        io::ErrorKind::PermissionDenied => CliError::new(
            "bind",
            format!("could not listen on {addr}: permission denied"),
            "Ports below 1024 need privileges; pass `--port 8080` or another port above 1023.",
        ),
        _ => CliError::new(
            "bind",
            format!("could not listen on {addr}: {err}"),
            "Set server.bind to an address of this host, such as 127.0.0.1:8080, or pass \
             `--port <N>`.",
        ),
    }
}

/// The bounds of [`accept`].
struct Limits {
    /// `server.max_connections`.
    connections: usize,
    /// `server.shutdown_grace`.
    grace: Duration,
}

/// Accepts connections until `stop`, then waits up to `limits.grace` for
/// open connections to finish their requests. Requests go to the CAS once
/// `app` is set, and to [`starting_router`] before.
///
/// At most `limits.connections` are open at once (ruling M1-R40). Past
/// that, the next is not accepted until one closes: it waits in the
/// kernel's listen backlog, its handshake done and its request unread, and
/// is served then. Accepting it only to refuse it would cost the file
/// descriptor the cap protects, and an answer sent with its request unread
/// reaches the client as a reset. Stock clients wait on a connection for
/// their 300 s read timeout and retry a connect error (S10 "Status codes
/// and retries").
async fn accept(
    listener: TcpListener,
    app: Arc<OnceLock<Router>>,
    http: http1::Builder,
    mut stop: watch::Receiver<bool>,
    limits: Limits,
) {
    let starting = starting_router();
    let graceful = GracefulShutdown::new();
    let slots = Arc::new(Semaphore::new(limits.connections));
    loop {
        let next = async {
            let slot = Arc::clone(&slots).acquire_owned().await;
            (slot, listener.accept().await)
        };
        let (slot, accepted) = tokio::select! {
            next = next => next,
            stopped = stop.wait_for(|stopped| *stopped) => {
                // An error means the signal task is gone: the runtime is
                // shutting down after a failed startup, and has nothing to
                // drain or report.
                if stopped.is_err() {
                    return;
                }
                break;
            }
        };
        let Ok(slot) = slot else {
            unreachable!("the connection slots are never closed");
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(err) => {
                // Out of file descriptors, typically: back off rather than
                // spin.
                tracing::warn!(error = %err, "could not accept a connection");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = Arc::clone(&app);
        let starting = starting.clone();
        let service = hyper::service::service_fn(move |request: hyper::Request<_>| {
            let routes = app.get().unwrap_or(&starting).clone();
            routes.oneshot(request.map(Body::new))
        });
        let connection = graceful.watch(http.serve_connection(TokioIo::new(stream), service));
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(err) = connection.await {
                // A client that left, or a header read timeout.
                tracing::debug!(error = %err, "connection closed with an error");
            }
        });
    }
    drop(listener);
    tokio::select! {
        () = graceful.shutdown() => tracing::info!("every connection finished; exiting"),
        () = tokio::time::sleep(limits.grace) => tracing::warn!(
            grace = ?limits.grace,
            "the shutdown grace period ended with connections still open; closing them"
        ),
    }
}

/// Opens the data directory: the index, the bucket, the transfer key and,
/// in dev mode, the dev token. Returns the state and the dev token.
async fn open(
    config: &Config,
    public_url: Url,
) -> Result<(AppState, Option<Zeroizing<String>>), CliError> {
    let dir = &config.data_dir;
    prepare_data_dir(dir)?;
    let index = SqliteIndex::open(&dir.join(INDEX_FILE))
        .await
        .map_err(|err| {
            CliError::new(
                "index",
                format!("could not open the index in {}: {err}", dir.display()),
                "Check that the data directory is writable and that no other openxet process \
                 uses it with an incompatible version. The index can be rebuilt from the bucket.",
            )
        })?;
    let bucket = LocalBucket::open(&dir.join("bucket")).map_err(|err| {
        let hint = if matches!(err, StorageError::NoHardLinks { .. }) {
            "Move the data directory to a filesystem with hard links, such as ext4, XFS, APFS \
             or NTFS (--data-dir or server.data_dir)."
        } else {
            "Check that the data directory is writable."
        };
        CliError::new(
            "storage",
            format!("could not open the bucket in {}: {err}", dir.display()),
            hint,
        )
    })?;
    let key = transfer_key(config)?;
    let urls = UrlSigner::new(*key, config.url_lifetime)
        .map_err(|err| CliError::new("config", err.to_string(), "Fix cas.url_lifetime."))?;
    let (auth, dev_token) = if let Some(auth) = &config.auth {
        (Arc::clone(auth), None)
    } else {
        let token = secret::dev_token(&dir.join("dev-token"))?;
        let auth = Authenticator::new(
            vec![dev_static_token(&token)],
            vec![],
            config.max_token_lifetime,
        )
        .map_err(|err| CliError::new("config", err.to_string(), "Fix [auth]."))?;
        (Arc::new(auth), Some(token))
    };
    let state = AppState::new(
        Arc::new(index),
        Arc::new(bucket),
        auth,
        Arc::new(urls),
        config.cas(public_url),
    );
    Ok((state, dev_token))
}

/// The dev token's entry: a write token for [`DEV_SCOPE`], as in M1, and a
/// user credential for subject `dev`, whom [`dev_rule`] lets do everything
/// (ruling M2-R3).
fn dev_static_token(token: &str) -> StaticToken {
    StaticToken {
        sha256: StaticToken::digest(token),
        subject: DEV_SUBJECT.to_owned(),
        cas: Some(CasGrant {
            scope: DEV_SCOPE.to_owned(),
            mode: Mode::Write,
            tenant: None,
        }),
        user: Some(UserProfile::default()),
    }
}

/// Dev mode's one rule (ruling M2-R3): `subjects = ["dev"], repos = ["*"],
/// actions = ["read", "write", "create"]`.
fn dev_rule() -> Rule {
    Rule {
        subjects: vec![DEV_SUBJECT.to_owned()],
        claims: std::collections::BTreeMap::new(),
        repos: vec![Glob::any()],
        actions: vec![Action::Read, Action::Write, Action::Create],
    }
}

/// The authorizer for user credentials: `[[auth.rules]]`, or in dev mode
/// (no `[auth]`) [`dev_rule`].
fn authorizer(config: &Config) -> RulesAuthorizer {
    if config.auth.is_some() {
        RulesAuthorizer::new(config.rules.clone())
    } else {
        RulesAuthorizer::new(vec![dev_rule()])
    }
}

const INDEX_FILE: &str = "index.sqlite";

/// Creates the data directory mode `0700` if it is missing, and the index
/// file mode `0600` (SQLite gives its `-wal` and `-shm` files the database
/// file's mode). Warns if an existing directory is open to other users:
/// the bucket holds every scope's data, readable there without a
/// credential.
fn prepare_data_dir(dir: &Path) -> Result<(), CliError> {
    let unwritable = |err: io::Error| {
        CliError::new(
            "data_dir",
            format!(
                "could not create the data directory {}: {err}",
                dir.display()
            ),
            "Pass `--data-dir` with a writable directory, or set server.data_dir.",
        )
    };
    if let Some(err) = preflight::dangling_symlink(dir) {
        return Err(err);
    }
    if dir.is_dir() {
        if let Some(warning) = preflight::shared_data_dir(dir) {
            let fix = preflight::shared_data_dir_fix(dir);
            tracing::warn!("{warning}. {fix}");
        }
        secret::sweep_stale_temps(dir);
    } else {
        create_private_dir_all(dir).map_err(unwritable)?;
    }
    match secret::create_new_file(&dir.join(INDEX_FILE), 0o600) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(unwritable(err)),
    }
}

/// The transfer key. A named `cas.transfer_key_file` must exist; the
/// default one is created, with a warning outside dev mode, since every
/// node behind one endpoint needs the same key (ruling M1-R29).
fn transfer_key(config: &Config) -> Result<Zeroizing<[u8; 32]>, CliError> {
    let path = &config.transfer_key_file;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        secret::sweep_stale_temps(parent);
    }
    if let Some(key) = secret::read_transfer_key(path)? {
        return Ok(key);
    }
    if config.transfer_key_explicit {
        return Err(preflight::missing_transfer_key(path));
    }
    let (key, created) = secret::transfer_key(path)?;
    if created && config.auth.is_some() {
        tracing::warn!(
            path = %path.display(),
            "generated a new transfer key; every node serving this deployment must share this \
             file, or transfer URLs signed by one node are refused by the others"
        );
    }
    Ok(key)
}

/// The `openxet.serve.v1` line. The token fields are set in dev mode only.
#[derive(Serialize)]
struct Ready<'a> {
    schema: &'static str,
    version: &'static str,
    endpoint: &'a str,
    data_dir: String,
    scope: Option<&'static str>,
    mode: Option<&'static str>,
    token: Option<&'a str>,
    token_exp: Option<u64>,
}

impl HumanDisplay for Ready<'_> {
    fn human(&self) -> String {
        let Some(token) = self.token else {
            return format!(
                "openxet {} is serving the Xet CAS at {} (data dir {}).\nStop with Ctrl-C.",
                self.version, self.endpoint, self.data_dir
            );
        };
        let endpoint = self.endpoint;
        let data_dir = &self.data_dir;
        format!(
            "openxet {version} is serving the Xet CAS in dev mode.\n\
             \n  endpoint  {endpoint}\
             \n  data dir  {data_dir}\
             \n  token     {token}\
             \n            scope \"{DEV_SCOPE}\" (write), kept in {data_dir}/dev-token\n\
             \nUpload with hf_xet (once: pip install hf_xet):\
             \n  python3 -c 'import hf_xet; c = hf_xet.XetSession().new_upload_commit(\
             endpoint=\"{endpoint}\", token=\"{token}\", token_expiry_unix_secs=2**62); \
             u = c.start_upload_file(\"model.bin\"); c.wait_to_finish(); \
             print(u.result().xet_info.hash)'\n\
             \nUpload with openxet:\
             \n  export OPENXET_ENDPOINT={endpoint} OPENXET_TOKEN={token}\
             \n  openxet upload model.bin\n\
             \nDev mode accepts only this token. To configure credentials, see \
             docs/reference/config.md.\
             \nStop with Ctrl-C.",
            version = self.version,
        )
    }
}

/// Prints the ready line, or the banner, on stdout: the one place a
/// credential is printed (design §7). A closed stdout is logged and the
/// server keeps running.
fn announce(output: Output, config: &Config, endpoint: &str, dev_token: Option<&str>) {
    let ready = Ready {
        schema: "openxet.serve.v1",
        version: env!("CARGO_PKG_VERSION"),
        endpoint,
        data_dir: std::path::absolute(&config.data_dir)
            .unwrap_or_else(|_| config.data_dir.clone())
            .display()
            .to_string(),
        scope: dev_token.map(|_| DEV_SCOPE),
        mode: dev_token.map(|_| "write"),
        token: dev_token,
        token_exp: dev_token.map(|_| STATIC_TOKEN_EXP),
    };
    if let Err(err) = output.emit(&ready).and_then(|()| io::stdout().flush()) {
        tracing::warn!(error = %err, "could not print the ready line on stdout");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openxet_auth::{Authorizer as _, Credential, Target};

    use super::*;

    /// The dev token keeps M1's write scope `default` and is also the user
    /// `dev`, whom dev mode's rule lets read, write and create everywhere
    /// (ruling M2-R3).
    #[tokio::test]
    async fn dev_token_is_both() {
        let token = "oxt_dev_unit";
        let auth = Authenticator::new(
            vec![dev_static_token(token)],
            vec![],
            Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        )
        .unwrap();
        let Credential::Both(principal, user) = auth.authenticate(token, 0).unwrap() else {
            panic!("the dev token must be both credentials");
        };
        assert_eq!(
            (
                principal.scope.as_str(),
                principal.mode,
                principal.subject.as_str()
            ),
            (DEV_SCOPE, Mode::Write, DEV_SUBJECT)
        );
        assert_eq!((user.subject.as_str(), user.name.as_str()), ("dev", "dev"));

        let config = crate::config::load(None, Vec::new(), &crate::config::Overrides::default())
            .config
            .unwrap();
        let authz = authorizer(&config);
        assert_eq!(authz.rules(), [dev_rule()]);
        for (action, target) in [
            (
                Action::Read,
                Target {
                    namespace: "any",
                    name: Some("repo"),
                },
            ),
            (
                Action::Write,
                Target {
                    namespace: "any",
                    name: Some("repo"),
                },
            ),
            (
                Action::Create,
                Target {
                    namespace: "any",
                    name: None,
                },
            ),
        ] {
            assert!(
                authz.allows(&user, action, target).await.unwrap(),
                "{action:?}"
            );
        }
        let stranger = openxet_auth::UserPrincipal::anonymous();
        assert!(
            !authz
                .allows(
                    &stranger,
                    Action::Read,
                    Target {
                        namespace: "a",
                        name: Some("b")
                    }
                )
                .await
                .unwrap()
        );
    }
}
