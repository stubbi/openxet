//! `openxet label`: a repository's labels through a running server's
//! labels API (ruling M2-R12, ADR 0010 item 3), with the endpoint and
//! token of the client commands.

use std::borrow::Cow;
use std::time::Duration;

use openxet_client::Endpoint;
use openxet_hub::{HubError, MAX_LABELS, check_label, check_name};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CliError;
use crate::output::{HumanDisplay, Output};
use crate::transfer_cmd::{ENDPOINT_VAR, TOKEN_VAR, client_error, flag_or_env, usage_error};

/// How long a request may take to connect, and in all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// How often a change is sent while the server answers `409
/// labels_changed`. Adding and removing are idempotent, so sending again
/// after another writer's change is safe.
const ATTEMPTS: usize = 3;

/// The server and the token, as `upload` and `download` take them.
#[derive(Debug, clap::Args)]
pub struct HubArgs {
    /// The server's endpoint, as `openxet serve` prints it (the Hub API's,
    /// where `hub.public_url` differs) [default: `$OPENXET_ENDPOINT`]
    #[arg(long, value_name = "URL")]
    endpoint: Option<String>,
    /// A user token for the server [default: `$OPENXET_TOKEN`]. Prefer the
    /// variable: a command-line argument is visible to other users of this
    /// machine
    #[arg(long, value_name = "TOKEN")]
    token: Option<String>,
}

#[derive(Debug, clap::Subcommand)]
pub enum LabelCommand {
    /// Add a label to a repository; adding one it carries changes nothing
    Add {
        /// The repository, as `namespace/name`
        #[arg(value_name = "REPO")]
        repo: String,
        /// The label: a lowercase slug of `a`-`z`, `0`-`9` and `-`
        #[arg(value_name = "LABEL")]
        label: String,
        /// Print the labels the repository would carry, and change nothing
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        hub: HubArgs,
    },
    /// Remove a label from a repository; removing one it lacks changes nothing
    Remove {
        /// The repository, as `namespace/name`
        #[arg(value_name = "REPO")]
        repo: String,
        /// The label to remove
        #[arg(value_name = "LABEL")]
        label: String,
        /// Print the labels the repository would carry, and change nothing
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        hub: HubArgs,
    },
    /// List a repository's labels
    List {
        /// The repository, as `namespace/name`
        #[arg(value_name = "REPO")]
        repo: String,
        #[command(flatten)]
        hub: HubArgs,
    },
}

/// The labels API's answer, `openxet.labels.v1`.
#[derive(Deserialize)]
struct Answer {
    repo: String,
    labels: Vec<String>,
}

/// What `openxet label` prints: the labels API's `openxet.labels.v1`,
/// and for `add` and `remove` whether it was a dry run.
#[derive(Debug, Serialize)]
struct Printed {
    schema: &'static str,
    repo: String,
    labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
}

impl HumanDisplay for Printed {
    fn human(&self) -> String {
        let labels = if self.labels.is_empty() {
            "no labels".to_owned()
        } else {
            self.labels.join(", ")
        };
        if self.dry_run == Some(true) {
            format!(
                "Dry run: {} would carry {labels}. Nothing was changed.",
                self.repo
            )
        } else {
            format!("{}: {labels}", self.repo)
        }
    }
}

/// The Hub error body's fields the CLI reads (ruling M2-R6).
#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
    hint: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Add,
    Remove,
}

