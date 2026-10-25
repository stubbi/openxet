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

use openxet_auth::{
    Action, AuthError, Authenticator, CasGrant, Glob, Mode, PublicKey, REPO_SCOPE_PREFIX, Rule,
    StaticToken, UrlSigner, UserProfile,
};
use openxet_cas::{BodyDeadline, CasConfig, MemoryBudget};
use openxet_git::{Budget, GitConfig};
use openxet_hub::HubConfig;
use openxet_index::Visibility;
use serde::Serialize;
use url::Url;

use crate::{duration, host_memory, secret, size};

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
    /// The rejected value as written, if there is one to show. A value
    /// under `auth` or one that holds a token is `<redacted>`.
    pub value: Option<String>,
    /// Where the value came from.
    pub source: String,
    /// The rule the value breaks.
    pub rule: String,
    /// What to do about it.
    pub hint: String,
    /// Whether `value` is text to show in quotes, rather than a TOML value
    /// shown as written (`0`, `"abc"`) or a placeholder.
    #[serde(skip)]
    pub quoted: bool,
}

/// What a problem shows instead of a value under `auth`, which may be a
/// credential (`auth.static_tokens[].sha256` is where a raw token is most
/// often pasted by mistake).
pub const REDACTED: &str = "<redacted>";

/// The hint for a token pasted where its hash belongs.
const RAW_TOKEN_IN_SHA256_HINT: &str = "This looks like a raw token, not its SHA-256; rotate \
     it and configure its hash (`openxet token generate`).";

/// The hint for a token in any other setting.
const RAW_TOKEN_HINT: &str = "This looks like a raw token, which no setting holds; rotate it, \
     since it may have been seen.";

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
    /// `server.memory_budget`, in bytes.
    pub memory_budget: u64,
    /// The memory this process may use, if it could be read.
    pub host_memory: Option<u64>,
    pub max_connections: u32,
    pub shutdown_grace: Duration,
    pub visibility: Visibility,
    pub url_lifetime: Duration,
    pub heartbeat_interval: Duration,
    pub transfer_key_file: PathBuf,
    /// Whether `cas.transfer_key_file` was set rather than defaulted: a
    /// named key must exist, a default one is created.
    pub transfer_key_explicit: bool,
    /// The configured credentials, or `None` in dev mode (no `[auth]`).
    pub auth: Option<Arc<Authenticator>>,
    /// `[[auth.rules]]`: what user credentials may do per repository
    /// (ruling M2-R3). Empty in dev mode, where `serve` installs the dev
    /// rule.
    pub rules: Vec<Rule>,
    pub max_token_lifetime: Duration,
    /// `hub.public_url`; `None` means the server's public URL.
    pub hub_public_url: Option<Url>,
    /// `hub.cas_url`; `None` means the server's public URL, where the CAS
    /// is served.
    pub hub_cas_url: Option<Url>,
    pub lfs_threshold: u64,
    pub max_commit_bytes: u64,
    pub token_ttl: Duration,
    pub page_size: usize,
    /// `[git]` and `hub.commit_wait`: the Git engine's settings, its cache
    /// under `data_dir` by default (ruling M2-R16).
    pub git: GitConfig,
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
            send_deadline: openxet_cas::SendDeadline::DEFAULT,
            memory_budget: self.memory_budget,
        }
    }

    /// The Hub configuration for a server reached at `public_url`.
    pub fn hub(&self, public_url: &Url) -> HubConfig {
        HubConfig {
            public_url: self
                .hub_public_url
                .clone()
                .unwrap_or_else(|| public_url.clone()),
            cas_url: self
                .hub_cas_url
                .clone()
                .unwrap_or_else(|| public_url.clone()),
            lfs_threshold: self.lfs_threshold,
            max_commit_bytes: self.max_commit_bytes,
            token_ttl: self.token_ttl,
            page_size: self.page_size,
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
    Rules,
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
            ("memory_budget", Kind::Str),
            ("max_connections", Kind::Int),
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
            ("rules", Kind::Rules),
            ("jwt_public_keys", Kind::StrList),
            ("max_token_lifetime", Kind::Str),
        ],
    ),
    (
        "hub",
        &[
            ("public_url", Kind::Str),
            ("cas_url", Kind::Str),
            ("lfs_threshold", Kind::Str),
            ("max_commit_bytes", Kind::Str),
            ("token_ttl", Kind::Str),
            ("page_size", Kind::Int),
            ("commit_wait", Kind::Str),
        ],
    ),
    (
        "git",
        &[
            ("cache_dir", Kind::Str),
            ("cache_max_bytes", Kind::Str),
            ("max_plain_blob_bytes", Kind::Str),
            ("reachability.max_commits", Kind::Int),
            ("reachability.max_ms", Kind::Int),
            ("reachability.max_concurrent_per_repo", Kind::Int),
            ("reachability.max_concurrent_per_principal", Kind::Int),
            ("thin_bases.max_objects", Kind::Int),
            ("thin_bases.max_ms", Kind::Int),
        ],
    ),
];

const STATIC_TOKEN_FIELDS: &[&str] = &[
    "sha256", "subject", "scope", "mode", "tenant", "name", "email", "claims",
];

