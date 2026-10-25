//! Who a Hub request acts for, and what it may do to a repository (design
//! §5.4, rulings M2-R3 and M2-R6, ADR 0010 item 4, ADR 0012).
//!
//! A repository the caller may not read answers exactly as one that doesn't
//! exist ([`HubError::UnknownRepo`]); a caller without a credential gets
//! `401` instead, whatever exists, so neither reveals a repository.

use axum::extract::{FromRequestParts, MatchedPath, RawPathParams};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use openxet_auth::{ANONYMOUS, Action, Credential, Target, UserPrincipal};
use openxet_index::Repo;

use crate::{HubError, HubState, View, names};

/// The user a request acts for: the user credential of its `Authorization:
/// Bearer` header, or [`UserPrincipal::anonymous`] without one, whom only
/// an `anonymous` rule grants anything (ruling M2-R3).
///
/// An invalid or expired credential answers `401` with the text
/// `huggingface_hub` reports as an authentication error, and a CAS-only
/// credential `403`: neither is ever treated as anonymous.
#[derive(Debug, Clone)]
pub struct Who(pub UserPrincipal);

impl Who {
    /// Whether the request carried no credential.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        is_anonymous(&self.0)
    }
}

/// Whether `who` is the principal of a request without a credential. No
/// credential may carry the subject [`ANONYMOUS`] (ruling M2-R3).
pub(crate) fn is_anonymous(who: &UserPrincipal) -> bool {
    who.subject == ANONYMOUS
}

impl FromRequestParts<HubState> for Who {
    type Rejection = HubError;

    async fn from_request_parts(parts: &mut Parts, state: &HubState) -> Result<Self, HubError> {
        let mut values = parts.headers.get_all(AUTHORIZATION).iter();
        let value = match (values.next(), values.next()) {
            (None, _) => return Ok(Self(UserPrincipal::anonymous())),
            (Some(value), None) => value,
            (Some(_), Some(_)) => return Err(HubError::InvalidCredential),
        };
        // The scheme is case-insensitive (RFC 9110 §11.1); anything other
        // than `Bearer`, or a value that isn't visible ASCII, is invalid.
        let token = value
            .to_str()
            .ok()
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim_start_matches(' '))
            .ok_or(HubError::InvalidCredential)?;
        match state.auth.authenticate(token, state.clock.now()) {
            Ok(Credential::User(user) | Credential::Both(_, user)) => Ok(Self(user)),
            Ok(Credential::Cas(_)) => Err(HubError::CasCredential),
            // An empty token is a malformed credential, not none.
            Err(_) => Err(HubError::InvalidCredential),
        }
    }
}

/// A caller allowed to read a repository, and the repository, reached
/// through a [`View`].
///
/// As an extractor it reads the route's `{ns}` and `{name}`, and its view
/// from the typed prefix of its template; [`RepoAccess::resolve`] does the
/// same for a caller that names them otherwise. Either way:
/// - a caller who may not read the repository, or a name that can't exist,
///   gets [`HubError::UnknownRepo`] (or `401` without a credential), the
///   same answer as a repository that doesn't exist;
/// - a repository the caller may read but that lacks the view's label gets
///   [`HubError::WrongView`], whose message says how to add the label.
#[derive(Debug, Clone)]
pub struct RepoAccess {
    /// The repository.
    pub repo: Repo,
    /// Who asked; allowed to read the repository.
    pub who: UserPrincipal,
    /// The view it was reached through, whose label it carries.
    pub view: View,
}

