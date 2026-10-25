//! The config-rules authorizer: subjects or claims, repository globs and
//! actions (design §5.4, ruling M2-R3, ADR 0012).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use openxet_auth::{
    ANONYMOUS, Action, Authorizer as _, Glob, Rule, RulesAuthorizer, Target, UserPrincipal,
};

fn user(subject: &str) -> UserPrincipal {
    UserPrincipal {
        subject: subject.to_owned(),
        name: subject.to_owned(),
        email: None,
        claims: BTreeMap::new(),
    }
}

fn claims(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(k, vs)| {
            (
                (*k).to_owned(),
                vs.iter().map(|v| (*v).to_owned()).collect(),
            )
        })
        .collect()
}

fn globs(patterns: &[&str]) -> Vec<Glob> {
    patterns.iter().map(|p| Glob::new(p).unwrap()).collect()
}

fn subject_rule(subjects: &[&str], repos: &[&str], actions: &[Action]) -> Rule {
    Rule {
        subjects: subjects.iter().map(|s| (*s).to_owned()).collect(),
        claims: BTreeMap::new(),
        repos: globs(repos),
        actions: actions.to_vec(),
    }
}

fn repo<'a>(namespace: &'a str, name: &'a str) -> Target<'a> {
    Target {
        namespace,
        name: Some(name),
    }
}

fn namespace(namespace: &str) -> Target<'_> {
    Target {
        namespace,
        name: None,
    }
}

async fn allows(
    rules: &RulesAuthorizer,
    who: &UserPrincipal,
    action: Action,
    t: Target<'_>,
) -> bool {
    rules.allows(who, action, t).await.unwrap()
}

#[tokio::test]
async fn subject_rule_allows_exact_repo() {
    let rules = RulesAuthorizer::new(vec![subject_rule(
        &["alice"],
        &["acme/model"],
        &[Action::Read],
    )]);
    let alice = user("alice");
    assert!(allows(&rules, &alice, Action::Read, repo("acme", "model")).await);
    assert!(!allows(&rules, &alice, Action::Write, repo("acme", "model")).await);
    assert!(!allows(&rules, &alice, Action::Read, repo("acme", "other")).await);
    assert!(!allows(&rules, &user("bob"), Action::Read, repo("acme", "model")).await);
    // Repository names are unique ignoring ASCII case (M2-R2), so a rule
    // matches every spelling of the one repository it names.
    assert!(allows(&rules, &alice, Action::Read, repo("ACME", "Model")).await);
}

#[tokio::test]
async fn write_implies_read() {
    let rules = RulesAuthorizer::new(vec![subject_rule(
        &["alice"],
        &["acme/*"],
        &[Action::Write],
    )]);
    let alice = user("alice");
    assert!(allows(&rules, &alice, Action::Read, repo("acme", "model")).await);
    assert!(allows(&rules, &alice, Action::Write, repo("acme", "model")).await);
    assert!(!allows(&rules, &alice, Action::Create, namespace("acme")).await);
}

#[tokio::test]
async fn glob_rule_allows_namespace_and_create() {
    let rules = RulesAuthorizer::new(vec![subject_rule(
        &["alice"],
        &["org/*"],
        &[Action::Read, Action::Create],
    )]);
    let alice = user("alice");
    assert!(allows(&rules, &alice, Action::Create, namespace("org")).await);
    assert!(!allows(&rules, &alice, Action::Create, namespace("other")).await);
    assert!(allows(&rules, &alice, Action::Read, repo("org", "anything")).await);
    assert!(!allows(&rules, &alice, Action::Read, repo("other", "anything")).await);

    // Creation is decided per namespace, so only a glob covering every name
    // in it grants it.
    let narrow = RulesAuthorizer::new(vec![subject_rule(
        &["alice"],
        &["org/m*"],
        &[Action::Create],
    )]);
    assert!(!allows(&narrow, &alice, Action::Create, namespace("org")).await);
    let everything =
        RulesAuthorizer::new(vec![subject_rule(&["alice"], &["*"], &[Action::Create])]);
    assert!(allows(&everything, &alice, Action::Create, namespace("any")).await);
}