const RULE_FIELDS: &[&str] = &["subjects", "claims", "repos", "actions"];

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
        self.push(key.into(), value, true, source, rule.into(), hint.into());
    }

    /// A problem whose value is shown as TOML writes it: `0`, `"abc"`.
    fn problem_toml(
        &mut self,
        key: impl Into<String>,
        value: Option<&toml::Value>,
        source: &Source,
        rule: impl Into<String>,
        hint: impl Into<String>,
    ) {
        let value = value.map(ToString::to_string);
        self.push(key.into(), value, false, source, rule.into(), hint.into());
    }

    /// Records a problem. Values under `auth` and values that hold a token
    /// are never shown (design §5.9: credentials are never echoed).
    fn push(
        &mut self,
        key: String,
        mut value: Option<String>,
        mut quoted: bool,
        source: &Source,
        rule: String,
        mut hint: String,
    ) {
        let holds_token = value.as_deref().is_some_and(secret::contains_token);
        if holds_token || key == "auth" || key.starts_with("auth.") {
            if value.is_some() {
                value = Some(REDACTED.to_owned());
                quoted = false;
            }
            if holds_token {
                let raw = if key.ends_with("sha256") {
                    RAW_TOKEN_IN_SHA256_HINT
                } else {
                    RAW_TOKEN_HINT
                };
                hint = format!("{raw} {hint}");
            }
        }
        self.problems.push(Problem {
            key,
            value,
            source: source.to_string(),
            rule,
            hint,
            quoted,
        });
    }

    fn unknown(&mut self, key: &str, source: &Source) {
        let hint = match key.split_once('.') {
            Some((section, _)) => match keys_of(section) {
                Some(keys) => format!("The keys of [{section}] are: {keys}. {DOCS}"),
                None => {
                    format!("The sections are [server], [cas], [auth], [hub] and [git]. {DOCS}")
                }
            },
            None => {
                format!("Put keys in a [server], [cas], [auth], [hub] or [git] section. {DOCS}")
            }
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
                match value {
                    // A subsection such as `[git.reachability]`: its keys
                    // are `git.reachability.max_commits` and so on.
                    toml::Value::Table(inner) if kind_of(&key).is_none() => {
                        for (name, value) in inner {
                            self.insert(format!("{key}.{name}"), value, &source);
                        }
                    }
                    value => self.insert(key, value, &source),
                }
            }
        }
    }

    /// Records `key`'s `value`, or `key` as unknown.
    fn insert(&mut self, key: String, value: toml::Value, source: &Source) {
        if kind_of(&key).is_some() {
            self.values.insert(key, (value, source.clone()));
        } else {
            self.unknown(&key, source);
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
            // `__` also separates a subsection's key:
            // `OPENXET_GIT__REACHABILITY__MAX_MS` is `git.reachability.max_ms`.
            let key = format!(
                "{}.{}",
                section.to_lowercase(),
                key.to_lowercase().replace("__", ".")
            );
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
            self.problem_toml(
                key,
                Some(&value),
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

    /// A byte size within `min..=max`.
    fn size(&mut self, key: &str, default: u64, (min, max): (u64, u64), hint: &str) -> Option<u64> {
        let shown = size::format(default);
        let rule = format!(
            "must be {}, from {} to {} inclusive",
            size::FORM,
            size::format(min),
            size::format(max)
        );
        self.parsed(
            key,
            (default, &shown),
            |text| size::parse(text).filter(|n| (min..=max).contains(n)),
            &rule,
            hint,
        )
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

/// The default `server.memory_budget` for a host with `memory` bytes
/// (ruling M1-R40, review I-3): a quarter of it, leaving the rest to the
/// index, the CPU-bound work and the system, rounded down to a whole MiB,
/// from [`MemoryBudget::MIN_BYTES`] to [`MemoryBudget::MAX_BYTES`]. Where
/// the memory can't be read, [`MemoryBudget::DEFAULT_BYTES`].
fn default_memory_budget(memory: Option<u64>) -> u64 {
    memory.map_or(MemoryBudget::DEFAULT_BYTES, |m| {
        ((m / 4) & !((1 << 20) - 1)).clamp(MemoryBudget::MIN_BYTES, MemoryBudget::MAX_BYTES)
    })
}

/// A warning if `budget` is over half of the `memory` this process may
/// use: request data alone could then crowd out the rest of the process.
pub fn memory_budget_warning(budget: u64, memory: Option<u64>) -> Option<String> {
    let memory = memory?;
    (budget > memory / 2).then(|| {
        format!(
            "server.memory_budget is {}, more than half of the {} of memory this process may use; \
             under load it may run out of memory",
            size::format(budget),
            size::format(memory & !((1 << 20) - 1)),
        )
    })
}

/// The default `server.max_connections`: far more than the few dozen
/// connections a busy client opens, well below the open-file limits
/// servers are given.
const DEFAULT_MAX_CONNECTIONS: u32 = 4096;

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
    let host_memory = host_memory::total();
    let memory_budget = c.size(
        "server.memory_budget",
        default_memory_budget(host_memory),
        (MemoryBudget::MIN_BYTES, MemoryBudget::MAX_BYTES),
        "Upload bodies and reconstruction answers reserve from it; uploads queue up to 120s \
         for room, then get 503 memory_busy. The default is a quarter of this host's memory \
         (1GiB if it can't be read); keep it under half.",
    );
    let max_connections = int(
        c,
        "server.max_connections",
        DEFAULT_MAX_CONNECTIONS,
        1..=1_000_000,
        "must be a whole number from 1 to 1000000",
        "Connections past it wait unaccepted until one closes. Keep it below the process's \
         open-file limit (ulimit -n); 4096 is the default.",
    );
    let shutdown_grace = c.duration(
        "server.shutdown_grace",
        Duration::from_secs(30),
        (SECOND, Duration::from_mins(10)),
        "On SIGTERM or SIGINT, in-flight requests get this long to finish.",
    );

    // `per-scope` was renamed before any release (ruling M1-R38).
    let renamed = matches!(
        c.raw.values.get("cas.dedup_visibility"),
        Some((toml::Value::String(text), _)) if text == "per-scope"
    );
    let visibility = c.parsed(
        "cas.dedup_visibility",
        (Visibility::Global, "global"),
        |text| match text {
            "global" => Some(Visibility::Global),
            "per-tenant" => Some(Visibility::PerTenant),
            _ => None,
        },
        "must be `global` or `per-tenant`",
        if renamed {
            "`per-scope` was renamed to `per-tenant`, which isolates tenants; with no tenant on \
             your tokens each scope is its own tenant, as before. Set dedup_visibility = \
             \"per-tenant\"."
        } else {
            "`global` deduplicates across tenants; `per-tenant` keeps each tenant's xorbs \
             invisible to the others."
        },
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
    let transfer_key_explicit = c.raw.values.contains_key("cas.transfer_key_file");
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
    let (auth, rules) = if c.raw.auth {
        let tokens = static_tokens(c);
        let rules = rules(c);
        let keys = jwt_public_keys(c);
        let auth = match (tokens, keys, max_token_lifetime) {
            (Some(tokens), Some(keys), Some(max)) => authenticator(c, tokens, keys, max),
            _ => None,
        };
        (auth, rules)
    } else {
        c.set(
            "auth",
            "none: dev mode, with a generated token for subject `dev`, who may read, write and \
             create every repository",
            &Source::Default,
        );
        (None, Some(Vec::new()))
    };
    let hub = hub(c, max_token_lifetime)?;
    let git = git(c, data_dir.as_deref());

    Some(Config {
        bind: bind?,
        public_url: public_url?,
        data_dir: data_dir?,
        log_format: log_format?,
        log_level: log_level?,
        header_read_timeout: header_read_timeout?,
        body_deadline: BodyDeadline::new(body_read_timeout?, body_min_rate?)?,
        memory_budget: memory_budget?,
        host_memory,
        max_connections: max_connections?,
        shutdown_grace: shutdown_grace?,
        visibility: visibility?,
        url_lifetime: url_lifetime?,
        heartbeat_interval: heartbeat_interval?,
        transfer_key_file: transfer_key_file?,
        transfer_key_explicit,
        auth: if c.raw.auth { Some(auth?) } else { None },
        rules: rules?,
        max_token_lifetime: max_token_lifetime?,
        hub_public_url: hub.public_url,
        hub_cas_url: hub.cas_url,
        lfs_threshold: hub.lfs_threshold,
        max_commit_bytes: hub.max_commit_bytes,
        token_ttl: hub.token_ttl,
        page_size: hub.page_size,
        git: git?,
    })
}

/// The `[hub]` section, checked.
struct Hub {
    public_url: Option<Url>,
    cas_url: Option<Url>,
    lfs_threshold: u64,
    max_commit_bytes: u64,
    token_ttl: Duration,
    page_size: usize,
}

/// `[hub]`, every key checked; `None` if any has a problem. The token
/// lifetime is checked against `max_token_lifetime`, since the server
/// refuses CAS tokens that live longer, its own included.
fn hub(c: &mut Checker<'_>, max_token_lifetime: Option<Duration>) -> Option<Hub> {
    let public_url = optional_origin_url(
        c,
        "hub.public_url",
        "Give the URL huggingface_hub reaches the Hub API at, its HF_ENDPOINT, such as \
         https://hub.example.com. Every repository URL the Hub answers is built from it, and \
         clients parse those against their endpoint, so it must be the one they use; the Hub \
         is served at the origin root.",
    );
    let cas_url = optional_origin_url(
        c,
        "hub.cas_url",
        "Give the URL clients reach the CAS at, which Hub tokens name, such as \
         https://cas.example.com. Leave it out when the CAS is served beside the Hub.",
    );
    let lfs_threshold = c.size(
        "hub.lfs_threshold",
        HubConfig::DEFAULT_LFS_THRESHOLD,
        (HubConfig::MIN_LFS_THRESHOLD, HubConfig::MAX_LFS_THRESHOLD),
        "Files at least this large are uploaded through Xet; smaller ones travel inside the \
         commit as Git blobs. 1MiB is the default.",
    );
    let max_commit_bytes = c.size(
        "hub.max_commit_bytes",
        HubConfig::DEFAULT_MAX_COMMIT_BYTES,
        (1 << 20, 1 << 30),
        "The largest commit body huggingface_hub may send. 64MiB is the default.",
    );
    if let (Some(threshold), Some(max)) = (lfs_threshold, max_commit_bytes)
        && max < 2 * threshold
        && let Some((_, source)) = c.raw.values.get("hub.max_commit_bytes").cloned()
    {
        c.raw.problem(
            "hub.max_commit_bytes",
            Some(size::format(max)),
            &source,
            format!(
                "must be at least twice hub.lfs_threshold ({})",
                size::format(threshold)
            ),
            "A regular file of up to hub.lfs_threshold travels base64-encoded inside the commit \
             body, so the body must have room for one; raise it or lower hub.lfs_threshold.",
        );
        return None;
    }
    let token_ttl = c.duration(
        "hub.token_ttl",
        HubConfig::DEFAULT_TOKEN_TTL,
        (Duration::from_mins(1), Duration::from_hours(24)),
        "How long the CAS tokens the Hub hands clients live; clients refresh them before they \
         expire. 15m is the default.",
    );
    if let (Some(ttl), Some(max)) = (token_ttl, max_token_lifetime)
        && ttl > max
    {
        let source = c
            .raw
            .values
            .get("hub.token_ttl")
            .map_or(Source::Default, |(_, source)| source.clone());
        c.raw.problem(
            "hub.token_ttl",
            Some(duration::format(ttl)),
            &source,
            format!(
                "must be at most auth.max_token_lifetime ({})",
                duration::format(max)
            ),
            "The server refuses CAS tokens that live longer than auth.max_token_lifetime, its \
             own included; lower hub.token_ttl or raise auth.max_token_lifetime.",
        );
        return None;
    }
    let max_page = u32::try_from(HubConfig::MAX_PAGE_SIZE).unwrap_or(u32::MAX);
    let page_size = int(
        c,
        "hub.page_size",
        max_page,
        1..=max_page,
        "must be a whole number from 1 to 1000",
        "Listings answer at most this many entries per page and link the next (ruling M2-R8). \
         1000 is the default.",
    );
    Some(Hub {
        public_url: public_url?,
        cas_url: cas_url?,
        lfs_threshold: lfs_threshold?,
        max_commit_bytes: max_commit_bytes?,
        token_ttl: token_ttl?,
        page_size: usize::try_from(page_size?).ok()?,
    })
}

/// `[git]` and `hub.commit_wait`, every key checked; `None` if any has a
/// problem (ruling M2-R16, ADR 0004 items 2 and 3). The cache defaults to
/// `{data_dir}/git-cache`.
#[allow(
    clippy::too_many_lines,
    reason = "one block per key, in documentation order, reads best whole"
)]
fn git(c: &mut Checker<'_>, data_dir: Option<&Path>) -> Option<GitConfig> {
    const KEY: &str = "git.cache_dir";
    let cache_dir = match c.raw.string(KEY) {
        Some((text, source)) if !text.is_empty() => {
            c.set(KEY, &text, &source);
            Some(PathBuf::from(text))
        }
        Some((_, source)) => {
            c.raw.problem(
                KEY,
                Some(String::new()),
                &source,
                "must be a directory path",
                "Leave it out to keep the Git cache in the data directory.",
            );
            None
        }
        None if c.raw.values.contains_key(KEY) => None,
        None => data_dir.map(|dir| {
            let path = dir.join("git-cache");
            c.set(KEY, path.display().to_string(), &Source::Default);
            path
        }),
    };
    let cache_max_bytes = c.size(
        "git.cache_max_bytes",
        openxet_git::DEFAULT_CACHE_MAX_BYTES,
        (1 << 30, 1 << 50),
        "The node's Git pack cache is evicted down to this size; packs are hydrated from the \
         bucket again on demand. 20GiB is the default.",
    );
    let max_plain_blob_bytes = if c.raw.values.contains_key("git.max_plain_blob_bytes") {
        c.size(
            "git.max_plain_blob_bytes",
            0,
            (1 << 10, 1 << 40),
            "A pushed blob larger than this that is not an LFS pointer is refused, naming `git \
             lfs track`; leave it out to accept any size and only report large ones.",
        )
        .map(Some)
    } else {
        c.set("git.max_plain_blob_bytes", "unset", &Source::Default);
        Some(None)
    };
    let reach = Budget::REACHABILITY;
    let thin = Budget::THIN_BASES;
    let max_commits = budget_int(
        c,
        "git.reachability.max_commits",
        reach.max_objects,
        "The most commits one request's reachability check (R22) may walk; a request needing \
         more is refused, retryably. 200000 is the default.",
    );
    let reach_ms = budget_int(
        c,
        "git.reachability.max_ms",
        millis(reach.max_time),
        "The most milliseconds one request's reachability check may take. 2000 is the default.",
    );
    let per_repo = budget_int(
        c,
        "git.reachability.max_concurrent_per_repo",
        count(reach.max_concurrent_per_repo),
        "The most reachability checks that may run at once in one repository; more are refused \
         as retryable. 2 is the default.",
    );
    let per_principal = budget_int(
        c,
        "git.reachability.max_concurrent_per_principal",
        count(reach.max_concurrent_per_principal),
        "The most reachability checks one caller may run at once. 4 is the default.",
    );
    let thin_objects = budget_int(
        c,
        "git.thin_bases.max_objects",
        thin.max_objects,
        "The most objects a push's thin-base check may visit. 2000000 is the default.",
    );
    let thin_ms = budget_int(
        c,
        "git.thin_bases.max_ms",
        millis(thin.max_time),
        "The most milliseconds a push's thin-base check may take. 2000 is the default.",
    );
    let commit_wait = c.duration(
        "hub.commit_wait",
        openxet_git::DEFAULT_HUB_COMMIT_WAIT,
        (SECOND, Duration::from_mins(10)),
        "How long a Hub commit waits for the commits to its branch already running on this \
         node before it is refused as retryable. 30s is the default.",
    );
    let mut git = GitConfig::new(cache_dir?);
    git.cache_max_bytes = cache_max_bytes?;
    git.max_plain_blob_bytes = max_plain_blob_bytes?;
    git.reachability = Budget {
        max_objects: max_commits?,
        max_time: Duration::from_millis(reach_ms?),
        max_concurrent_per_repo: Some(usize::try_from(per_repo?).ok()?),
        max_concurrent_per_principal: Some(usize::try_from(per_principal?).ok()?),
    };
    git.thin_bases = Budget {
        max_objects: thin_objects?,
        max_time: Duration::from_millis(thin_ms?),
        ..thin
    };
    git.hub_commit_wait = commit_wait?;
    Some(git)
}

/// A default concurrency limit as a budget value; ADR 0004's defaults set
/// both.
fn count(limit: Option<usize>) -> u64 {
    limit.map_or(u64::MAX, |n| u64::try_from(n).unwrap_or(u64::MAX))
}

/// `d` in whole milliseconds.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// A positive whole-number budget at `key`, `default` if unset.
fn budget_int(c: &mut Checker<'_>, key: &str, default: u64, hint: &str) -> Option<u64> {
    let default = u32::try_from(default).unwrap_or(u32::MAX);
    int(
        c,
        key,
        default,
        1..=u32::MAX,
        "must be a whole number from 1",
        hint,
    )
    .map(u64::from)
}

/// An optional origin URL at `key`, checked as `server.public_url` is:
/// `Some(None)` when unset, so the server's public URL is used.
#[allow(
    clippy::option_option,
    reason = "`None` is a problem, `Some(None)` an unset URL that defaults to the server's"
)]
fn optional_origin_url(c: &mut Checker<'_>, key: &str, hint: &str) -> Option<Option<Url>> {
    let Some((text, source)) = c.raw.string(key) else {
        if c.raw.values.contains_key(key) {
            return None;
        }
        c.set(key, "server.public_url", &Source::Default);
        return Some(None);
    };
    origin_url(c, key, &text, &source, hint).map(Some)
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
        c.raw.problem_toml(key, Some(&value), &source, rule, hint);
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
    origin_url(c, KEY, &text, &source, HINT).map(Some)
}

