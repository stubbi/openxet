//! `openxet token`: CAS-token signing keys and tokens, and static tokens.
//!
//! CAS tokens are `EdDSA` JWTs (ADR 0007); `docs/reference/config.md` has the
//! policy the server applies to them.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_auth::{AuthError, Authenticator, Mode, StaticToken, TokenSigner, generate_keypair};
use serde::Serialize;

use crate::duration;
use crate::error::CliError;
use crate::output::{HumanDisplay, Output};
use crate::secret;

/// The default signing key of `keygen` and `mint`, under the user's config
/// directory rather than the working directory, where it would sit one
/// `git add` away from a commit.
fn default_key(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let config = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })?;
    Some(config.join("openxet").join("token.key"))
}

/// `--key`, or the default signing key.
fn key_path(key: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(key) = key {
        return Ok(key.to_owned());
    }
    default_key(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .ok_or_else(|| {
        CliError::new(
            "no_key",
            "neither XDG_CONFIG_HOME nor HOME names a directory for the signing key",
            "Pass `--key FILE`.",
        )
    })
}

#[derive(Debug, clap::Subcommand)]
pub enum TokenCommand {
    /// Write an Ed25519 key pair for signing CAS tokens and print the public key
    Keygen {
        /// Where to write the private key (mode 0600); the public key goes to
        /// FILE.pub [default: `$XDG_CONFIG_HOME/openxet/token.key`, or
        /// `~/.config/openxet/token.key`]
        #[arg(long, value_name = "FILE")]
        key: Option<PathBuf>,
    },
    /// Sign a CAS token (an `EdDSA` JWT) with a key from `token keygen`
    Mint {
        /// The scope the token is limited to
        #[arg(long)]
        scope: String,
        /// What the token may do within its scope
        #[arg(long, value_enum)]
        mode: ModeArg,
        /// How long the token is valid, from 1m to 30d
        #[arg(long, default_value = "1h", value_parser = parse_ttl)]
        ttl: Duration,
        /// Who the token acts for (its `sub` claim)
        #[arg(long, default_value = "openxet-cli")]
        subject: String,
        /// The private key from `token keygen` [default: the one `keygen`
        /// writes by default]
        #[arg(long, value_name = "FILE")]
        key: Option<PathBuf>,
    },
    /// Generate a static token and print its config entry, which holds only its hash
    Generate {
        /// The scope the token is limited to
        #[arg(long, default_value = "default")]
        scope: String,
        /// What the token may do within its scope
        #[arg(long, value_enum)]
        mode: ModeArg,
        /// Who the token acts for, as audit entries name it; give each token
        /// its own
        #[arg(long)]
        subject: String,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ModeArg {
    Read,
    Write,
}

impl ModeArg {
    fn mode(self) -> Mode {
        match self {
            Self::Read => Mode::Read,
            Self::Write => Mode::Write,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

fn parse_ttl(text: &str) -> Result<Duration, String> {
    duration::parse(text).ok_or_else(|| format!("expected {}", duration::FORM))
}

pub fn run(command: &TokenCommand, output: Output) -> Result<(), CliError> {
    match command {
        TokenCommand::Keygen { key } => keygen(&key_path(key.as_deref())?, output),
        TokenCommand::Mint {
            scope,
            mode,
            ttl,
            subject,
            key,
        } => mint(
            &key_path(key.as_deref())?,
            scope,
            *mode,
            *ttl,
            subject,
            output,
        ),
        TokenCommand::Generate {
            scope,
            mode,
            subject,
        } => generate(scope, *mode, subject, output),
    }
}

fn emit<T: Serialize + HumanDisplay>(output: Output, value: &T) -> Result<(), CliError> {
    output.emit(value).map_err(|err| CliError::output(&err))
}

fn auth_error(err: &AuthError) -> CliError {
    CliError::new(
        "key",
        err.to_string(),
        "Retry; if it persists, check the host's entropy source.",
    )
}

#[derive(Serialize)]
struct Keygen {
    schema: &'static str,
    kid: String,
    private_key: String,
    public_key: String,
    public_key_pem: String,
}

impl HumanDisplay for Keygen {
    fn human(&self) -> String {
        format!(
            "Wrote the signing key to {} (keep it secret) and the public key to {} (kid {}).\n\
             \nTrust it in the server's config:\n\
             \n[auth]\njwt_public_keys = [{}]\n\
             \nMint a token with:\n  openxet token mint --key {} --scope <scope> --mode read --ttl 1h\n\
             \n{}",
            self.private_key,
            self.public_key,
            self.kid,
            toml::Value::String(self.public_key.clone()),
            self.private_key,
            self.public_key_pem.trim_end()
        )
    }
}

fn public_path(key: &Path) -> PathBuf {
    let mut name = key.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

fn shown(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_owned())
        .display()
        .to_string()
}

fn keygen(key: &Path, output: Output) -> Result<(), CliError> {
    let public = public_path(key);
    if public.exists() {
        return Err(CliError::new(
            "exists",
            format!("{} already exists", public.display()),
            "Nothing was overwritten. Remove the key pair first, or pass another `--key`.",
        ));
    }
    let pair = generate_keypair().map_err(|err| auth_error(&err))?;
    // The public key first: a failure then never leaves a lone private key.
    secret::create_with_mode(&public, pair.public_pem().as_bytes(), 0o644)?;
    if let Err(err) = secret::create(key, pair.private_pem().as_bytes()) {
        let _ = std::fs::remove_file(&public);
        return Err(err);
    }
    emit(
        output,
        &Keygen {
            schema: "openxet.token-keygen.v1",
            kid: pair.kid().to_owned(),
            private_key: shown(key),
            public_key: shown(&public),
            public_key_pem: pair.public_pem().to_owned(),
        },
    )
}

#[derive(Serialize)]
struct Minted<'a> {
    schema: &'static str,
    token: String,
    kid: String,
    subject: &'a str,
    scope: &'a str,
    mode: &'static str,
    iat: u64,
    exp: u64,
}

impl HumanDisplay for Minted<'_> {
    fn human(&self) -> String {
        self.token.clone()
    }
}

fn mint(
    key: &Path,
    scope: &str,
    mode: ModeArg,
    ttl: Duration,
    subject: &str,
    output: Output,
) -> Result<(), CliError> {
    let pem = secret::read_unchecked(key)?.ok_or_else(|| {
        CliError::new(
            "no_key",
            format!("{} does not exist", key.display()),
            "Create a signing key with `openxet token keygen`, or pass `--key` with its path.",
        )
    })?;
    let signer = TokenSigner::from_pem(&pem).map_err(|err| {
        let hint = if pem.contains("BEGIN PUBLIC KEY") {
            "This is the public key. Pass the private key `openxet token keygen` wrote next to \
             it: the same path without `.pub`."
        } else {
            "Pass the private key `openxet token keygen` wrote (PKCS#8 PEM, -----BEGIN \
             PRIVATE KEY-----)."
        };
        CliError::new("invalid_key", format!("{}: {err}", key.display()), hint)
    })?;
    // Only a private key is held to private permissions.
    secret::check_private(key)?;
    let signer = signer.with_max_token_lifetime(ttl).map_err(|_| {
        let (min, max) = (
            Authenticator::MIN_TOKEN_LIFETIME,
            Authenticator::MAX_TOKEN_LIFETIME,
        );
        CliError::new(
            "invalid_ttl",
            format!(
                "--ttl {} is outside {} to {}",
                duration::format(ttl),
                duration::format(min),
                duration::format(max)
            ),
            format!(
                "Pass `--ttl` from {} to {}. Servers also refuse tokens that expire beyond \
                 their auth.max_token_lifetime ({} by default).",
                duration::format(min),
                duration::format(max),
                duration::format(Authenticator::DEFAULT_MAX_TOKEN_LIFETIME)
            ),
        )
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let exp = now.saturating_add(ttl.as_secs());
    let token = signer
        .sign(subject, scope, mode.mode(), exp, now)
        .map_err(|err| {
            CliError::new(
                "invalid_claims",
                err.to_string(),
                "Give a non-empty --scope and --subject without control characters (scope at \
                 most 1024 bytes).",
            )
        })?;
    if output == Output::Human && ttl > Authenticator::DEFAULT_MAX_TOKEN_LIFETIME {
        eprintln!(
            "note: servers refuse this token unless their auth.max_token_lifetime is at least {}",
            duration::format(ttl)
        );
    }
    emit(
        output,
        &Minted {
            schema: "openxet.token-mint.v1",
            token,
            kid: signer.kid().to_owned(),
            subject,
            scope,
            mode: mode.name(),
            iat: now,
            exp,
        },
    )
}

#[derive(Serialize)]
struct Generated<'a> {
    schema: &'static str,
    token: &'a str,
    sha256: String,
    scope: &'a str,
    mode: &'static str,
    subject: &'a str,
    /// The `[[auth.static_tokens]]` entry, which holds the hash only.
    config: String,
}

impl HumanDisplay for Generated<'_> {
    fn human(&self) -> String {
        format!(
            "{}\n\nThis is the only time the token is shown; store it in a secret manager.\n\
             Add this entry to the server's config, which holds only its hash:\n\n{}",
            self.token,
            self.config.trim_end()
        )
    }
}

fn generate(scope: &str, mode: ModeArg, subject: &str, output: Output) -> Result<(), CliError> {
    let token = secret::generate_token()?;
    let entry = StaticToken {
        sha256: StaticToken::digest(&token),
        scope: scope.to_owned(),
        mode: mode.mode(),
        subject: subject.to_owned(),
    };
    let sha256 = secret::hex(&entry.sha256);
    // The server applies the same checks when it loads the entry.
    Authenticator::new(
        vec![entry],
        vec![],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .map_err(|err| {
        CliError::new(
            "invalid_claims",
            err.to_string(),
            "Give a non-empty --scope and --subject without control characters (scope at \
                 most 1024 bytes).",
        )
    })?;
    let quote = |text: &str| toml::Value::String(text.to_owned()).to_string();
    let config = format!(
        "[[auth.static_tokens]]\nsha256 = \"{sha256}\"\nscope = {}\nmode = \"{}\"\nsubject = {}\n",
        quote(scope),
        mode.name(),
        quote(subject),
    );
    emit(
        output,
        &Generated {
            schema: "openxet.token-generate.v1",
            token: &token,
            sha256,
            scope,
            mode: mode.name(),
            subject,
            config,
        },
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_default_key_lives_in_the_config_directory() {
        assert_eq!(
            default_key(Some("/x/cfg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/cfg/openxet/token.key"))
        );
        assert_eq!(
            default_key(Some("relative".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/openxet/token.key"))
        );
        assert_eq!(default_key(None, None), None);
    }

    #[test]
    fn the_trust_snippet_escapes_the_path() {
        let shown = Keygen {
            schema: "openxet.token-keygen.v1",
            kid: "k".to_owned(),
            private_key: "/a/key".to_owned(),
            public_key: r#"/a "quoted"\dir/key.pub"#.to_owned(),
            public_key_pem: String::new(),
        }
        .human();
        let snippet = shown
            .lines()
            .skip_while(|l| *l != "[auth]")
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        let table: toml::Table = snippet.parse().unwrap();
        assert_eq!(
            table["auth"]["jwt_public_keys"][0].as_str(),
            Some(r#"/a "quoted"\dir/key.pub"#)
        );
    }
}
