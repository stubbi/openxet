//! The key report of `openxet config check` (ruling M3-R15, issue #65):
//! every key the server signs or verifies with, by id, `alg` and age, and
//! a warning for each key older than `auth.key_max_age`.
//!
//! A key's age is its file's modification time, so a key copied without
//! keeping it (`cp -p`, `rsync -t`) looks new. Secret keys are named by an
//! id derived from them, never shown.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::config::{Config, Problem};
use crate::{duration, secret, serve};

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// `cas-token.key`: signs the CAS tokens the Hub hands out.
    CasToken,
    /// An `auth.jwt_public_keys` entry: verifies CAS tokens.
    CasTokenVerify,
    /// `cas.transfer_key_file`: signs transfer URLs and capabilities.
    Transfer,
    /// A `cas.transfer_verify_key_files` entry: verifies them, never signs.
    TransferVerify,
}

impl KeyKind {
    /// The name in the report, as serialized.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CasToken => "cas_token",
            Self::CasTokenVerify => "cas_token_verify",
            Self::Transfer => "transfer",
            Self::TransferVerify => "transfer_verify",
        }
    }

    /// How to rotate a key of this kind without refusing a credential
    /// handed out under it.
    const fn rotation(self) -> &'static str {
        match self {
            Self::CasToken => {
                "Rotate it: trust the new key's public half in auth.jwt_public_keys on every \
                 node, then replace cas-token.key with the new key and list this key's public \
                 half instead, and remove that after auth.max_token_lifetime."
            }
            Self::CasTokenVerify => {
                "Rotate it with whoever signs with it: trust the new public key next to this \
                 one on every node, switch the signer, and remove this one after \
                 auth.max_token_lifetime."
            }
            Self::Transfer => {
                "Rotate it: list a new key in cas.transfer_verify_key_files on every node, then \
                 make the new key cas.transfer_key_file and list this one there instead, and \
                 remove this one after cas.url_lifetime."
            }
            Self::TransferVerify => {
                "Remove it from cas.transfer_verify_key_files once cas.url_lifetime has passed \
                 since the transfer key was switched."
            }
        }
    }
}

impl Serialize for KeyKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One key in the `openxet.config-check.v1` report.
#[derive(Debug, Serialize)]
pub struct KeyEntry {
    pub kind: KeyKind,
    /// The `kid` of an Ed25519 key, or the id of a transfer key
    /// ([`openxet_auth::transfer_key_id`]).
    pub id: String,
    /// The JOSE algorithm the key is bound to.
    pub alg: &'static str,
    /// The file holding the key; `None` for a PEM given inline.
    pub file: Option<String>,
    /// Whole days since the file was last modified; `None` without a file.
    pub age_days: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// The keys, and a warning for each one past `auth.key_max_age`.
pub struct KeyReport {
    pub keys: Vec<KeyEntry>,
    pub warnings: Vec<Problem>,
}

/// The JOSE name of HMAC-SHA256 (RFC 7518 §3.1), which transfer keys sign
/// with.
const HS256: &str = "HS256";

/// The report of `config`'s keys at `now`. A key file that is missing or
/// can't be read is left out: `preflight` reports it as a problem.
pub fn report(config: &Config, now: SystemTime) -> KeyReport {
    struct Found {
        kind: KeyKind,
        setting: String,
        id: String,
        alg: &'static str,
        file: Option<PathBuf>,
    }
    let mut found = Vec::new();

    let cas_token = config.data_dir.join(serve::CAS_TOKEN_KEY_FILE);
    if let Ok(Some(signer)) = secret::read_cas_token_key(&cas_token) {
        found.push(Found {
            kind: KeyKind::CasToken,
            setting: "server.data_dir".to_owned(),
            id: signer.kid().to_owned(),
            alg: signer.public_key().alg().as_str(),
            file: Some(cas_token),
        });
    }
    for (index, jwt) in config.jwt_public_keys.iter().enumerate() {
        found.push(Found {
            kind: KeyKind::CasTokenVerify,
            setting: format!("auth.jwt_public_keys[{index}]"),
            id: jwt.key.kid().to_owned(),
            alg: jwt.key.alg().as_str(),
            file: jwt.file.clone(),
        });
    }
    let transfer = std::iter::once((
        KeyKind::Transfer,
        "cas.transfer_key_file".to_owned(),
        &config.transfer_key_file,
    ));
    let verify = config
        .transfer_verify_key_files
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                KeyKind::TransferVerify,
                format!("cas.transfer_verify_key_files[{index}]"),
                path,
            )
        });
    for (kind, setting, path) in transfer.chain(verify) {
        if let Ok(Some(key)) = secret::read_transfer_key(path) {
            found.push(Found {
                kind,
                setting,
                id: openxet_auth::transfer_key_id(&key),
                alg: HS256,
                file: Some(path.clone()),
            });
        }
    }

    let max_age = config.key_max_age;
    let mut warnings = Vec::new();
    let keys = found
        .into_iter()
        .map(|key| {
            let age = key.file.as_deref().and_then(|file| age(file, now));
            let warning = age.filter(|age| *age > max_age).map(|age| {
                format!(
                    "the key {} is {} days old, older than auth.key_max_age ({})",
                    key.id,
                    days(age),
                    duration::format(max_age)
                )
            });
            if let Some(rule) = &warning {
                let value = key
                    .file
                    .as_ref()
                    .filter(|_| !key.setting.starts_with("auth."));
                warnings.push(Problem {
                    key: key.setting,
                    value: value.map(|file| file.display().to_string()),
                    source: "the file system".to_owned(),
                    rule: rule.clone(),
                    hint: key.kind.rotation().to_owned(),
                    quoted: true,
                });
            }
            KeyEntry {
                kind: key.kind,
                id: key.id,
                alg: key.alg,
                file: key.file.map(|f| f.display().to_string()),
                age_days: age.map(days),
                warning,
            }
        })
        .collect();
    KeyReport { keys, warnings }
}

/// The time since `file` was last modified; zero for a time ahead of `now`.
fn age(file: &Path, now: SystemTime) -> Option<Duration> {
    let modified = std::fs::metadata(file).and_then(|m| m.modified()).ok()?;
    Some(now.duration_since(modified).unwrap_or_default())
}

const fn days(age: Duration) -> u64 {
    age.as_secs() / 86_400
}