/// `text`, the value of `key` from `source`, as an absolute `http` or
/// `https` URL of an origin root: no path, query, fragment or user info.
/// A password is never shown in a problem.
fn origin_url(
    c: &mut Checker<'_>,
    key: &str,
    text: &str,
    source: &Source,
    hint: &str,
) -> Option<Url> {
    let (url, shown) = match Url::parse(text) {
        Ok(url) => {
            let mut shown = url.clone();
            if shown.password().is_some() {
                let _ = shown.set_password(Some("REDACTED"));
            }
            (Some(url), shown.to_string())
        }
        Err(_) => (None, text.to_owned()),
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
        let url = url?;
        c.set(key, url.to_string(), source);
        Some(url)
    } else {
        c.raw.problem(
            key,
            Some(shown),
            source,
            format!(
                "must be an absolute http or https URL of the origin root, with no path, query, \
                 fragment or user info; {}",
                wrong.join(", ")
            ),
            hint,
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
            let mut parts = Vec::new();
            if let Some(grant) = &t.cas {
                let mode = if grant.mode == Mode::Write {
                    "write"
                } else {
                    "read"
                };
                parts.push(match &grant.tenant {
                    Some(tenant) => format!("{}, tenant {tenant}, {mode}", grant.scope),
                    None => format!("{}, {mode}", grant.scope),
                });
            }
            if t.user.is_some() {
                parts.push("user".to_owned());
            }
            format!("{} ({})", t.subject, parts.join("; "))
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
/// Without `scope` it is a user credential; with one, a CAS credential,
/// and a user credential too when it names `name`, `email` or `claims`
/// (ruling M2-R3).
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
                "A static token has sha256 and subject; scope, mode and optionally tenant for \
                 CAS routes; and optionally name, email and claims for the Hub.",
            );
            ok = false;
        }
    }
    let mut required = |name: &str| match table.get(name) {
        Some(toml::Value::String(text)) => Some(text.clone()),
        other => {
            c.raw.problem_toml(
                format!("{at}.{name}"),
                other,
                source,
                "is required and must be a string",
                STATIC_TOKEN_HINT,
            );
            None
        }
    };
    let (sha256, subject) = (required("sha256"), required("subject"));
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
    let cas = cas_grant(c, at, table, source);
    let user = user_profile(c, at, table, source);
    let token = StaticToken {
        sha256: digest?,
        subject: subject?,
        cas: cas?,
        user: match user? {
            None if !table.contains_key("scope") => Some(UserProfile::default()),
            user => user,
        },
    };
    ok.then_some(token)
}

