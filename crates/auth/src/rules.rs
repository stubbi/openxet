//! The authorization seam and the config-rules authorizer (design §5.4,
//! ruling M2-R3, ADR 0012).
//!
//! Rules only grant: a request is allowed if any rule allows it, and denied
//! when none does. There is no rule order and no deny rule, so the effect
//! of a set of rules is the union of what each says.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;

use crate::{ANONYMOUS, AuthzError, InvalidGlob, UserPrincipal};

/// What a request does to a repository or namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Read the repository: its files, refs and history.
    Read,
    /// Change the repository: commit, push, change refs or labels. Implies
    /// `Read`.
    Write,
    /// Create a repository in a namespace.
    Create,
}

impl Action {
    /// The action named `name` in configuration: `read`, `write` or
    /// `create`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "create" => Some(Self::Create),
            _ => None,
        }
    }

    /// The action's name in configuration.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Create => "create",
        }
    }

    /// Whether a grant of `self` allows `wanted`.
    fn grants(self, wanted: Self) -> bool {
        self == wanted || (self == Self::Write && wanted == Self::Read)
    }
}

/// What an action applies to: a repository, or for [`Action::Create`] a
/// namespace (ADR 0010 item 5: creation is authorized before the name is
/// looked up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target<'a> {
    /// The namespace, the repository id's first segment.
    pub namespace: &'a str,
    /// The repository's name; `None` for [`Action::Create`].
    pub name: Option<&'a str>,
}

/// Decides what a user may do per repository or namespace (design §5.4).
///
/// M2 ships [`RulesAuthorizer`]; M3's webhook authorizer implements the same
/// trait.
#[async_trait]
pub trait Authorizer: Send + Sync + 'static {
    /// Whether `who` may do `action` on `target`.
    ///
    /// # Errors
    ///
    /// [`AuthzError`] if the authorizer can't decide; the request then
    /// fails rather than being allowed or denied.
    async fn allows(
        &self,
        who: &UserPrincipal,
        action: Action,
        target: Target<'_>,
    ) -> Result<bool, AuthzError>;
}

/// The longest glob segment: as long as the longest repository namespace or
/// name (ruling M2-R2).
const MAX_SEGMENT: usize = 96;

/// A repository pattern: `*`, matching every repository, or
/// `{namespace}/{name}` where `*` in either segment matches any run of
/// characters other than `/`. `**` is not supported: a repository id has
/// exactly one slash (ruling M2-R2).
///
/// Matching ignores ASCII case, since `{namespace}/{name}` is unique
/// ignoring case (ruling M2-R2): a rule naming a repository covers every
/// spelling of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Glob {
    pattern: String,
}

impl Glob {
    /// Parses `pattern`.
    ///
    /// # Errors
    ///
    /// [`InvalidGlob`] unless `pattern` is `*` or two non-empty segments
    /// joined by one `/`, each at most 96 characters of letters, digits,
    /// `.`, `_`, `-` and `*`, without `**` or `..`.
    pub fn new(pattern: &str) -> Result<Self, InvalidGlob> {
        let invalid = |reason| Err(InvalidGlob { reason });
        if pattern == "*" {
            return Ok(Self {
                pattern: pattern.to_owned(),
            });
        }
        let Some((namespace, name)) = pattern.split_once('/') else {
            return invalid("must be `*` or `{namespace}/{name}`, such as `org/*`");
        };
        for segment in [namespace, name] {
            if segment.is_empty() {
                return invalid(
                    "has an empty namespace or name; write `org/*` for every name in `org`",
                );
            }
            if segment.len() > MAX_SEGMENT {
                return invalid("has a namespace or name longer than 96 characters");
            }
            if !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'*'))
            {
                return invalid(
                    "may hold only letters, digits, `.`, `_`, `-`, `*` and one `/`, as repository ids do",
                );
            }
            if segment.contains("**") {
                return invalid(
                    "uses `**`, which is not supported: `*` already matches a whole namespace or name",
                );
            }
            if segment.contains("..") {
                return invalid("contains `..`, which no repository id does");
            }
        }
        Ok(Self {
            pattern: pattern.to_owned(),
        })
    }

    /// The pattern as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Whether the repository `{namespace}/{name}` matches.
    #[must_use]
    pub fn matches(&self, namespace: &str, name: &str) -> bool {
        match self.pattern.split_once('/') {
            None => true,
            Some((ns, n)) => wildcard(ns, namespace) && wildcard(n, name),
        }
    }

    /// Whether every repository in `namespace` matches: the test for
    /// [`Action::Create`], decided before any name is known.
    fn covers_namespace(&self, namespace: &str) -> bool {
        match self.pattern.split_once('/') {
            None => true,
            Some((ns, n)) => n == "*" && wildcard(ns, namespace),
        }
    }

    fn covers(&self, target: Target<'_>) -> bool {
        match target.name {
            Some(name) => self.matches(target.namespace, name),
            None => self.covers_namespace(target.namespace),
        }
    }
}

impl fmt::Debug for Glob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Glob").field(&self.pattern).finish()
    }
}

/// Whether `text` matches `pattern`, a segment in which `*` matches any run
/// of characters other than `/`, ignoring ASCII case. Backtracks only to
/// the last `*`, so it is `O(pattern × text)` at worst.
fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut resume: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            resume = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi].eq_ignore_ascii_case(&t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((star, from)) = resume.filter(|&(_, from)| t[from] != b'/') {
            pi = star + 1;
            ti = from + 1;
            resume = Some((star, from + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&b| b == b'*')
}

/// One `[[auth.rules]]` entry: who it applies to, which repositories, and
/// what it allows there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Subjects the rule applies to, matched exactly. [`ANONYMOUS`] applies
    /// it to everyone, with or without a credential.
    pub subjects: Vec<String>,
    /// Claims the rule applies to: a principal matches if, under any of
    /// these names, it holds any of the listed values.
    pub claims: BTreeMap<String, Vec<String>>,
    /// The repositories the rule covers.
    pub repos: Vec<Glob>,
    /// What the rule allows on them; `write` implies `read`.
    pub actions: Vec<Action>,
}

impl Rule {
    fn applies_to(&self, who: &UserPrincipal) -> bool {
        let subject = self
            .subjects
            .iter()
            .any(|s| s == ANONYMOUS || *s == who.subject);
        subject
            || self.claims.iter().any(|(name, wanted)| {
                who.claims
                    .get(name)
                    .is_some_and(|held| held.iter().any(|value| wanted.contains(value)))
            })
    }

    fn allows(&self, who: &UserPrincipal, action: Action, target: Target<'_>) -> bool {
        self.actions.iter().any(|granted| granted.grants(action))
            && self.repos.iter().any(|glob| glob.covers(target))
            && self.applies_to(who)
    }
}

/// The config-rules authorizer: allows a request if any rule applies to the
/// principal's subject or claims, covers the repository and allows the
/// action (ruling M2-R3).
///
/// A rule naming [`ANONYMOUS`] grants everyone, so a public repository is
/// readable with any credential as well as without one.
#[derive(Debug, Clone, Default)]
pub struct RulesAuthorizer(Vec<Rule>);

impl RulesAuthorizer {
    /// An authorizer applying `rules`.
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        Self(rules)
    }

    /// The rules, in configuration order.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.0
    }
}

#[async_trait]
impl Authorizer for RulesAuthorizer {
    async fn allows(
        &self,
        who: &UserPrincipal,
        action: Action,
        target: Target<'_>,
    ) -> Result<bool, AuthzError> {
        Ok(self.0.iter().any(|rule| rule.allows(who, action, target)))
    }
}