impl RepoAccess {
    /// Resolves `{namespace}/{name}` for `who` through `view`.
    ///
    /// # Errors
    ///
    /// [`HubError::MissingCredential`] for an anonymous caller no rule lets
    /// read it, [`HubError::UnknownRepo`] for any other caller who may not
    /// read it or a repository that doesn't exist, [`HubError::WrongView`]
    /// for one without the view's label, and the index's and authorizer's
    /// failures.
    pub async fn resolve(
        state: &HubState,
        who: UserPrincipal,
        namespace: &str,
        name: &str,
        view: View,
    ) -> Result<Self, HubError> {
        // Authorized before the lookup, by name: a glob ignores ASCII case
        // as names do (ADR 0012 item 7), so the stored spelling decides the
        // same, and an unauthorized caller costs no index read.
        if names::check_name(namespace, name).is_err()
            || !allowed(state, &who, Action::Read, namespace, Some(name)).await?
        {
            return Err(denied(&who));
        }
        let repo = state
            .index
            .repo_by_name(namespace, name)
            .await?
            .ok_or(HubError::UnknownRepo)?;
        if !view.admits(&repo) {
            return Err(HubError::WrongView {
                repo: names::full_name(&repo),
                label: view.label().unwrap_or_default(),
            });
        }
        Ok(Self { repo, who, view })
    }
}

impl FromRequestParts<HubState> for RepoAccess {
    type Rejection = HubError;

    async fn from_request_parts(parts: &mut Parts, state: &HubState) -> Result<Self, HubError> {
        let Who(who) = Who::from_request_parts(parts, state).await?;
        let view = parts
            .extensions
            .get::<MatchedPath>()
            .map(|path| View::of_route(path.as_str()))
            .ok_or_else(|| {
                HubError::Internal("RepoAccess used on a route without a matched path".into())
            })?;
        // A parameter that isn't UTF-8 names no repository that can exist.
        let Ok(params) = RawPathParams::from_request_parts(parts, state).await else {
            return Err(denied(&who));
        };
        let param = |key: &str| {
            params
                .iter()
                .find_map(|(k, v)| (k == key).then(|| v.to_owned()))
        };
        let (Some(namespace), Some(name)) = (param("ns"), param("name")) else {
            return Err(HubError::Internal(
                "RepoAccess used on a route without {ns} and {name}".into(),
            ));
        };
        Self::resolve(state, who, &namespace, &name, view).await
    }
}

/// Checks that `who` may do `action` to `repo`, which they may already
/// read or not: a caller who may not read it gets the unknown-repository
/// answer, so a refusal never reveals it (ruling M2-R6). `Create` is
/// checked on the repository's namespace (ADR 0012 item 6).
///
/// # Errors
///
/// [`HubError::UnknownRepo`] or [`HubError::MissingCredential`] for a
/// caller who may not read `repo`, [`HubError::Forbidden`] for one who may
/// read it but not do `action`, and the authorizer's failures.
pub async fn require(
    state: &HubState,
    who: &UserPrincipal,
    action: Action,
    repo: &Repo,
) -> Result<(), HubError> {
    let verb = match action {
        Action::Read => "read",
        Action::Write => "write to",
        Action::Create => "create repositories beside",
    };
    require_as(state, who, action, repo, verb).await
}

/// [`require`], naming the refused action `verb` in its message.
pub(crate) async fn require_as(
    state: &HubState,
    who: &UserPrincipal,
    action: Action,
    repo: &Repo,
    verb: &'static str,
) -> Result<(), HubError> {
    let name = (action != Action::Create).then_some(repo.name.as_str());
    if allowed(state, who, action, &repo.namespace, name).await? {
        return Ok(());
    }
    if !allowed(state, who, Action::Read, &repo.namespace, Some(&repo.name)).await? {
        return Err(denied(who));
    }
    if is_anonymous(who) {
        return Err(HubError::MissingCredential);
    }
    Err(HubError::Forbidden {
        verb,
        repo: names::full_name(repo),
    })
}

/// Whether `who` may do `action` on `{namespace}/{name}`, or for `Create`
/// in `namespace` (`name` `None`).
pub(crate) async fn allowed(
    state: &HubState,
    who: &UserPrincipal,
    action: Action,
    namespace: &str,
    name: Option<&str>,
) -> Result<bool, HubError> {
    Ok(state
        .authz
        .allows(who, action, Target { namespace, name })
        .await?)
}

/// The answer to a caller refused a repository: `401` without a
/// credential, else exactly the unknown-repository answer (ruling M2-R6).
pub(crate) fn denied(who: &UserPrincipal) -> HubError {
    if is_anonymous(who) {
        HubError::MissingCredential
    } else {
        HubError::UnknownRepo
    }
}