/// An optional string field of `table`: `Some(None)` when absent, `None`
/// (with a problem) when not a string.
#[allow(
    clippy::option_option,
    reason = "`None` is a problem, `Some(None)` a field left out"
)]
fn optional_string(
    c: &mut Checker<'_>,
    at: &str,
    table: &toml::Table,
    name: &str,
    source: &Source,
    hint: &str,
) -> Option<Option<String>> {
    match table.get(name) {
        None => Some(None),
        Some(toml::Value::String(text)) => Some(Some(text.clone())),
        other => {
            c.raw.problem_toml(
                format!("{at}.{name}"),
                other,
                source,
                "must be a string when given",
                hint,
            );
            None
        }
    }
}

/// The CAS half of a static token: `scope`, `mode` and `tenant`.
#[allow(
    clippy::option_option,
    reason = "`None` is a problem, `Some(None)` a user-only token"
)]
fn cas_grant(
    c: &mut Checker<'_>,
    at: &str,
    table: &toml::Table,
    source: &Source,
) -> Option<Option<CasGrant>> {
    let scope = optional_string(
        c,
        at,
        table,
        "scope",
        source,
        "The scope limits the token on CAS routes; leave it out for a user token.",
    );
    let tenant = optional_string(
        c,
        at,
        table,
        "tenant",
        source,
        "The tenant groups scopes for `per-tenant` dedup visibility; leave it out to use the \
         scope.",
    );
    let mode = match table.get("mode") {
        None => Some(None),
        Some(toml::Value::String(text)) if text == "read" => Some(Some(Mode::Read)),
        Some(toml::Value::String(text)) if text == "write" => Some(Some(Mode::Write)),
        Some(toml::Value::String(text)) => {
            c.raw.problem(
                format!("{at}.mode"),
                Some(text.clone()),
                source,
                "must be `read` or `write`",
                "`write` may upload; `read` may only download and query.",
            );
            None
        }
        other => {
            c.raw.problem_toml(
                format!("{at}.mode"),
                other,
                source,
                "must be `read` or `write`",
                "`write` may upload; `read` may only download and query.",
            );
            None
        }
    };
    let Some(scope) = scope? else {
        let mut ok = true;
        for (name, given) in [
            ("mode", table.contains_key("mode")),
            ("tenant", table.contains_key("tenant")),
        ] {
            if given {
                c.raw.problem(
                    format!("{at}.{name}"),
                    None,
                    source,
                    "is given without a scope",
                    "Give the scope it applies to, or leave it out for a user token, which has \
                     no scope.",
                );
                ok = false;
            }
        }
        return ok.then_some(None);
    };
    // Repository scopes are minted by the Hub for repositories it resolved
    // (M2-R2); a static token naming one would bypass the authorizer.
    let scope = if scope.starts_with(REPO_SCOPE_PREFIX) {
        c.raw.problem(
            format!("{at}.scope"),
            Some(scope),
            source,
            "starts with `repo:`, a prefix reserved for repositories",
            "Pick another scope; for access to a repository, give the token no scope and \
             grant its subject an [[auth.rules]] entry.",
        );
        None
    } else {
        Some(scope)
    };
    let mode = mode?;
    if mode.is_none() {
        c.raw.problem(
            format!("{at}.mode"),
            None,
            source,
            "is required with a scope",
            "`write` may upload; `read` may only download and query.",
        );
    }
    Some(Some(CasGrant {
        scope: scope?,
        mode: mode?,
        tenant: tenant?,
    }))
}

