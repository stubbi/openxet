//! The server's configuration: a TOML file, `OPENXET_<SECTION>__<KEY>`
//! environment variables over it, and command-line flags over both
//! (`docs/reference/config.md`).
//!
//! Loading never stops at the first mistake: every key is checked and every
//! problem is reported with its key path, value, source, the rule broken and
//! a hint, so `openxet config check` can explain a whole file at once.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openxet_auth::{AuthError, Authenticator, Mode, PublicKey, StaticToken, UrlSigner};
use openxet_cas::{BodyDeadline, CasConfig};
use openxet_index::Visibility;
use serde::Serialize;
use url::Url;

use crate::duration;

/// Where a setting's value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    File(PathBuf),
    Env(String),
    Flag(&'static str),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::File(path) => write!(f, "file {}", path.display()),
            Self::Env(name) => write!(f, "env {name}"),
            Self::Flag(flag) => write!(f, "flag {flag}"),
        }
    }
}

/// A configuration mistake.
#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    /// The key path, such as `cas.url_lifetime` or `auth.static_tokens[0].sha256`.
    pub key: String,
    /// The rejected value as written, if there is one to show.
    pub value: Option<String>,
    /// Where the value came from.
    pub source: String,
    /// The rule the value breaks.
    pub rule: String,
    /// What to do about it.
    pub hint: String,
}

/// One effective setting, for `openxet config check`.
#[derive(Debug, Clone, Serialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub source: String,
}

/// Log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

/// Command-line flags that override the configuration.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub port: Option<u16>,
    pub data_dir: Option<PathBuf>,
    pub log_format: Option<LogFormat>,
}

/// A valid configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// The URL clients reach the server at; `None` means `http://` and the
    /// bound address.
    pub public_url: Option<Url>,
    pub data_dir: PathBuf,
    pub log_format: LogFormat,
    pub log_level: tracing::Level,
    pub header_read_timeout: Duration,
    pub body_deadline: BodyDeadline,
    pub shutdown_grace: Duration,
    pub visibility: Visibility,
    pub url_lifetime: Duration,
    pub heartbeat_interval: Duration,
    pub transfer_key_file: PathBuf,
    /// The configured credentials, or `None` in dev mode (no `[auth]`).
    pub auth: Option<Arc<Authenticator>>,
    pub max_token_lifetime: Duration,
}

impl Config {
    /// The CAS configuration for a server reached at `public_url`.
    pub fn cas(&self, public_url: Url) -> CasConfig {
        CasConfig {
            public_url,
            visibility: self.visibility,
            verify_limits: openxet_protocol::VerifyLimits::default(),
            heartbeat_interval: self.heartbeat_interval,
            body_deadline: self.body_deadline,
        }
    }
}

/// The outcome of loading: every effective setting and every problem, and
/// the configuration if there were no problems.
#[derive(Debug)]
pub struct Loaded {
    pub file: Option<PathBuf>,
    pub settings: Vec<Setting>,
    pub problems: Vec<Problem>,
    pub config: Option<Config>,
}

/// The prefix of configuration environment variables.
pub const ENV_PREFIX: &str = "OPENXET_";

/// What a key holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    StrList,
    Tokens,
}

/// Every key, by section. Keep in step with `docs/reference/config.md`.
const KEYS: &[(&str, &[(&str, Kind)])] = &[
    (
        "server",
        &[
            ("bind", Kind::Str),
            ("public_url", Kind::Str),
            ("data_dir", Kind::Str),
            ("log_format", Kind::Str),
            ("log_level", Kind::Str),
            ("header_read_timeout", Kind::Str),
            ("body_read_timeout", Kind::Str),
            ("body_min_rate", Kind::Int),
            ("shutdown_grace", Kind::Str),
        ],
    ),
    (
        "cas",
        &[
            ("dedup_visibility", Kind::Str),
            ("url_lifetime", Kind::Str),
            ("heartbeat_interval", Kind::Str),
            ("transfer_key_file", Kind::Str),
        ],
    ),
    (
        "auth",
        &[
            ("static_tokens", Kind::Tokens),
            ("jwt_public_keys", Kind::StrList),
            ("max_token_lifetime", Kind::Str),
        ],
    ),
];

const STATIC_TOKEN_FIELDS: &[&str] = &["sha256", "scope", "mode", "subject"];

fn kind_of(key: &str) -> Option<Kind> {
    let (section, name) = key.split_once('.')?;
    KEYS.iter()
        .find(|(s, _)| *s == section)?
        .1
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, kind)| *kind)
}