pub fn run(command: &LabelCommand, output: Output) -> Result<(), CliError> {
    let (repo, hub, change) = match command {
        LabelCommand::Add {
            repo,
            label,
            dry_run,
            hub,
        } => (repo, hub, Some((Change::Add, label, *dry_run))),
        LabelCommand::Remove {
            repo,
            label,
            dry_run,
            hub,
        } => (repo, hub, Some((Change::Remove, label, *dry_run))),
        LabelCommand::List { repo, hub } => (repo, hub, None),
    };
    let (namespace, name) = parse_repo(repo)?;
    if let Some((_, label, _)) = change {
        check_label(label).map_err(|err| invalid_argument("invalid_label", &err))?;
    }
    let client = Client::new(hub, output)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            CliError::new(
                "runtime",
                format!("could not start the async runtime: {err}"),
                "Check the process's thread and file-descriptor limits.",
            )
        })?;
    let labels = runtime.block_on(async {
        let path = [namespace, name];
        match change {
            None => client
                .send(Method::GET, &path, None)
                .await
                .map(|a| answer(a, None)),
            Some((change, label, true)) => {
                let current = client.send(Method::GET, &path, None).await?;
                dry_run(current, change, label)
            }
            Some((change, label, false)) => {
                let method = match change {
                    Change::Add => Method::PUT,
                    Change::Remove => Method::DELETE,
                };
                client
                    .send(method, &path, Some(label))
                    .await
                    .map(|a| answer(a, Some(false)))
            }
        }
    })?;
    output.emit(&labels).map_err(|err| CliError::output(&err))
}

fn answer(answer: Answer, dry_run: Option<bool>) -> Printed {
    Printed {
        schema: "openxet.labels.v1",
        repo: answer.repo,
        labels: answer.labels,
        dry_run,
    }
}

/// The labels `change` would leave, as the server computes them.
fn dry_run(current: Answer, change: Change, label: &str) -> Result<Printed, CliError> {
    let mut labels = current.labels;
    let present = labels.iter().any(|l| l == label);
    match change {
        Change::Add if !present => {
            if labels.len() >= MAX_LABELS {
                return Err(CliError::new(
                    "invalid_label",
                    format!("{} already carries {MAX_LABELS} labels", current.repo),
                    "A repository carries at most 32 labels (ADR 0010); remove one first with \
                     `openxet label remove`.",
                ));
            }
            labels.push(label.to_owned());
        }
        Change::Remove => labels.retain(|l| l != label),
        Change::Add => {}
    }
    Ok(answer(
        Answer {
            repo: current.repo,
            labels,
        },
        Some(true),
    ))
}

/// `namespace/name`, checked as the server checks it (ruling M2-R2).
fn parse_repo(repo: &str) -> Result<(&str, &str), CliError> {
    let Some((namespace, name)) = repo.split_once('/') else {
        return Err(usage_error(
            "invalid_repo",
            &format!("{repo:?} is not a repository: expected `namespace/name`"),
            "Pass the repository as `namespace/name`, such as `alice/my-model`.",
        ));
    };
    check_name(namespace, name).map_err(|err| invalid_argument("invalid_repo", &err))?;
    Ok((namespace, name))
}

/// A name or label the server would refuse, refused before any request
/// with the server's message and hint.
fn invalid_argument(code: &'static str, err: &HubError) -> CliError {
    let message = err.to_string();
    let hint = match err {
        HubError::InvalidName { hint, .. } | HubError::InvalidLabel { hint, .. } => *hint,
        _ => "Run `openxet label --help` for the forms it takes.",
    };
    usage_error(code, &message, hint)
}

struct Client {
    http: reqwest::Client,
    endpoint: Endpoint,
}