/// The user half of a static token: `name`, `email` and `claims`; `None`
/// inside when none is given.
#[allow(
    clippy::option_option,
    reason = "`None` is a problem, `Some(None)` no user fields"
)]
fn user_profile(
    c: &mut Checker<'_>,
    at: &str,
    table: &toml::Table,
    source: &Source,
) -> Option<Option<UserProfile>> {
    let name = optional_string(
        c,
        at,
        table,
        "name",
        source,
        "The name whoami answers for the token; it defaults to the subject.",
    );
    let email = optional_string(
        c,
        at,
        table,
        "email",
        source,
        "The address Hub commits made with the token are authored with.",
    );
    let claims = match table.get("claims") {
        None => Some(None),
        Some(value) => string_lists(c, &format!("{at}.claims"), value, source).map(Some),
    };
    let (name, email, claims) = (name?, email?, claims?);
    if name.is_none() && email.is_none() && claims.is_none() {
        return Some(None);
    }
    Some(Some(UserProfile {
        name,
        email,
        claims: claims.unwrap_or_default(),
    }))
}

const CLAIMS_HINT: &str = "Write claims as a table of string lists, such as \
     `claims = { orgs = [\"acme\"], groups = [\"ml-team\"] }`.";

/// A table of string lists at `at`, such as `claims`, recording a problem
/// for each entry that isn't one.
fn string_lists(
    c: &mut Checker<'_>,
    at: &str,
    value: &toml::Value,
    source: &Source,
) -> Option<BTreeMap<String, Vec<String>>> {
    let Some(table) = value.as_table() else {
        c.raw.problem_toml(
            at,
            Some(value),
            source,
            "must be a table of string lists",
            CLAIMS_HINT,
        );
        return None;
    };
    let mut lists = BTreeMap::new();
    let mut ok = true;
    for (name, entry) in table {
        if let Some(list) = string_list(entry) {
            lists.insert(name.clone(), list);
        } else {
            c.raw.problem_toml(
                format!("{at}.{name}"),
                Some(entry),
                source,
                "must be a list of strings",
                CLAIMS_HINT,
            );
            ok = false;
        }
    }
    ok.then_some(lists)
}

fn string_list(value: &toml::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}

const RULES_HINT: &str = "A rule is `[[auth.rules]]` with `subjects = [\"alice\"]` and/or \
     `claims = { groups = [\"ml-team\"] }`, `repos = [\"org/*\"]` and \
     `actions = [\"read\", \"write\", \"create\"]`. See docs/reference/config.md#rules.";