fn keys_of(section: &str) -> Option<String> {
    KEYS.iter().find(|(s, _)| *s == section).map(|(_, keys)| {
        keys.iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

const DOCS: &str = "See docs/reference/config.md for every key.";

/// Raw values by dotted key, before checking, and their sources.
#[derive(Default)]
struct Raw {
    values: BTreeMap<String, (toml::Value, Source)>,
    /// Whether an `[auth]` section was given at all.
    auth: bool,
    problems: Vec<Problem>,
}

impl Raw {
    fn problem(
        &mut self,
        key: impl Into<String>,
        value: Option<String>,
        source: &Source,
        rule: impl Into<String>,
        hint: impl Into<String>,
    ) {
        self.problems.push(Problem {
            key: key.into(),
            value,
            source: source.to_string(),
            rule: rule.into(),
            hint: hint.into(),
        });
    }

    fn unknown(&mut self, key: &str, source: &Source) {
        let hint = match key.split_once('.') {
            Some((section, _)) => match keys_of(section) {
                Some(keys) => format!("The keys of [{section}] are: {keys}. {DOCS}"),
                None => format!("The sections are [server], [cas] and [auth]. {DOCS}"),
            },
            None => format!("Put keys in a [server], [cas] or [auth] section. {DOCS}"),
        };
        self.problem(key, None, source, "is not a configuration key", hint);
    }

    fn file(&mut self, path: &Path) {
        let source = Source::File(path.to_owned());
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                self.problem(
                    "--config",
                    Some(path.display().to_string()),
                    &source,
                    format!("the file could not be read: {err}"),
                    "Pass the path of a readable TOML file, or leave out --config to run in dev \
                     mode.",
                );
                return;
            }
        };
        let table: toml::Table = match text.parse() {
            Ok(table) => table,
            Err(err) => {
                self.problem(
                    "--config",
                    Some(path.display().to_string()),
                    &source,
                    format!("the file is not valid TOML: {}", err.message()),
                    format!(
                        "Fix the TOML syntax{}.",
                        err.span()
                            .map(|span| format!(
                                " at line {}",
                                text[..span.start].matches('\n').count() + 1
                            ))
                            .unwrap_or_default()
                    ),
                );
                return;
            }
        };
        for (section, value) in table {
            let toml::Value::Table(keys) = value else {
                self.unknown(&section, &source);
                continue;
            };
            if section == "auth" {
                self.auth = true;
            }
            for (name, value) in keys {
                let key = format!("{section}.{name}");
                if kind_of(&key).is_some() {
                    self.values.insert(key, (value, source.clone()));
                } else {
                    self.unknown(&key, &source);
                }
            }
        }
    }

    fn env(&mut self, vars: impl IntoIterator<Item = (OsString, OsString)>) {
        let mut vars: Vec<(String, OsString)> = vars
            .into_iter()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
            .filter(|(name, _)| name.starts_with(ENV_PREFIX) && name.contains("__"))
            .collect();
        vars.sort();
        for (name, value) in vars {
            let source = Source::Env(name.clone());
            let Some((section, key)) = name[ENV_PREFIX.len()..].split_once("__") else {
                continue;
            };
            let key = format!("{}.{}", section.to_lowercase(), key.to_lowercase());
            let Some(kind) = kind_of(&key) else {
                self.unknown(&key, &source);
                continue;
            };
            let Ok(text) = value.into_string() else {
                self.problem(
                    key,
                    None,
                    &source,
                    "is not valid UTF-8",
                    "Set the variable to UTF-8 text.",
                );
                continue;
            };
            if section.eq_ignore_ascii_case("auth") {
                self.auth = true;
            }
            let value = if kind == Kind::Str {
                toml::Value::String(text)
            } else if let Ok(mut table) = format!("v = {text}").parse::<toml::Table>() {
                table.remove("v").unwrap_or(toml::Value::String(text))
            } else {
                self.problem(
                    key,
                    Some(text),
                    &source,
                    "is not a TOML value",
                    "Write the value as in the file, e.g. `4096` or `[\"a\", \"b\"]`.",
                );
                continue;
            };
            self.values.insert(key, (value, source));
        }
    }

    /// The string at `key` and its source, or the default; records a
    /// problem and returns `None` if it is not a string.
    fn string(&mut self, key: &str) -> Option<(String, Source)> {
        let (value, source) = self.values.get(key)?.clone();
        if let toml::Value::String(text) = value {
            Some((text, source))
        } else {
            self.problem(
                key,
                Some(value.to_string()),
                &source,
                "must be a string",
                format!("Quote the value, as in `{key} = \"…\"`. {DOCS}"),
            );
            None
        }
    }
}

/// Checks the value of one key, recording problems.
struct Checker<'a> {
    raw: &'a mut Raw,
    settings: Vec<Setting>,
}

