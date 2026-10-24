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
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use openxet_auth::{Authenticator, Mode, StaticToken, UrlSigner};
use openxet_cas::{AppState, router, starting_router};
use openxet_index::SqliteIndex;
use openxet_storage::LocalBucket;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower::ServiceExt as _;
use url::Url;
use zeroize::Zeroizing;

use crate::config::{self, Config, LogFormat, Overrides, Problem};
use crate::error::{CliError, ExitCode};
use crate::logging;
use crate::output::{HumanDisplay, Output};
use crate::secret;

/// The dev token's scope and the `exp` to hand `hf_xet` for it: static
/// tokens never expire, and `hf_xet` requires an integer (S10 "Auth").
pub const DEV_SCOPE: &str = "default";
pub const DEV_TOKEN_EXP: u64 = 1 << 62;
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
        config.shutdown_grace,
    ));

    let (state, dev_token) = open(&config, public_url.clone()).await?;
    let _ = app.set(router(state));
    let endpoint = public_url.as_str().trim_end_matches('/').to_owned();
    tracing::info!(%endpoint, dev_mode = dev_token.is_some(), "ready");
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

/// Accepts connections until `stop`, then waits up to `grace` for open
/// connections to finish their requests. Requests go to the CAS once `app`
/// is set, and to [`starting_router`] before.
async fn accept(
    listener: TcpListener,
    app: Arc<OnceLock<Router>>,
    http: http1::Builder,
    mut stop: watch::Receiver<bool>,
    grace: Duration,
) {
    let starting = starting_router();
    let graceful = GracefulShutdown::new();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stop.wait_for(|stopped| *stopped) => break,
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
            if let Err(err) = connection.await {
                // A client that left, or a header read timeout.
                tracing::debug!(error = %err, "connection closed with an error");
            }
        });
    }
    drop(listener);
    tokio::select! {
        () = graceful.shutdown() => tracing::info!("every connection finished; exiting"),
        () = tokio::time::sleep(grace) => tracing::warn!(
            grace = ?grace,
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
    std::fs::create_dir_all(dir).map_err(|err| {
        CliError::new(
            "data_dir",
            format!(
                "could not create the data directory {}: {err}",
                dir.display()
            ),
            "Pass `--data-dir` with a writable directory, or set server.data_dir.",
        )
    })?;
    let index = SqliteIndex::open(&dir.join("index.sqlite"))
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
        CliError::new(
            "storage",
            format!("could not open the bucket in {}: {err}", dir.display()),
            "Check that the data directory is writable.",
        )
    })?;
    let key = secret::transfer_key(&config.transfer_key_file)?;
    let urls = UrlSigner::new(*key, config.url_lifetime)
        .map_err(|err| CliError::new("config", err.to_string(), "Fix cas.url_lifetime."))?;
    let (auth, dev_token) = if let Some(auth) = &config.auth {
        (Arc::clone(auth), None)
    } else {
        let token = secret::dev_token(&dir.join("dev-token"))?;
        let dev = StaticToken {
            sha256: StaticToken::digest(&token),
            scope: DEV_SCOPE.to_owned(),
            mode: Mode::Write,
            subject: DEV_SUBJECT.to_owned(),
        };
        let auth = Authenticator::new(vec![dev], vec![], config.max_token_lifetime)
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
             \nUpload with hf_xet:\
             \n  python -c 'import hf_xet; print(hf_xet.upload_files([\"model.bin\"], \"{endpoint}\", (\"{token}\", 2**62), None, None, None))'\n\
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
        data_dir: config.data_dir.display().to_string(),
        scope: dev_token.map(|_| DEV_SCOPE),
        mode: dev_token.map(|_| "write"),
        token: dev_token,
        token_exp: dev_token.map(|_| DEV_TOKEN_EXP),
    };
    if let Err(err) = output.emit(&ready).and_then(|()| io::stdout().flush()) {
        tracing::warn!(error = %err, "could not print the ready line on stdout");
    }
}