#[tokio::test]
async fn claims_rule_matches_any_listed_value() {
    let rules = RulesAuthorizer::new(vec![Rule {
        subjects: vec![],
        claims: claims(&[("groups", &["ml-team", "research"])]),
        repos: globs(&["org/*"]),
        actions: vec![Action::Write],
    }]);
    let mut carol = user("carol");
    carol.claims = claims(&[("groups", &["sales", "research"])]);
    assert!(allows(&rules, &carol, Action::Write, repo("org", "m")).await);

    let mut dave = user("dave");
    dave.claims = claims(&[("groups", &["sales"]), ("orgs", &["ml-team"])]);
    assert!(
        !allows(&rules, &dave, Action::Write, repo("org", "m")).await,
        "a value under another claim name does not match"
    );
    assert!(!allows(&rules, &user("erin"), Action::Read, repo("org", "m")).await);
}

#[tokio::test]
async fn no_rule_denies() {
    let rules = RulesAuthorizer::new(vec![]);
    for action in [Action::Read, Action::Write] {
        assert!(!allows(&rules, &user("alice"), action, repo("a", "b")).await);
        assert!(!allows(&rules, &UserPrincipal::anonymous(), action, repo("a", "b")).await);
    }
    assert!(!allows(&rules, &user("alice"), Action::Create, namespace("a")).await);
}

#[tokio::test]
async fn anonymous_only_with_explicit_rule() {
    let anonymous = UserPrincipal::anonymous();
    assert_eq!(anonymous.subject, ANONYMOUS);
    assert!(anonymous.claims.is_empty());

    let everyone_else = RulesAuthorizer::new(vec![subject_rule(&["*"], &["*"], &[Action::Read])]);
    assert!(
        !allows(
            &everyone_else,
            &anonymous,
            Action::Read,
            repo("org", "public")
        )
        .await,
        "subjects are matched literally: `*` is no subject wildcard"
    );

    let public = RulesAuthorizer::new(vec![subject_rule(
        &[ANONYMOUS],
        &["org/public"],
        &[Action::Read],
    )]);
    assert!(allows(&public, &anonymous, Action::Read, repo("org", "public")).await);
    assert!(!allows(&public, &anonymous, Action::Write, repo("org", "public")).await);
    assert!(!allows(&public, &anonymous, Action::Read, repo("org", "private")).await);
    // What a caller without a credential may do, every authenticated caller
    // may do too: a public repository is public to everyone.
    assert!(allows(&public, &user("bob"), Action::Read, repo("org", "public")).await);
}

#[test]
fn glob_never_crosses_the_slash() {
    let matches =
        |pattern: &str, ns: &str, name: &str| Glob::new(pattern).unwrap().matches(ns, name);
    assert!(matches("*", "org", "x"));
    assert!(matches("*", "a", "b"));
    assert!(matches("org/*", "org", "x"));
    assert!(!matches("org/*", "org", "a/b"));
    assert!(!matches("org/*", "orgx", "x"));
    assert!(matches("o*/x", "org", "x"));
    assert!(!matches("o*/x", "org", "y"));
    assert!(!matches("*/x", "org/a", "x"));
    assert!(matches("org/*-v*", "org", "llama-v2"));
    assert!(!matches("org/*-v*", "org", "llama"));
    assert!(matches("org/a*b*c", "org", "abc"));
    assert!(matches("org/a*b*c", "org", "aXbYbZc"));
    assert!(!matches("org/a*b*c", "org", "aXbYcZ"));

    for bad in [
        "", "org", "org/", "/x", "org/x/y", "org/**", "**", "o rg/x", "org/x\n", "org/..",
    ] {
        assert!(Glob::new(bad).is_err(), "{bad:?} must be refused");
    }
    assert_eq!(Glob::new("org/*").unwrap().as_str(), "org/*");
    assert_eq!(Glob::any(), Glob::new("*").unwrap());
}

#[test]
fn actions_parse_by_name() {
    for action in [Action::Read, Action::Write, Action::Create] {
        assert_eq!(Action::parse(action.name()), Some(action));
    }
    assert_eq!(Action::parse("admin"), None);
    assert_eq!(Action::parse("Read"), None);
}