impl Checker<'_> {
    fn set(&mut self, key: &str, value: impl Into<String>, source: &Source) {
        self.settings.push(Setting {
            key: key.to_owned(),
            value: value.into(),
            source: source.to_string(),
        });
    }

    /// The string at `key` parsed by `parse`, or `default` if unset.
    /// `rule` and `hint` explain a value `parse` rejects.
    fn parsed<T>(
        &mut self,
        key: &str,
        default: (T, &str),
        parse: impl FnOnce(&str) -> Option<T>,
        rule: &str,
        hint: &str,
    ) -> Option<T> {
        let Some((text, source)) = self.raw.string(key) else {
            if self.raw.values.contains_key(key) {
                return None;
            }
            self.set(key, default.1, &Source::Default);
            return Some(default.0);
        };
        if let Some(value) = parse(&text) {
            self.set(key, text, &source);
            Some(value)
        } else {
            self.raw.problem(key, Some(text), &source, rule, hint);
            None
        }
    }

    /// A duration within `min..=max`.
    fn duration(
        &mut self,
        key: &str,
        default: Duration,
        (min, max): (Duration, Duration),
        hint: &str,
    ) -> Option<Duration> {
        let shown = duration::format(default);
        let rule = format!(
            "must be {}, from {} to {} inclusive",
            duration::FORM,
            duration::format(min),
            duration::format(max)
        );
        self.parsed(
            key,
            (default, &shown),
            |text| duration::parse(text).filter(|d| (min..=max).contains(d)),
            &rule,
            hint,
        )
    }
}

const SECOND: Duration = Duration::from_secs(1);