impl Client {
    fn new(hub: &HubArgs, output: Output) -> Result<Self, CliError> {
        let url = flag_or_env(hub.endpoint.as_ref(), ENDPOINT_VAR).ok_or_else(|| {
            usage_error(
                "missing_endpoint",
                "no endpoint: neither --endpoint nor OPENXET_ENDPOINT is set",
                "Set OPENXET_ENDPOINT to the endpoint `openxet serve` prints, for example \
                 `export OPENXET_ENDPOINT=http://127.0.0.1:8080`.",
            )
        })?;
        let token = flag_or_env(hub.token.as_ref(), TOKEN_VAR).ok_or_else(|| {
            usage_error(
                "missing_token",
                "no token: neither --token nor OPENXET_TOKEN is set",
                "Set OPENXET_TOKEN to a user token for this server: a dev server prints one on \
                 startup (and keeps it in <data dir>/dev-token); otherwise ask the operator.",
            )
        })?;
        let endpoint = Endpoint::new(&url, token).map_err(|err| client_error(&err))?;
        // With --json, stderr carries nothing but an error object.
        if output == Output::Human && endpoint.token_in_clear() {
            eprintln!(
                "warning: the token is sent unencrypted to {}; use an https:// endpoint unless \
                 every network on the way is trusted",
                endpoint.url
            );
        }
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| {
                CliError::new(
                    "runtime",
                    format!("could not start the HTTP client: {err}"),
                    "Check the system's TLS and proxy configuration.",
                )
            })?;
        Ok(Self { http, endpoint })
    }

    /// `…/api/openxet/v1/repos/{ns}/{name}/labels[/{label}]`, each segment
    /// percent-encoded.
    fn url(&self, repo: &[&str; 2], label: Option<&str>) -> Result<Url, CliError> {
        let mut url = Url::parse(&self.endpoint.url).map_err(|_| {
            usage_error(
                "invalid_endpoint",
                "the endpoint is not a URL",
                "Set OPENXET_ENDPOINT (or --endpoint) to the endpoint `openxet serve` prints, \
                 such as http://127.0.0.1:8080.",
            )
        })?;
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .extend(["api", "openxet", "v1", "repos", repo[0], repo[1], "labels"])
                .extend(label);
        }
        Ok(url)
    }

    async fn send(
        &self,
        method: Method,
        repo: &[&str; 2],
        label: Option<&str>,
    ) -> Result<Answer, CliError> {
        let url = self.url(repo, label)?;
        let mut attempt = 1;
        loop {
            let response = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&self.endpoint.token)
                .send()
                .await
                .map_err(|err| self.unreachable(&err))?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|err| self.unreachable(&err))?;
            if status.is_success() {
                return serde_json::from_slice(&body).map_err(|_| {
                    CliError::new(
                        "server_error",
                        format!(
                            "{} answered {status} without an openxet.labels.v1 body",
                            self.endpoint.url
                        ),
                        "Check that the endpoint is an openxet server's, as `openxet serve` \
                         prints it.",
                    )
                });
            }
            let error = server_error(status, &body, &repo.join("/"));
            if error.code == "labels_changed" && attempt < ATTEMPTS {
                attempt += 1;
                continue;
            }
            return Err(error);
        }
    }

    fn unreachable(&self, err: &reqwest::Error) -> CliError {
        // reqwest's message names the URL, which holds no credential; the
        // token travels in a header only.
        let reason = if err.is_timeout() {
            "it didn't answer in time".to_owned()
        } else if err.is_connect() {
            "nothing accepted the connection".to_owned()
        } else {
            err.to_string()
        };
        CliError::new(
            "unreachable_endpoint",
            format!("could not reach {}: {reason}", self.endpoint.url),
            "Check that the server is running and that OPENXET_ENDPOINT (or --endpoint) is the \
             endpoint it printed on startup.",
        )
    }
}