/// `[[auth.rules]]` (ruling M2-R3), every entry checked field by field.
fn rules(c: &mut Checker<'_>) -> Option<Vec<Rule>> {
    const KEY: &str = "auth.rules";
    let Some((value, source)) = c.raw.values.get(KEY).cloned() else {
        c.set(KEY, "none: user tokens may do nothing", &Source::Default);
        return Some(Vec::new());
    };
    let Some(entries) = value.as_array() else {
        c.raw.problem(
            KEY,
            None,
            &source,
            "must be an array of tables, written [[auth.rules]]",
            RULES_HINT,
        );
        return None;
    };
    let mut rules = Vec::new();
    let mut ok = true;
    for (index, entry) in entries.iter().enumerate() {
        match rule(c, &format!("{KEY}[{index}]"), entry, &source) {
            Some(rule) => rules.push(rule),
            None => ok = false,
        }
    }
    let summary = rules
        .iter()
        .map(|rule| {
            let mut who: Vec<String> = rule.subjects.clone();
            who.extend(
                rule.claims
                    .iter()
                    .flat_map(|(name, values)| values.iter().map(move |v| format!("{name}:{v}"))),
            );
            let repos: Vec<&str> = rule.repos.iter().map(Glob::as_str).collect();
            let actions: Vec<&str> = rule.actions.iter().map(|a| a.name()).collect();
            format!(
                "{} may {} {}",
                who.join(" or "),
                actions.join(", "),
                repos.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    c.set(KEY, format!("{} rule(s): {summary}", rules.len()), &source);
    ok.then_some(rules)
}

/// The rule entry `at`, recording a problem for each bad field.
fn rule(c: &mut Checker<'_>, at: &str, entry: &toml::Value, source: &Source) -> Option<Rule> {
    let Some(table) = entry.as_table() else {
        c.raw
            .problem(at, None, source, "must be a table", RULES_HINT);
        return None;
    };
    let mut ok = true;
    for name in table.keys() {
        if !RULE_FIELDS.contains(&name.as_str()) {
            c.raw.problem(
                format!("{at}.{name}"),
                None,
                source,
                "is not a rule field",
                RULES_HINT,
            );
            ok = false;
        }
    }
    let subjects = match table.get("subjects") {
        None => Some(Vec::new()),
        Some(value) => rule_subjects(c, &format!("{at}.subjects"), value, source),
    };
    let claims = match table.get("claims") {
        None => Some(BTreeMap::new()),
        Some(value) => string_lists(c, &format!("{at}.claims"), value, source),
    };
    if let (Some(subjects), Some(claims)) = (&subjects, &claims)
        && subjects.is_empty()
        && claims.values().all(Vec::is_empty)
    {
        c.raw.problem(
            at,
            None,
            source,
            "matches nobody: it names no subject and no claim value",
            "Give `subjects`, `claims` or both; `subjects = [\"anonymous\"]` applies it to \
             everyone, with or without a credential.",
        );
        ok = false;
    }
    let repos = list_field(c, at, table, "repos", source, |text| {
        Glob::new(text).map_err(|err| err.reason)
    });
    let actions = list_field(c, at, table, "actions", source, |text| {
        Action::parse(text).ok_or("is not `read`, `write` or `create`")
    });
    let rule = Rule {
        subjects: subjects?,
        claims: claims?,
        repos: repos?,
        actions: actions?,
    };
    ok.then_some(rule)
}

/// A rule's `subjects` at `at`: each a subject a credential can carry, and
/// never `*`, which subjects don't expand (ruling M2-R3). One problem names
/// every bad entry by position.
fn rule_subjects(
    c: &mut Checker<'_>,
    at: &str,
    value: &toml::Value,
    source: &Source,
) -> Option<Vec<String>> {
    let Some(subjects) = string_list(value) else {
        c.raw.problem_toml(
            at,
            Some(value),
            source,
            "must be a list of strings",
            RULES_HINT,
        );
        return None;
    };
    let mut wrong = Vec::new();
    let mut star = false;
    for (index, subject) in subjects.iter().enumerate() {
        if subject == "*" {
            star = true;
            wrong.push(format!("entry {index} is `*`, which matches no subject"));
        } else if let Err(reason) = openxet_auth::check_subject(subject) {
            wrong.push(format!(
                "entry {index}: {}",
                reason.trim_start_matches("the subject ")
            ));
        }
    }
    if wrong.is_empty() {
        return Some(subjects);
    }
    let hint = if star {
        format!("Use `anonymous` for everyone, with or without a credential. {RULES_HINT}")
    } else {
        format!("Name each subject as the credential carries it. {RULES_HINT}")
    };
    c.raw.problem(at, None, source, wrong.join("; "), hint);
    None
}

/// The required, non-empty list of strings `name` of the rule `at`, each
/// parsed by `parse`. One problem names the field and every bad entry by
/// position, never by value (values under `auth` are not shown).
fn list_field<T>(
    c: &mut Checker<'_>,
    at: &str,
    table: &toml::Table,
    name: &str,
    source: &Source,
    parse: impl Fn(&str) -> Result<T, &'static str>,
) -> Option<Vec<T>> {
    let key = format!("{at}.{name}");
    let Some(texts) = table.get(name).and_then(string_list) else {
        c.raw.problem_toml(
            key,
            table.get(name),
            source,
            "is required and must be a list of strings",
            RULES_HINT,
        );
        return None;
    };
    if texts.is_empty() {
        c.raw
            .problem(key, None, source, "must not be empty", RULES_HINT);
        return None;
    }
    let mut parsed = Vec::new();
    let mut wrong = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        match parse(text) {
            Ok(value) => parsed.push(value),
            Err(reason) => wrong.push(format!("entry {index} {reason}")),
        }
    }
    if wrong.is_empty() {
        Some(parsed)
    } else {
        c.raw
            .problem(key, None, source, wrong.join("; "), RULES_HINT);
        None
    }
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
            c.raw
                .problem_toml(&at, Some(entry), &source, "must be a string", HINT);
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
                "Give every static token a non-empty subject other than `anonymous`, and a scope, \
                 a user identity or both, without control characters (scope and tenant at most \
                 1024 bytes).",
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
             url_lifetime = \"61m\"\n[nope]\nx = 1\n",
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
    fn the_memory_budget_is_a_size_within_its_bounds() {
        let config = load_toml("", &[]).config.unwrap();
        let default = default_memory_budget(host_memory::total());
        assert_eq!(config.memory_budget, default);
        assert_eq!(
            config.cas(Url::parse("http://x").unwrap()).memory_budget,
            default
        );

        let config = load_toml("[server]\nmemory_budget = \"3GiB\"\n", &[])
            .config
            .unwrap();
        assert_eq!(config.memory_budget, 3 << 30);
        let config = load_toml("", &[("OPENXET_SERVER__MEMORY_BUDGET", "512MiB")])
            .config
            .unwrap();
        assert_eq!(config.memory_budget, 512 << 20);

        for bad in ["255MiB", "2TiB", "1GB", "1073741824", "0MiB"] {
            let loaded = load_toml(&format!("[server]\nmemory_budget = \"{bad}\"\n"), &[]);
            assert_eq!(problem_keys(&loaded), ["server.memory_budget"], "{bad}");
        }
        let loaded = load_toml("[server]\nmemory_budget = 1073741824\n", &[]);
        assert_eq!(problem_keys(&loaded), ["server.memory_budget"]);
    }

    #[test]
    fn the_default_memory_budget_is_a_quarter_of_detected_memory() {
        // Review I-3 of M1-R40: the 1 GiB floor applies only where memory
        // can't be read; a small host gets a quarter, at least 256 MiB.
        const GIB: u64 = 1 << 30;
        assert_eq!(default_memory_budget(Some(16 * GIB)), 4 * GIB);
        assert_eq!(default_memory_budget(Some(2 * GIB)), GIB / 2);
        assert_eq!(default_memory_budget(Some(GIB)), 256 << 20);
        assert_eq!(default_memory_budget(Some(512 << 20)), 256 << 20);
        assert_eq!(default_memory_budget(None), GIB);
        assert_eq!(default_memory_budget(Some(8 << 40)), 1 << 40);
        // Rounded down to a whole MiB, so `config check` shows a size.
        assert_eq!(default_memory_budget(Some(16_303_312 * 1024)), 3980 << 20);
    }

    #[test]
    fn a_budget_over_half_the_memory_is_warned_about() {
        const GIB: u64 = 1 << 30;
        assert_eq!(memory_budget_warning(4 * GIB, Some(16 * GIB)), None);
        assert_eq!(memory_budget_warning(8 * GIB, Some(16 * GIB)), None);
        let warning = memory_budget_warning(256 << 20, Some(400 << 20)).unwrap();
        assert!(warning.contains("256MiB"), "{warning}");
        assert!(memory_budget_warning(9 * GIB, Some(16 * GIB)).is_some());
        assert_eq!(memory_budget_warning(64 * GIB, None), None);
    }

    #[test]
    fn max_connections_is_a_count_within_its_bounds() {
        assert_eq!(load_toml("", &[]).config.unwrap().max_connections, 4096);
        let config = load_toml("", &[("OPENXET_SERVER__MAX_CONNECTIONS", "64")])
            .config
            .unwrap();
        assert_eq!(config.max_connections, 64);
        for bad in ["0", "1000001", "\"64\""] {
            let loaded = load_toml(&format!("[server]\nmax_connections = {bad}\n"), &[]);
            assert_eq!(problem_keys(&loaded), ["server.max_connections"], "{bad}");
        }
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

    fn sha() -> String {
        "a".repeat(64)
    }

    /// Without `scope` a static token is a user credential (ruling M2-R3);
    /// `mode` and `tenant` then have nothing to apply to.
    #[test]
    fn a_static_token_without_scope_is_a_user_token() {
        let loaded = load_toml(
            &format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"alice\"\nname = \"Alice\"\n\
                 email = \"alice@example.com\"\nclaims = {{ orgs = [\"acme\"] }}\n",
                sha()
            ),
            &[],
        );
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let summary = &loaded
            .settings
            .iter()
            .find(|s| s.key == "auth.static_tokens")
            .unwrap()
            .value;
        assert!(summary.contains("alice (user)"), "{summary}");

        let loaded = load_toml(
            &format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"alice\"\nmode = \"read\"\n\
                 tenant = \"t\"\n",
                sha()
            ),
            &[],
        );
        assert_eq!(
            problem_keys(&loaded),
            ["auth.static_tokens[0].mode", "auth.static_tokens[0].tenant"]
        );
        let loaded = load_toml(
            &format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"ci\"\nscope = \"s\"\n",
                sha()
            ),
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["auth.static_tokens[0].mode"]);
        let loaded = load_toml(
            &format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"anonymous\"\n",
                sha()
            ),
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["auth.static_tokens[0]"]);
    }

    #[test]
    fn claims_values_are_string_lists() {
        let entry = |claims: &str| {
            format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"alice\"\nclaims = {claims}\n",
                sha()
            )
        };
        assert!(
            load_toml(&entry("{ orgs = [\"a\", \"b\"], groups = [] }"), &[])
                .problems
                .is_empty()
        );
        assert_eq!(
            problem_keys(&load_toml(&entry("{ orgs = \"acme\", groups = [1] }"), &[])),
            [
                "auth.static_tokens[0].claims.groups",
                "auth.static_tokens[0].claims.orgs"
            ]
        );
        assert_eq!(
            problem_keys(&load_toml(&entry("[\"acme\"]"), &[])),
            ["auth.static_tokens[0].claims"]
        );
        let loaded = load_toml(
            &format!(
                "[[auth.rules]]\nclaims = {{ groups = \"ml\" }}\nrepos = [\"*\"]\n\
                 actions = [\"read\"]\n{}",
                entry("{}")
            ),
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["auth.rules[0].claims.groups"]);
    }

    /// `repo:` scopes belong to repositories (M2-R2): the Hub mints CAS
    /// tokens for them after authorizing, so a static token may not name
    /// one.
    #[test]
    fn static_token_scope_with_repo_prefix_is_a_problem() {
        let loaded = load_toml(
            &format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"ci\"\n\
                 scope = \"repo:0192f0c4a1b27c3d8e9f00112233aabb\"\nmode = \"write\"\n",
                sha()
            ),
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["auth.static_tokens[0].scope"]);
        let problem = &loaded.problems[0];
        assert!(
            problem.rule.contains("reserved for repositories"),
            "{problem:?}"
        );
        assert_eq!(problem.value.as_deref(), Some(REDACTED));
    }

    #[test]
    fn rules_are_checked_field_by_field() {
        let token = format!(
            "[[auth.static_tokens]]\nsha256 = \"{}\"\nsubject = \"alice\"\n",
            sha()
        );
        let loaded = load_toml(
            &format!(
                "{token}[[auth.rules]]\nsubjects = [\"alice\"]\nrepos = [\"org/*\"]\n\
                 actions = [\"read\", \"admin\"]\n\
                 [[auth.rules]]\nsubjects = [\"alice\"]\nrepos = [\"org/**\"]\nactions = [\"read\"]\n\
                 [[auth.rules]]\nrepos = [\"*\"]\nactions = [\"read\"]\n\
                 [[auth.rules]]\nsubjects = [\"a\"]\nrepos = []\nactions = [\"read\"]\nusers = []\n"
            ),
            &[],
        );
        assert_eq!(
            problem_keys(&loaded),
            [
                "auth.rules[0].actions",
                "auth.rules[1].repos",
                "auth.rules[2]",
                "auth.rules[3].users",
                "auth.rules[3].repos",
            ]
        );
        assert!(
            loaded.problems[0].rule.contains("entry 1"),
            "{:?}",
            loaded.problems[0]
        );

        let loaded = load_toml(
            &format!(
                "{token}[[auth.rules]]\nsubjects = [\"*\"]\nrepos = [\"*\"]\nactions = [\"read\"]\n\
                 [[auth.rules]]\nsubjects = [\"\", \"a\\nb\"]\nrepos = [\"*\"]\n\
                 actions = [\"read\"]\n"
            ),
            &[],
        );
        assert_eq!(
            problem_keys(&loaded),
            ["auth.rules[0].subjects", "auth.rules[1].subjects"]
        );
        let (star, bad) = (&loaded.problems[0], &loaded.problems[1]);
        assert!(
            star.hint.contains("Use `anonymous` for everyone"),
            "{star:?}"
        );
        assert!(
            bad.rule.contains("entry 0") && bad.rule.contains("entry 1"),
            "{bad:?}"
        );
        assert!(
            loaded
                .problems
                .iter()
                .all(|p| p.value.is_none() || p.value.as_deref() == Some(REDACTED)),
            "{:?}",
            loaded.problems
        );

        let loaded = load_toml(
            &format!(
                "{token}[[auth.rules]]\nsubjects = [\"alice\"]\nclaims = {{ groups = [\"ml\"] }}\n\
                 repos = [\"org/*\", \"*\"]\nactions = [\"read\", \"write\", \"create\"]\n"
            ),
            &[],
        );
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let rules = loaded.config.unwrap().rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].actions,
            [Action::Read, Action::Write, Action::Create]
        );
        assert_eq!(rules[0].repos[0].as_str(), "org/*");
        let summary = &loaded_setting(
            &load_toml(
                &format!(
                    "{token}[[auth.rules]]\nsubjects = [\"alice\"]\nrepos = [\"org/*\"]\n\
                     actions = [\"read\"]\n"
                ),
                &[],
            ),
            "auth.rules",
        );
        assert_eq!(summary, "1 rule(s): alice may read org/*");
    }

    fn loaded_setting(loaded: &Loaded, key: &str) -> String {
        loaded
            .settings
            .iter()
            .find(|s| s.key == key)
            .unwrap()
            .value
            .clone()
    }

    /// A static token may name its tenant (ruling M1-R38); one that isn't a
    /// valid tenant is refused like a bad scope.
    #[test]
    fn the_renamed_per_scope_value_says_what_replaced_it() {
        let loaded = load_toml("[cas]\ndedup_visibility = \"per-scope\"\n", &[]);
        assert_eq!(problem_keys(&loaded), ["cas.dedup_visibility"]);
        let hint = &loaded.problems[0].hint;
        assert!(hint.contains("renamed to `per-tenant`"), "{hint}");
        let loaded = load_toml("[cas]\ndedup_visibility = \"tenants\"\n", &[]);
        assert!(!loaded.problems[0].hint.contains("renamed"));
    }

    #[test]
    fn a_static_token_may_name_its_tenant() {
        let entry = |tenant: &str| {
            format!(
                "[[auth.static_tokens]]\nsha256 = \"{}\"\nscope = \"org/model\"\nmode = \"read\"\n\
                 subject = \"ci\"\ntenant = {tenant}\n",
                "a".repeat(64)
            )
        };
        let loaded = load_toml(&entry("\"org\""), &[]);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let summary = &loaded
            .settings
            .iter()
            .find(|s| s.key == "auth.static_tokens")
            .unwrap()
            .value;
        assert!(
            summary.contains("ci (org/model, tenant org, read)"),
            "{summary}"
        );
        assert!(loaded.config.unwrap().auth.is_some());

        assert_eq!(
            problem_keys(&load_toml(&entry("7"), &[])),
            ["auth.static_tokens[0].tenant"]
        );
        assert_eq!(
            problem_keys(&load_toml(&entry("\"org\\n\""), &[])),
            ["auth.static_tokens[0]"]
        );
        assert_eq!(
            problem_keys(&load_toml(&entry("\"scope:org\""), &[])),
            ["auth.static_tokens[0]"],
            "`scope:` is reserved for defaulted tenants"
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
        assert_eq!(config.visibility, Visibility::PerTenant);
        assert!(config.auth.is_some());
    }

    #[test]
    fn hub_keys_default_to_the_server_and_ruling_m2_r4() {
        let config = load_toml("", &[]).config.unwrap();
        let server = Url::parse("http://127.0.0.1:8080").unwrap();
        let hub = config.hub(&server);
        assert_eq!(hub.public_url, server);
        assert_eq!(hub.cas_url, server);
        assert_eq!(hub.lfs_threshold, 1 << 20);
        assert_eq!(hub.max_commit_bytes, 64 << 20);
        assert_eq!(hub.token_ttl, Duration::from_mins(15));
        assert_eq!(hub.page_size, 1000);

        let config = load_toml(
            "[hub]\npublic_url = \"https://hub.example.com\"\n\
             cas_url = \"https://cas.example.com\"\nlfs_threshold = \"64KiB\"\n\
             max_commit_bytes = \"1MiB\"\ntoken_ttl = \"30m\"\npage_size = 50\n",
            &[("OPENXET_HUB__PAGE_SIZE", "20")],
        )
        .config
        .unwrap();
        let hub = config.hub(&server);
        assert_eq!(hub.public_url.as_str(), "https://hub.example.com/");
        assert_eq!(hub.cas_url.as_str(), "https://cas.example.com/");
        assert_eq!(hub.lfs_threshold, 64 << 10);
        assert_eq!(hub.max_commit_bytes, 1 << 20);
        assert_eq!(hub.token_ttl, Duration::from_mins(30));
        assert_eq!(hub.page_size, 20);
    }

    #[test]
    fn hub_keys_are_checked_with_hints() {
        let loaded = load_toml(
            "[hub]\npublic_url = \"https://hub.example.com/prefix\"\ncas_url = \"ftp://x\"\n\
             lfs_threshold = \"512B\"\npage_size = 1001\ntoken_ttl = \"0s\"\nnope = 1\n",
            &[],
        );
        assert_eq!(
            problem_keys(&loaded),
            [
                "hub.nope",
                "hub.public_url",
                "hub.cas_url",
                "hub.lfs_threshold",
                "hub.token_ttl",
                "hub.page_size",
            ]
        );
        let loaded = load_toml("[hub]\nlfs_threshold = \"11MiB\"\n", &[]);
        assert_eq!(problem_keys(&loaded), ["hub.lfs_threshold"]);
        assert!(loaded.problems[0].rule.contains("from 1KiB to 10MiB"));

        // A regular file must fit in a commit body, base64-encoded.
        let loaded = load_toml(
            "[hub]\nlfs_threshold = \"1MiB\"\nmax_commit_bytes = \"1MiB\"\n",
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["hub.max_commit_bytes"]);
        assert!(loaded.problems[0].rule.contains("twice hub.lfs_threshold"));

        // The server refuses CAS tokens longer-lived than the cap, its own
        // included.
        let loaded = load_toml(
            &format!(
                "[auth]\nmax_token_lifetime = \"10m\"\n[[auth.static_tokens]]\n\
                 sha256 = \"{}\"\nsubject = \"a\"\n",
                "0".repeat(64)
            ),
            &[],
        );
        assert_eq!(problem_keys(&loaded), ["hub.token_ttl"]);
        assert!(
            loaded.problems[0]
                .rule
                .contains("auth.max_token_lifetime (10m)")
        );
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