/// Loads the configuration from `file`, then `env` (only
/// `OPENXET_<SECTION>__<KEY>` variables are read), then `overrides`.
pub fn load(
    file: Option<&Path>,
    env: impl IntoIterator<Item = (OsString, OsString)>,
    overrides: &Overrides,
) -> Loaded {
    let mut raw = Raw::default();
    if let Some(path) = file {
        raw.file(path);
    }
    raw.env(env);
    let mut c = Checker {
        raw: &mut raw,
        settings: Vec::new(),
    };
    let config = check(&mut c, overrides);
    let settings = c.settings;
    let problems = raw.problems;
    Loaded {
        file: file.map(Path::to_owned),
        settings,
        config: if problems.is_empty() { config } else { None },
        problems,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one block per key, in documentation order, reads best whole"
)]
fn check(c: &mut Checker<'_>, overrides: &Overrides) -> Option<Config> {
    let bind = c.parsed(
        "server.bind",
        (SocketAddr::from(([127, 0, 0, 1], 8080)), "127.0.0.1:8080"),
        |text| text.parse().ok(),
        "must be an IP address and port, such as 127.0.0.1:8080 or [::]:8080",
        "Use an IP address, not a host name; 0.0.0.0:8080 listens on every interface.",
    );
    let bind = bind.map(|mut bind| {
        if let Some(port) = overrides.port {
            bind.set_port(port);
            c.set("server.bind", bind.to_string(), &Source::Flag("--port"));
        }
        bind
    });

    let public_url = public_url(c, bind);

    let data_dir = match &overrides.data_dir {
        Some(dir) => {
            c.set(
                "server.data_dir",
                dir.display().to_string(),
                &Source::Flag("--data-dir"),
            );
            Some(dir.clone())
        }
        None => c.parsed(
            "server.data_dir",
            (PathBuf::from("./openxet-data"), "./openxet-data"),
            |text| (!text.is_empty()).then(|| PathBuf::from(text)),
            "must be a directory path",
            "Name the directory for the index, the bucket and the keys.",
        ),
    };

    let log_format = match overrides.log_format {
        Some(format) => {
            c.set(
                "server.log_format",
                format.name(),
                &Source::Flag("--log-format"),
            );
            Some(format)
        }
        None => c.parsed(
            "server.log_format",
            (LogFormat::Text, "text"),
            LogFormat::parse,
            "must be `text` or `json`",
            "Use `json` for log collectors and `text` for people.",
        ),
    };
    let log_level = c.parsed(
        "server.log_level",
        (tracing::Level::INFO, "info"),
        |text| match text {
            "error" => Some(tracing::Level::ERROR),
            "warn" => Some(tracing::Level::WARN),
            "info" => Some(tracing::Level::INFO),
            "debug" => Some(tracing::Level::DEBUG),
            "trace" => Some(tracing::Level::TRACE),
            _ => None,
        },
        "must be one of error, warn, info, debug or trace",
        "Use `info` in production and `debug` to see every request.",
    );
    let header_read_timeout = c.duration(
        "server.header_read_timeout",
        Duration::from_secs(30),
        (SECOND, Duration::from_mins(5)),
        "A connection whose request line and headers take longer is closed. 30s suits most \
         setups.",
    );
    let body_read_timeout = c.duration(
        "server.body_read_timeout",
        BodyDeadline::DEFAULT.idle(),
        (SECOND, Duration::from_hours(1)),
        "A request body that sends nothing for this long is refused with 408. Keep it at or \
         above hf_xet's own 300s read timeout.",
    );
    let body_min_rate = int(
        c,
        "server.body_min_rate",
        BodyDeadline::DEFAULT.min_rate(),
        1..=1024 * 1024,
        "must be a whole number of bytes per second from 1 to 1048576",
        "4096 (4 KiB/s) keeps uploads over slow links working; a higher rate frees stalled \
         connections sooner.",
    );
    let shutdown_grace = c.duration(
        "server.shutdown_grace",
        Duration::from_secs(30),
        (SECOND, Duration::from_mins(10)),
        "On SIGTERM or SIGINT, in-flight requests get this long to finish.",
    );

    let visibility = c.parsed(
        "cas.dedup_visibility",
        (Visibility::Global, "global"),
        |text| match text {
            "global" => Some(Visibility::Global),
            "per-scope" => Some(Visibility::PerScope),
            _ => None,
        },
        "must be `global` or `per-scope`",
        "`global` deduplicates across scopes; `per-scope` keeps each scope's xorbs invisible to \
         the others.",
    );
    let url_lifetime = c.duration(
        "cas.url_lifetime",
        UrlSigner::DEFAULT_LIFETIME,
        (UrlSigner::MIN_LIFETIME, UrlSigner::MAX_LIFETIME),
        "Use 60m unless you need otherwise. Shortening it on a live server invalidates \
         outstanding transfer URLs; clients recover by requesting new ones.",
    );
    let heartbeat_interval = c.duration(
        "cas.heartbeat_interval",
        CasConfig::DEFAULT_HEARTBEAT_INTERVAL,
        (SECOND, Duration::from_mins(2)),
        "hf_xet fails a shard upload stream that is silent for 300s; 5s is the default.",
    );
    let transfer_key_file = match c.raw.string("cas.transfer_key_file") {
        Some((text, source)) if !text.is_empty() => {
            c.set("cas.transfer_key_file", &text, &source);
            Some(PathBuf::from(text))
        }
        Some((_, source)) => {
            c.raw.problem(
                "cas.transfer_key_file",
                Some(String::new()),
                &source,
                "must be a file path",
                "Leave it out to keep the key in the data directory.",
            );
            None
        }
        None if c.raw.values.contains_key("cas.transfer_key_file") => None,
        None => data_dir.as_ref().map(|dir| {
            let path = dir.join("transfer.key");
            c.set(
                "cas.transfer_key_file",
                path.display().to_string(),
                &Source::Default,
            );
            path
        }),
    };

    let max_token_lifetime = c.duration(
        "auth.max_token_lifetime",
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        (
            Authenticator::MIN_TOKEN_LIFETIME,
            Authenticator::MAX_TOKEN_LIFETIME,
        ),
        "CAS tokens expiring further ahead are refused. Match it to your issuer's longest \
         token; 24h is the default.",
    );
    let auth = if c.raw.auth {
        let tokens = static_tokens(c);
        let keys = jwt_public_keys(c);
        match (tokens, keys, max_token_lifetime) {
            (Some(tokens), Some(keys), Some(max)) => authenticator(c, tokens, keys, max),
            _ => None,
        }
    } else {
        c.set(
            "auth",
            "none: dev mode, with a generated write token",
            &Source::Default,
        );
        None
    };

    Some(Config {
        bind: bind?,
        public_url: public_url?,
        data_dir: data_dir?,
        log_format: log_format?,
        log_level: log_level?,
        header_read_timeout: header_read_timeout?,
        body_deadline: BodyDeadline::new(body_read_timeout?, body_min_rate?)?,
        shutdown_grace: shutdown_grace?,
        visibility: visibility?,
        url_lifetime: url_lifetime?,
        heartbeat_interval: heartbeat_interval?,
        transfer_key_file: transfer_key_file?,
        auth: if c.raw.auth { Some(auth?) } else { None },
        max_token_lifetime: max_token_lifetime?,
    })
}