/// The CLI's error for the server's answer: its stable code where the CLI
/// documents one, with a hint for this command, else the server's own
/// message and hint.
fn server_error(status: StatusCode, body: &[u8], repo: &str) -> CliError {
    let Ok(error) = serde_json::from_slice::<ErrorBody>(body) else {
        return CliError::new(
            "server_error",
            format!("the server answered {status}"),
            "Check that the endpoint is an openxet server's. If it is, retry; if it keeps \
             failing, give the operator the time of this run: the server log records the cause.",
        );
    };
    let message = printable(&error.message);
    let server_hint: Cow<'static, str> = error
        .hint
        .as_deref()
        .map_or(Cow::Borrowed("Follow the server's message."), |hint| {
            Cow::Owned(printable(hint))
        });
    let (code, hint): (&'static str, Cow<'static, str>) = match error.code.as_str() {
        "repo_not_found" => ("repo_not_found", server_hint),
        "missing_credential" | "invalid_credential" => (
            "unauthorized",
            Cow::Borrowed(
                "Check that OPENXET_TOKEN is a user token for this server, not revoked or \
                 expired. A dev server prints its token on startup and keeps it in \
                 <data dir>/dev-token.",
            ),
        ),
        "cas_credential" => (
            "forbidden",
            Cow::Borrowed(
                "This is a CAS token, which only the CAS routes take; `openxet label` needs a \
                 user token, such as a static token without `scope` or the dev token.",
            ),
        ),
        "forbidden" => (
            "forbidden",
            Cow::Owned(format!(
                "Changing labels needs `write` on {repo}; ask the operator for an \
                 [[auth.rules]] entry granting it."
            )),
        ),
        "invalid_label" => ("invalid_label", server_hint),
        "invalid_name" => ("invalid_repo", server_hint),
        "labels_changed" => ("labels_changed", server_hint),
        _ => ("server_error", server_hint),
    };
    CliError::new(code, message, hint)
}

/// `text` without control characters, so a server's message stays on one
/// line of the terminal.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn server_errors_keep_documented_codes_and_carry_a_hint() {
        for (code, expected) in [
            ("repo_not_found", "repo_not_found"),
            ("missing_credential", "unauthorized"),
            ("invalid_credential", "unauthorized"),
            ("cas_credential", "forbidden"),
            ("forbidden", "forbidden"),
            ("invalid_label", "invalid_label"),
            ("invalid_name", "invalid_repo"),
            ("labels_changed", "labels_changed"),
            ("unavailable", "server_error"),
        ] {
            let body = serde_json::json!({
                "error": "m", "schema": "openxet.error.v1", "code": code,
                "message": "line\nbreak", "hint": "do\tthis",
            });
            let err = server_error(StatusCode::BAD_REQUEST, body.to_string().as_bytes(), "a/b");
            assert_eq!(err.code, expected, "{code}");
            assert_eq!(err.message, "linebreak");
            assert!(!err.hint.is_empty() && !err.hint.contains('\t'), "{err:?}");
        }
        let err = server_error(StatusCode::BAD_GATEWAY, b"<html>", "a/b");
        assert_eq!(err.code, "server_error");
        assert!(err.message.contains("502"), "{err:?}");
    }

    #[test]
    fn a_dry_run_computes_what_the_server_would() {
        let current = || Answer {
            repo: "a/b".to_owned(),
            labels: vec!["model".to_owned()],
        };
        let labels = |change, label| dry_run(current(), change, label).unwrap().labels;
        assert_eq!(labels(Change::Add, "eval"), ["model", "eval"]);
        assert_eq!(labels(Change::Add, "model"), ["model"]);
        assert_eq!(labels(Change::Remove, "model"), Vec::<String>::new());
        assert_eq!(labels(Change::Remove, "eval"), ["model"]);

        let full = Answer {
            repo: "a/b".to_owned(),
            labels: (0..MAX_LABELS).map(|i| format!("l{i}")).collect(),
        };
        assert_eq!(
            dry_run(full, Change::Add, "one-more").unwrap_err().code,
            "invalid_label"
        );
    }

    #[test]
    fn label_urls_encode_each_segment() {
        let client = Client {
            http: reqwest::Client::new(),
            endpoint: Endpoint::new("http://h:1/base/", "t".into()).unwrap(),
        };
        assert_eq!(
            client.url(&["a", "b.c"], Some("x-y")).unwrap().as_str(),
            "http://h:1/base/api/openxet/v1/repos/a/b.c/labels/x-y"
        );
        assert_eq!(
            client.url(&["a", "b"], None).unwrap().as_str(),
            "http://h:1/base/api/openxet/v1/repos/a/b/labels"
        );
    }
}