fn int(
    c: &mut Checker<'_>,
    key: &str,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
    rule: &str,
    hint: &str,
) -> Option<u32> {
    let Some((value, source)) = c.raw.values.get(key).cloned() else {
        c.set(key, default.to_string(), &Source::Default);
        return Some(default);
    };
    let parsed = value
        .as_integer()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| range.contains(n));
    if let Some(n) = parsed {
        c.set(key, n.to_string(), &source);
    } else {
        c.raw
            .problem(key, Some(value.to_string()), &source, rule, hint);
    }
    parsed
}

/// `server.public_url`: `Some(None)` when unset, so the bound address is
/// used.
#[allow(
    clippy::option_option,
    reason = "`None` is a problem, `Some(None)` an unset URL derived from the bound address"
)]
fn public_url(c: &mut Checker<'_>, bind: Option<SocketAddr>) -> Option<Option<Url>> {
    const KEY: &str = "server.public_url";
    const HINT: &str = "Give the URL clients reach this server at, such as \
         https://cas.example.com or http://127.0.0.1:8080. The CAS is served at the origin root: \
         clients append /v1/... to it and resolve /v1/telemetry against the origin (S10), so it \
         can't live under a path; put a proxy's path prefix on its own host instead.";
    let Some((text, source)) = c.raw.string(KEY) else {
        if c.raw.values.contains_key(KEY) {
            return None;
        }
        if bind.is_some_and(|b| b.ip().is_unspecified()) {
            c.raw.problem(
                KEY,
                None,
                &Source::Default,
                "is required when server.bind listens on every interface, since the bound \
                 address is not one clients can reach",
                HINT,
            );
            return None;
        }
        c.set(KEY, "http:// and the bound address", &Source::Default);
        return Some(None);
    };
    let (url, shown) = match Url::parse(&text) {
        Ok(url) => {
            let mut shown = url.clone();
            if shown.password().is_some() {
                let _ = shown.set_password(Some("REDACTED"));
            }
            (Some(url), shown.to_string())
        }
        Err(_) => (None, text.clone()),
    };
    let mut wrong = Vec::new();
    match &url {
        None => wrong.push("it is not an absolute URL"),
        Some(url) => {
            if !matches!(url.scheme(), "http" | "https") {
                wrong.push("its scheme is not http or https");
            }
            if !url.has_host() {
                wrong.push("it has no host");
            }
            if !url.username().is_empty() || url.password().is_some() {
                wrong.push("it holds user info");
            }
            if url.path() != "/" && !url.path().is_empty() {
                wrong.push("it has a path");
            }
            if url.query().is_some() {
                wrong.push("it has a query");
            }
            if url.fragment().is_some() {
                wrong.push("it has a fragment");
            }
        }
    }
    if wrong.is_empty() {
        c.set(KEY, url.as_ref().map_or(text, Url::to_string), &source);
        Some(url)
    } else {
        c.raw.problem(
            KEY,
            Some(shown),
            &source,
            format!(
                "must be an absolute http or https URL of the origin root, with no path, query, \
                 fragment or user info; {}",
                wrong.join(", ")
            ),
            HINT,
        );
        None
    }
}

const STATIC_TOKEN_HINT: &str = "Generate one with `openxet token generate --scope <scope> \
     --mode <read|write>`, which prints the token and its [[auth.static_tokens]] entry.";

fn static_tokens(c: &mut Checker<'_>) -> Option<Vec<StaticToken>> {
    const KEY: &str = "auth.static_tokens";
    let Some((value, source)) = c.raw.values.get(KEY).cloned() else {
        c.set(KEY, "none", &Source::Default);
        return Some(Vec::new());
    };
    let Some(entries) = value.as_array() else {
        c.raw.problem(
            KEY,
            None,
            &source,
            "must be an array of tables, written [[auth.static_tokens]]",
            STATIC_TOKEN_HINT,
        );
        return None;
    };
    let mut tokens = Vec::new();
    let mut ok = true;
    for (index, entry) in entries.iter().enumerate() {
        match static_token(c, &format!("{KEY}[{index}]"), entry, &source) {
            Some(token) => tokens.push(token),
            None => ok = false,
        }
    }
    let summary = tokens
        .iter()
        .map(|t| {
            let mode = if t.mode == Mode::Write {
                "write"
            } else {
                "read"
            };
            format!("{} ({}, {mode})", t.subject, t.scope)
        })
        .collect::<Vec<_>>()
        .join(", ");
    c.set(
        KEY,
        format!("{} token(s): {summary}", tokens.len()),
        &source,
    );
    ok.then_some(tokens)
}

/// The static token entry `at`, recording a problem for each bad field.
fn static_token(
    c: &mut Checker<'_>,
    at: &str,
    entry: &toml::Value,
    source: &Source,
) -> Option<StaticToken> {
    let Some(table) = entry.as_table() else {
        c.raw
            .problem(at, None, source, "must be a table", STATIC_TOKEN_HINT);
        return None;
    };
    let mut ok = true;
    for name in table.keys() {
        if !STATIC_TOKEN_FIELDS.contains(&name.as_str()) {
            c.raw.problem(
                format!("{at}.{name}"),
                None,
                source,
                "is not a static token field",
                "A static token has sha256, scope, mode and subject.",
            );
            ok = false;
        }
    }
    let mut field = |name: &str| match table.get(name) {
        Some(toml::Value::String(text)) => Some(text.clone()),
        other => {
            c.raw.problem(
                format!("{at}.{name}"),
                other.map(ToString::to_string),
                source,
                "is required and must be a string",
                STATIC_TOKEN_HINT,
            );
            None
        }
    };
    let (sha256, scope, mode, subject) = (
        field("sha256"),
        field("scope"),
        field("mode"),
        field("subject"),
    );
    let digest = sha256.as_deref().and_then(parse_sha256);
    if sha256.is_some() && digest.is_none() {
        c.raw.problem(
            format!("{at}.sha256"),
            sha256.clone(),
            source,
            "must be 64 lowercase hex characters: the SHA-256 of the token",
            format!("Configure the token's hash, never the token itself. {STATIC_TOKEN_HINT}"),
        );
    }
    let mode = match mode.as_deref() {
        Some("read") => Some(Mode::Read),
        Some("write") => Some(Mode::Write),
        Some(other) => {
            c.raw.problem(
                format!("{at}.mode"),
                Some(other.to_owned()),
                source,
                "must be `read` or `write`",
                "`write` may upload; `read` may only download and query.",
            );
            None
        }
        None => None,
    };
    let token = StaticToken {
        sha256: digest?,
        scope: scope?,
        mode: mode?,
        subject: subject?,
    };
    ok.then_some(token)
}

fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn jwt_public_keys(c: &mut Checker<'_>) -> Option<Vec<PublicKey>> {
    const KEY: &str = "auth.jwt_public_keys";
    const HINT: &str = "Give the path of an Ed25519 public key in SPKI PEM \
         (-----BEGIN PUBLIC KEY-----), or the PEM itself. `openxet token keygen` writes one.";
    let Some((value, source)) = c.raw.values.get(KEY).cloned() else {
        c.set(KEY, "none", &Source::Default);
        return Some(Vec::new());
    };
    let Some(entries) = value.as_array() else {
        c.raw
            .problem(KEY, None, &source, "must be an array of strings", HINT);
        return None;
    };
    let mut keys = Vec::new();
    let mut shown = Vec::new();
    let mut ok = true;
    for (index, entry) in entries.iter().enumerate() {
        let at = format!("{KEY}[{index}]");
        let Some(text) = entry.as_str() else {
            c.raw.problem(
                &at,
                Some(entry.to_string()),
                &source,
                "must be a string",
                HINT,
            );
            ok = false;
            continue;
        };
        let (pem, label) = if text.trim_start().starts_with("-----BEGIN") {
            (Ok(text.to_owned()), "inline PEM".to_owned())
        } else {
            (std::fs::read_to_string(text), text.to_owned())
        };
        let key = match pem {
            Ok(pem) => PublicKey::from_pem(&pem).map_err(|err| err.to_string()),
            Err(err) => Err(format!("the file could not be read: {err}")),
        };
        match key {
            Ok(key) => {
                shown.push(format!("{label} (kid {})", key.kid()));
                keys.push(key);
            }
            Err(rule) => {
                c.raw.problem(&at, Some(label), &source, rule, HINT);
                ok = false;
            }
        }
    }
    c.set(KEY, shown.join(", "), &source);
    ok.then_some(keys)
}

fn authenticator(
    c: &mut Checker<'_>,
    tokens: Vec<StaticToken>,
    keys: Vec<PublicKey>,
    max_token_lifetime: Duration,
) -> Option<Arc<Authenticator>> {
    let source = c
        .raw
        .values
        .get("auth.static_tokens")
        .or_else(|| c.raw.values.get("auth.jwt_public_keys"))
        .map_or(Source::Default, |(_, source)| source.clone());
    if tokens.is_empty() && keys.is_empty() {
        c.raw.problem(
            "auth",
            None,
            &source,
            "configures no credential, so no request could authenticate",
            "Add [[auth.static_tokens]] or auth.jwt_public_keys, or remove [auth] to run in dev \
             mode.",
        );
        return None;
    }
    match Authenticator::new(tokens, keys, max_token_lifetime) {
        Ok(auth) => Some(Arc::new(auth)),
        Err(AuthError::InvalidStaticToken { index, reason }) => {
            c.raw.problem(
                format!("auth.static_tokens[{index}]"),
                None,
                &source,
                reason,
                "Give every static token a non-empty subject and scope without control \
                 characters (scope at most 1024 bytes).",
            );
            None
        }
        Err(err) => {
            c.raw.problem("auth", None, &source, err.to_string(), DOCS);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn env(vars: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        vars.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    fn load_toml(toml: &str, vars: &[(&str, &str)]) -> Loaded {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openxet.toml");
        std::fs::write(&path, toml).unwrap();
        load(Some(&path), env(vars), &Overrides::default())
    }

    fn problem_keys(loaded: &Loaded) -> Vec<&str> {
        loaded.problems.iter().map(|p| p.key.as_str()).collect()
    }

    #[test]
    fn defaults_are_dev_mode_on_localhost() {
        let loaded = load(None, env(&[]), &Overrides::default());
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let config = loaded.config.unwrap();
        assert_eq!(config.bind, SocketAddr::from(([127, 0, 0, 1], 8080)));
        assert!(config.public_url.is_none());
        assert!(config.auth.is_none());
        assert_eq!(config.url_lifetime, Duration::from_hours(1));
        assert_eq!(config.body_deadline, BodyDeadline::DEFAULT);
        assert_eq!(
            config.transfer_key_file,
            Path::new("./openxet-data/transfer.key")
        );
    }

    #[test]
    fn env_overrides_the_file_and_flags_override_env() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            "[server]\nbind = \"127.0.0.1:1000\"\ndata_dir = \"/a\"\n",
        )
        .unwrap();
        let loaded = load(
            Some(&path),
            env(&[
                ("OPENXET_SERVER__BIND", "127.0.0.1:2000"),
                ("OPENXET_SERVER__DATA_DIR", "/b"),
                ("OPENXET_BIN", "/not/config"),
                ("OPENXET_TOKEN", "not config either"),
            ]),
            &Overrides {
                port: Some(3000),
                data_dir: None,
                log_format: None,
            },
        );
        let config = loaded.config.unwrap();
        assert_eq!(config.bind.port(), 3000);
        assert_eq!(config.data_dir, Path::new("/b"));
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let loaded = load_toml(
            "[server]\nbind = \"localhost\"\nbody_min_rate = 0\n[cas]\nheartbeat_interval = \"0s\"\n\
             url_lifetime = \"25h\"\n[nope]\nx = 1\n",
            &[("OPENXET_CAS__TYPO", "1")],
        );
        assert_eq!(
            problem_keys(&loaded),
            [
                "nope.x",
                "cas.typo",
                "server.bind",
                "server.body_min_rate",
                "cas.url_lifetime",
                "cas.heartbeat_interval",
            ]
        );
        assert!(loaded.config.is_none());
    }

    #[test]
    fn zero_durations_are_refused() {
        for key in [
            "server.header_read_timeout",
            "server.body_read_timeout",
            "server.shutdown_grace",
            "cas.heartbeat_interval",
            "cas.url_lifetime",
            "auth.max_token_lifetime",
        ] {
            let (section, name) = key.split_once('.').unwrap();
            let loaded = load_toml(&format!("[{section}]\n{name} = \"0s\"\n"), &[]);
            assert!(
                problem_keys(&loaded).contains(&key),
                "{key}: {:?}",
                loaded.problems
            );
        }
    }

    #[test]
    fn lifetime_bounds_come_from_openxet_auth() {
        let ok = load_toml(
            "[cas]\nurl_lifetime = \"30m\"\n[auth]\nmax_token_lifetime = \"30d\"\n\
             jwt_public_keys = []\n[[auth.static_tokens]]\nsha256 = \"{}\"\nscope = \"s\"\n\
             mode = \"read\"\nsubject = \"x\"\n"
                .replace("{}", &"a".repeat(64))
                .as_str(),
            &[],
        );
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        let bad = load_toml(
            "[cas]\nurl_lifetime = \"29m\"\n[auth]\nmax_token_lifetime = \"31d\"\n",
            &[],
        );
        assert!(problem_keys(&bad).contains(&"cas.url_lifetime"));
        assert!(problem_keys(&bad).contains(&"auth.max_token_lifetime"));
    }

    #[test]
    fn static_tokens_are_checked_field_by_field() {
        let loaded = load_toml(
            "[[auth.static_tokens]]\nsha256 = \"ABC\"\nscope = \"s\"\nmode = \"admin\"\n\
             secret = \"x\"\n",
            &[],
        );
        assert_eq!(
            problem_keys(&loaded),
            [
                "auth.static_tokens[0].secret",
                "auth.static_tokens[0].subject",
                "auth.static_tokens[0].sha256",
                "auth.static_tokens[0].mode",
            ]
        );
    }

    #[test]
    fn an_empty_auth_section_is_not_dev_mode() {
        let loaded = load_toml("[auth]\n", &[]);
        assert_eq!(problem_keys(&loaded), ["auth"]);
        let loaded = load(
            None,
            env(&[("OPENXET_AUTH__MAX_TOKEN_LIFETIME", "1h")]),
            &Overrides::default(),
        );
        assert_eq!(problem_keys(&loaded), ["auth"]);
    }

    #[test]
    fn invalid_static_token_identity_names_the_entry() {
        let loaded = load_toml(
            &format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nscope = \"s\"\nmode = \"read\"\n\
                 subject = \"a\\nb\"\n",
                "0".repeat(64)
            ),
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["auth.static_tokens[0]"]);
    }

    #[test]
    fn env_values_parse_as_toml_for_non_string_keys() {
        let loaded = load(
            None,
            env(&[
                ("OPENXET_SERVER__BODY_MIN_RATE", "8192"),
                ("OPENXET_AUTH__JWT_PUBLIC_KEYS", "[\"/no/such/key.pub\"]"),
            ]),
            &Overrides::default(),
        );
        assert_eq!(problem_keys(&loaded), ["auth.jwt_public_keys[0]"]);
        let rate = loaded
            .settings
            .iter()
            .find(|s| s.key == "server.body_min_rate")
            .unwrap();
        assert_eq!(rate.value, "8192");
    }

    #[test]
    fn public_url_credentials_are_redacted_in_problems() {
        let loaded = load_toml(
            "[server]\npublic_url = \"https://u:hunter2@cas.example.com\"\n",
            &[],
        );
        let problem = &loaded.problems[0];
        assert_eq!(problem.key, "server.public_url");
        assert!(!problem.value.as_ref().unwrap().contains("hunter2"));
    }

    /// Every TOML example in `docs/reference/config.md` parses, and the
    /// full example is a valid configuration once its placeholders are
    /// filled in.
    #[test]
    fn the_documented_examples_are_valid() {
        let doc = include_str!("../../../docs/reference/config.md");
        let blocks: Vec<&str> = doc
            .split("```toml\n")
            .skip(1)
            .map(|rest| rest.split("```").next().unwrap())
            .collect();
        assert!(blocks.len() >= 2);
        for block in &blocks {
            block.parse::<toml::Table>().unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let public = dir.path().join("issuer.pub");
        let pair = openxet_auth::generate_keypair().unwrap();
        std::fs::write(&public, pair.public_pem()).unwrap();
        let example = blocks
            .last()
            .unwrap()
            .replace("/etc/openxet/issuer.pub", public.to_str().unwrap())
            .replace("<from openxet token generate>", &"0".repeat(64));
        let loaded = load_toml(&example, &[]);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let config = loaded.config.unwrap();
        assert_eq!(config.visibility, Visibility::PerScope);
        assert!(config.auth.is_some());
    }

    #[test]
    fn a_broken_file_names_the_line() {
        let loaded = load_toml("[server]\nbind = \n", &[]);
        assert_eq!(problem_keys(&loaded), ["--config"]);
        assert!(
            loaded.problems[0].hint.contains("line 2"),
            "{:?}",
            loaded.problems
        );
    }
}
