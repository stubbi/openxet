//! The Hub's security tests of design §5.9 (`docs/reference/security-tests.md`):
//! creation authorized per namespace, a repository the caller may not read
//! answering exactly as an unknown one on every route (ruling M2-R6), no
//! credential echoed, anonymous reads only by explicit rule (ruling
//! M2-R3), and OIDC tokens authorized by the rules as static user tokens
//! are and never logged (ruling M3-R7).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use axum::http::header::AUTHORIZATION;
use axum::http::{Method, Request, StatusCode};
use openxet_auth::test_support::{TestAlg, TestIssuer};
use openxet_auth::{Action, CasGrant, Glob, Mode, Rule, StaticToken};
use openxet_hub::test_support::{ALL, BEARER, Reply, TestHub, rule, user_token};
use serde_json::{Value, json};

const ALICE: &str = "oxt_test_alice";
const BOB: &str = "oxt_test_bob";
const CAROL: &str = "oxt_test_carol";
const CAS_ONLY: &str = "oxt_test_cas_only";
/// A static user token for `corp:dave`, the subject the OIDC issuer `corp`
/// gives its user `dave`.
const STATIC_DAVE: &str = "oxt_test_static_dave";

/// The unknown-repository body of ruling M2-R6, byte for byte.
const UNKNOWN_BODY: &str = r#"{"error":"Repository not found","schema":"openxet.error.v1","code":"repo_not_found","message":"Repository not found","hint":"Check the repository id and the token's access: a repository the token may not read answers exactly like one that doesn't exist."}"#;

/// The same answer on the Git smart HTTP routes, as `text/plain`.
const GIT_UNKNOWN_BODY: &str = "Repository not found\nCheck the repository id and the token's \
     access: a repository the token may not read answers exactly like one that doesn't exist.\n";

/// Alice owns `alice/*`, Bob `bob/*`; Carol may create in `alice` but not
/// read there.
async fn hub() -> TestHub {
    let cas_only = StaticToken {
        sha256: StaticToken::digest(CAS_ONLY),
        subject: "ci".to_owned(),
        cas: Some(CasGrant {
            scope: "default".to_owned(),
            mode: Mode::Write,
            tenant: None,
        }),
        user: None,
    };
    let hub = TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token(BOB, "bob", &[]),
            user_token(CAROL, "carol", &[]),
            cas_only,
        ],
        vec![
            rule(&["alice"], &["alice/*"], ALL),
            rule(&["bob"], &["bob/*"], ALL),
            rule(&["carol"], &["alice/*"], &[Action::Create]),
        ],
    )
    .await;
    hub.create(ALICE, "alice", "m", None).await;
    hub
}

fn assert_unknown(reply: &Reply, what: &str) {
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "{what}: {}",
        reply.text()
    );
    // The Git routes answer `text/plain`, which git shows its user; the
    // API routes JSON (ruling M2-R6 as amended). Each is one constant.
    let git = ["/info/refs", "/git-upload-pack", "/git-receive-pack"]
        .iter()
        .any(|route| what.contains(route));
    let body = if git { GIT_UNKNOWN_BODY } else { UNKNOWN_BODY };
    assert_eq!(reply.text(), body, "{what}");
    assert_eq!(reply.header("x-error-code"), Some("RepoNotFound"), "{what}");
    assert_eq!(
        reply.header("x-error-message"),
        Some("Repository not found"),
        "{what}"
    );
}

#[tokio::test]
async fn repo_create_authorized_per_namespace() {
    let mut claims = BTreeMap::new();
    claims.insert("orgs".to_owned(), vec!["acme".to_owned()]);
    let hub = TestHub::start(
        vec![
            user_token(ALICE, "alice", &["acme"]),
            user_token(BOB, "bob", &[]),
        ],
        vec![
            rule(&["alice"], &["alice/*"], ALL),
            // A pattern naming one repository grants no `create` (ADR 0012
            // item 7).
            rule(&["bob"], &["bob/only", "b*/x"], ALL),
            Rule {
                subjects: vec![],
                claims,
                repos: vec![Glob::new("acme/*").unwrap()],
                actions: vec![Action::Create, Action::Read],
            },
        ],
    )
    .await;
    let create = |token: Option<&'static str>, namespace: &'static str| {
        let hub = &hub;
        async move {
            hub.call(
                Method::POST,
                "/api/repos/create",
                token,
                Some(&json!({"name": "x", "organization": namespace})),
            )
            .await
        }
    };
    assert_eq!(create(Some(ALICE), "alice").await.status, StatusCode::OK);
    assert_eq!(create(Some(ALICE), "acme").await.status, StatusCode::OK);
    let refused = create(Some(ALICE), "bob").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(
        refused.header("x-error-message"),
        Some("You don't have the rights to create a model under the namespace \"bob\"")
    );
    for namespace in ["bob", "acme"] {
        assert_eq!(
            create(Some(BOB), namespace).await.status,
            StatusCode::FORBIDDEN,
            "{namespace}"
        );
    }
    let anonymous = create(None, "alice").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    // A CAS-only credential is no user, so it creates nothing.
    let hub = self::hub().await;
    let reply = hub
        .call(
            Method::POST,
            "/api/repos/create",
            Some(CAS_ONLY),
            Some(&json!({"name": "y", "organization": "alice"})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["code"], "cas_credential");
}

#[tokio::test]
async fn wrong_type_without_read_access_matches_unknown_repo() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    for view in ["models", "spaces", "kernels", "datasets"] {
        let existing = hub
            .call(
                Method::GET,
                &format!("/api/{view}/alice/d"),
                Some(BOB),
                None,
            )
            .await;
        let missing = hub
            .call(
                Method::GET,
                &format!("/api/{view}/alice/nope"),
                Some(BOB),
                None,
            )
            .await;
        assert_unknown(&existing, view);
        assert_eq!(existing.comparable(), missing.comparable(), "{view}");
    }
    // The owner, who may read it, gets the label hint instead.
    let owner = hub
        .call(Method::GET, "/api/models/alice/d", Some(ALICE), None)
        .await;
    assert_eq!(owner.status, StatusCode::NOT_FOUND);
    assert_ne!(owner.text(), UNKNOWN_BODY);
}

#[tokio::test]
async fn create_collision_authorized_first() {
    let hub = hub().await;
    let create = |token: &'static str, name: &'static str| {
        let hub = &hub;
        async move {
            hub.call(
                Method::POST,
                "/api/repos/create",
                Some(token),
                Some(&json!({"name": name, "organization": "alice"})),
            )
            .await
        }
    };
    // Bob may not create in `alice`: the same 403 whether the name is taken
    // or free, so the 403 reveals nothing.
    let taken = create(BOB, "m").await;
    let free = create(BOB, "free").await;
    assert_eq!(taken.status, StatusCode::FORBIDDEN);
    assert_eq!(taken.comparable(), free.comparable());
    // Carol may create there but not read: her 409 names no labels.
    let carol = create(CAROL, "M").await;
    assert_eq!(carol.status, StatusCode::CONFLICT);
    let error = carol.json()["error"].as_str().unwrap().to_owned();
    assert_eq!(error, "Repository alice/M already exists");
    assert_eq!(carol.header("x-error-message"), Some(error.as_str()));
    // Alice, who may read it, sees them.
    let alice = create(ALICE, "M").await;
    assert!(alice.json()["error"].as_str().unwrap().contains("[model]"));
}

/// A concrete path for `template`: `{ns}` and `{name}` filled with
/// `namespace` and `name`, any other parameter with a plausible value.
fn concrete(template: &str, namespace: &str, name: &str) -> String {
    template
        .split('/')
        .map(|segment| match segment {
            "{ns}" => namespace.to_owned(),
            "{name}" => name.to_owned(),
            s if s.starts_with('{') => "main".to_owned(),
            s => s.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Hub routes that address no repository by path, each with why it can't
/// reveal one. Every other route has `{ns}` and `{name}`.
const NOT_REPOSITORY_ROUTES: &[(&str, &str)] = &[
    ("/api/whoami-v2", "answers only the caller's own identity"),
    (
        "/api/repos/create",
        "authorized per namespace first; create_collision_authorized_first",
    ),
    (
        "/api/repos/delete",
        "addresses the repository in its body; checked below",
    ),
    ("/api/models", "lists only what the caller may read"),
    ("/api/datasets", "lists only what the caller may read"),
    ("/api/spaces", "lists only what the caller may read"),
    ("/api/kernels", "lists only what the caller may read"),
    ("/api/validate-yaml", "reads no repository"),
    ("/api/agent-harnesses", "501, whatever exists"),
    ("/api/collections", "501, whatever exists"),
    ("/api/collections/{*rest}", "501, whatever exists"),
    ("/api/users/{user}/likes", "501, whatever exists"),
    ("/api/models-tags-by-type", "501, whatever exists"),
    ("/api/datasets-tags-by-type", "501, whatever exists"),
    (
        "/v1/transfer/files/{repo}/{file}",
        "addresses the repository by id; gateway_needs_credential_scoped_to_repo",
    ),
    (
        "/v1/capabilities/{repo}/{op}",
        "the signature is the credential (ADR 0003 decision 2); a bad one is refused before \
         any lookup; lfs.rs",
    ),
];

/// Asserts that `method template` answers `token`, whose holder may not
/// read `alice/m`, exactly as for a repository that doesn't exist or
/// can't, and a caller without a credential the same `401` either way.
async fn assert_route_hides_repository(
    hub: &TestHub,
    token: &str,
    method: &Method,
    template: &str,
) {
    let existing = hub
        .call(
            method.clone(),
            &concrete(template, "alice", "m"),
            Some(token),
            None,
        )
        .await;
    let missing = hub
        .call(
            method.clone(),
            &concrete(template, "alice", "gone"),
            Some(token),
            None,
        )
        .await;
    let impossible = hub
        .call(
            method.clone(),
            &concrete(template, "alice", "a..b"),
            Some(token),
            None,
        )
        .await;
    let what = format!("{method} {template}");
    assert_unknown(&existing, &what);
    assert_eq!(existing.comparable(), missing.comparable(), "{what}");
    assert_eq!(existing.comparable(), impossible.comparable(), "{what}");
    // Without a credential: the same 401 whatever exists.
    let anonymous = hub
        .call(
            method.clone(),
            &concrete(template, "alice", "m"),
            None,
            None,
        )
        .await;
    let anonymous_missing = hub
        .call(
            method.clone(),
            &concrete(template, "alice", "gone"),
            None,
            None,
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "{what}");
    assert_eq!(
        anonymous.comparable(),
        anonymous_missing.comparable(),
        "{what}"
    );
}

#[tokio::test]
async fn unreadable_repo_matches_unknown_repo_on_every_route() {
    let hub = hub().await;
    // Carrying every Hub label, so no route's view could refuse it first.
    for label in ["dataset", "space", "kernel"] {
        let reply = hub
            .call(
                Method::PUT,
                &format!("/api/openxet/v1/repos/alice/m/labels/{label}"),
                Some(ALICE),
                None,
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK);
    }
    let endpoints = openxet_hub::endpoints();
    for (template, _) in NOT_REPOSITORY_ROUTES {
        assert!(
            endpoints.iter().any(|e| e.path == *template),
            "{template} is listed but not served"
        );
    }
    let mut checked = 0;
    for endpoint in &endpoints {
        let (method, template) = (&endpoint.method, endpoint.path);
        let by_path = template.contains("/{ns}/{name}");
        let exempt = NOT_REPOSITORY_ROUTES.iter().any(|(t, _)| *t == template);
        assert!(
            by_path || exempt,
            "{method} {template} addresses no repository by path; add it to \
             NOT_REPOSITORY_ROUTES with why it can't reveal one"
        );
        if !by_path {
            continue;
        }
        assert_route_hides_repository(&hub, BOB, method, template).await;
        checked += 1;
    }
    assert!(checked >= 12, "only {checked} repository routes");

    // Delete names the repository in its body.
    let delete = |name: &'static str| {
        let hub = &hub;
        async move {
            hub.call(
                Method::DELETE,
                "/api/repos/delete",
                Some(BOB),
                Some(&json!({"name": name, "organization": "alice"})),
            )
            .await
        }
    };
    let existing = delete("m").await;
    assert_unknown(&existing, "delete");
    assert_eq!(existing.comparable(), delete("gone").await.comparable());
    // And it is still there.
    let info = hub
        .call(Method::GET, "/api/models/alice/m", Some(ALICE), None)
        .await;
    assert_eq!(info.status, StatusCode::OK);
}

#[tokio::test]
async fn responses_never_echo_credential() {
    let idp = TestIssuer::start().await;
    let hub = oidc_hub(&idp).await;
    let unknown = "oxt_test_never_configured_0123456789";
    let oidc = idp.sign(TestAlg::Rs256, &dave(&idp, hub.state.clock.now()));
    let tokens = [ALICE, BOB, CAROL, CAS_ONLY, STATIC_DAVE, unknown, &oidc];
    let mut targets: Vec<(Method, String)> = openxet_hub::endpoints()
        .iter()
        .map(|e| (e.method.clone(), concrete(e.path, "alice", "m")))
        .collect();
    targets.push((Method::GET, "/api/models/alice/gone".to_owned()));
    for (method, path) in &targets {
        for token in tokens {
            for auth in [
                format!("{BEARER} {token}"),
                format!("Basic {token}"),
                token.to_owned(),
            ] {
                let request = Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header(AUTHORIZATION, auth)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"name": "x", "organization": "alice", "content": ""}).to_string(),
                    ))
                    .unwrap();
                let reply = hub.send(request).await;
                assert!(!reply.text().contains(token), "{method} {path} body");
                for (name, value) in &reply.headers {
                    assert!(
                        !String::from_utf8_lossy(value.as_bytes()).contains(token),
                        "{method} {path} header {name}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn anonymous_reads_only_with_rule() {
    let rules = |public: bool| {
        let mut rules = vec![rule(&["alice"], &["*"], ALL)];
        if public {
            rules.push(rule(&["anonymous"], &["pub/*"], &[Action::Read]));
        }
        rules
    };
    let private = TestHub::start(vec![user_token(ALICE, "alice", &[])], rules(false)).await;
    let public = TestHub::start(vec![user_token(ALICE, "alice", &[])], rules(true)).await;
    for hub in [&private, &public] {
        hub.create(ALICE, "pub", "x", None).await;
        hub.create(ALICE, "priv", "y", None).await;
    }

    // No rule names `anonymous`: nothing is readable without a credential.
    let reply = private
        .call(Method::GET, "/api/models/pub/x", None, None)
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        reply.header("www-authenticate"),
        Some(r#"Bearer realm="openxet""#)
    );
    let listing = private.call(Method::GET, "/api/models", None, None).await;
    assert_eq!(listing.json(), json!([]));

    // `anonymous` may read `pub/*`: exactly that, and only reading.
    let reply = public
        .call(Method::GET, "/api/models/pub/x", None, None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["id"], "pub/x");
    let labels = public
        .call(
            Method::GET,
            "/api/openxet/v1/repos/pub/x/labels",
            None,
            None,
        )
        .await;
    assert_eq!(labels.status, StatusCode::OK);
    let hidden = public
        .call(Method::GET, "/api/models/priv/y", None, None)
        .await;
    let absent = public
        .call(Method::GET, "/api/models/priv/nope", None, None)
        .await;
    assert_eq!(hidden.status, StatusCode::UNAUTHORIZED);
    assert_eq!(hidden.comparable(), absent.comparable());
    let write = public
        .call(
            Method::PUT,
            "/api/openxet/v1/repos/pub/x/labels/new",
            None,
            None,
        )
        .await;
    assert_eq!(write.status, StatusCode::UNAUTHORIZED);
    let listing = public.call(Method::GET, "/api/models", None, None).await;
    assert_eq!(listing.json()[0]["id"], "pub/x");
    assert_eq!(listing.json().as_array().unwrap().len(), 1);
    // A public repository is readable with any credential too.
    let reply = public
        .call(Method::GET, "/api/models/pub/x", Some(ALICE), None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
}

/// [`hub`], also accepting OIDC tokens from `idp` as the issuer `corp`,
/// with its `orgs` claim mapped (ruling M3-R7). `corp:dave` may do
/// everything in `dave/*`, through `STATIC_DAVE` or an OIDC token alike,
/// and members of the org `acme` may read `alice/*`.
async fn oidc_hub(idp: &TestIssuer) -> TestHub {
    let cas_only = StaticToken {
        sha256: StaticToken::digest(CAS_ONLY),
        subject: "ci".to_owned(),
        cas: Some(CasGrant {
            scope: "default".to_owned(),
            mode: Mode::Write,
            tenant: None,
        }),
        user: None,
    };
    let acme = Rule {
        subjects: vec![],
        claims: BTreeMap::from([("orgs".to_owned(), vec!["acme".to_owned()])]),
        repos: vec![Glob::new("alice/*").unwrap()],
        actions: vec![Action::Read],
    };
    let hub = TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token(BOB, "bob", &[]),
            user_token(CAROL, "carol", &[]),
            user_token(STATIC_DAVE, "corp:dave", &[]),
            cas_only,
        ],
        vec![
            rule(&["alice"], &["alice/*"], ALL),
            rule(&["bob"], &["bob/*"], ALL),
            rule(&["carol"], &["alice/*"], &[Action::Create]),
            rule(&["corp:dave"], &["dave/*"], ALL),
            acme,
        ],
    )
    .await;
    hub.create(ALICE, "alice", "m", None).await;
    let oidc = idp
        .discover(|cfg| cfg.claims = vec!["orgs".to_owned()])
        .await;
    let mut state = hub.state.clone();
    state.auth = Arc::new((*state.auth).clone().with_oidc(vec![oidc]));
    TestHub::with_state(hub.dir, state)
}

/// Claims for `corp`'s user `dave` at `now`.
fn dave(idp: &TestIssuer, now: u64) -> Value {
    let mut claims = idp.claims("dave", now);
    claims["preferred_username"] = "Dave".into();
    claims
}

#[tokio::test]
async fn oidc_token_scoped_like_static_user_token() {
    // Design §5.9 req 2 for OIDC (ruling M3-R7): the rules decide what an
    // OIDC token may do exactly as for a static user token with its
    // subject `{name}:{sub}` and claims; nothing about OIDC widens it.
    let idp = TestIssuer::start().await;
    let hub = oidc_hub(&idp).await;
    let now = hub.state.clock.now();
    let token = |claims: &Value| idp.sign(TestAlg::Es256, claims);

    // Eve of `corp` has no rule: every repository route answers her as for
    // a repository that doesn't exist.
    let eve = token(&idp.claims("eve", now));
    for endpoint in openxet_hub::endpoints() {
        if endpoint.path.contains("/{ns}/{name}") {
            assert_route_hides_repository(&hub, &eve, &endpoint.method, endpoint.path).await;
        }
    }

    // Dave through OIDC and through his static token get the same answers.
    let oidc_dave = token(&dave(&idp, now));
    hub.create(&oidc_dave, "dave", "x", None).await;
    for path in [
        "/api/models/dave/x",
        "/api/openxet/v1/repos/dave/x/labels",
        "/api/models/alice/m",
        "/api/models/bob/none",
    ] {
        let by_oidc = hub.call(Method::GET, path, Some(&oidc_dave), None).await;
        let by_static = hub.call(Method::GET, path, Some(STATIC_DAVE), None).await;
        assert_eq!(by_oidc.comparable(), by_static.comparable(), "{path}");
    }
    for namespace in ["alice", "bob"] {
        let body = json!({"name": "y", "organization": namespace});
        let by_oidc = hub
            .call(
                Method::POST,
                "/api/repos/create",
                Some(&oidc_dave),
                Some(&body),
            )
            .await;
        let by_static = hub
            .call(
                Method::POST,
                "/api/repos/create",
                Some(STATIC_DAVE),
                Some(&body),
            )
            .await;
        assert_eq!(by_oidc.status, StatusCode::FORBIDDEN, "{namespace}");
        assert_eq!(by_oidc.comparable(), by_static.comparable(), "{namespace}");
    }
    // The Hub mints CAS tokens for an OIDC user as for any user.
    let read_token = hub
        .call(
            Method::GET,
            "/api/models/dave/x/xet-read-token/main",
            Some(&oidc_dave),
            None,
        )
        .await;
    assert_eq!(read_token.status, StatusCode::OK, "{}", read_token.text());
    assert!(read_token.json()["accessToken"].is_string());

    // A mapped claim matches claim rules: `acme` members read `alice/*`,
    // and only read.
    let mut member = idp.claims("frank", now);
    member["orgs"] = json!(["acme"]);
    let member = token(&member);
    let read = hub
        .call(Method::GET, "/api/models/alice/m", Some(&member), None)
        .await;
    assert_eq!(read.status, StatusCode::OK);
    let write = hub
        .call(
            Method::PUT,
            "/api/openxet/v1/repos/alice/m/labels/new",
            Some(&member),
            None,
        )
        .await;
    assert_eq!(write.status, StatusCode::FORBIDDEN, "{}", write.text());
    let whoami = hub
        .call(Method::GET, "/api/whoami-v2", Some(&member), None)
        .await;
    assert_eq!(whoami.json()["name"], "corp:frank");
    assert_eq!(whoami.json()["orgs"][0]["name"], "acme");

    // While the issuer is down, a token under a key the cache lacks can't
    // be verified: `503 authn_unavailable`, never an accept and never a
    // `401` that would make a client drop a valid token.
    idp.set_up(false);
    idp.rotate(TestAlg::Rs256);
    let unverifiable = idp.sign(TestAlg::Rs256, &dave(&idp, now));
    for path in ["/api/models/dave/x", "/api/whoami-v2"] {
        let reply = hub.call(Method::GET, path, Some(&unverifiable), None).await;
        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(reply.json()["code"], "authn_unavailable", "{path}");
        assert_eq!(reply.header("retry-after"), Some("1"), "{path}");
    }
    // Keys already cached keep verifying meanwhile.
    let reply = hub
        .call(Method::GET, "/api/models/dave/x", Some(&oidc_dave), None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
}

/// Every log line written on this thread, at every level.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn oidc_token_never_logged() {
    // Design §5.9 req 6 for OIDC, the Hub half: OIDC tokens, accepted,
    // expired, forged or unverifiable, as `Bearer` and as a Basic
    // password, on every Hub route, never reach the log, whole or as any
    // of their parts. The test's runtime runs on this one thread, so the
    // capture sees everything the Hub logs, at `TRACE`.
    let logs = Logs::default();
    let writer = logs.clone();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let idp = TestIssuer::start().await;
    let hub = oidc_hub(&idp).await;
    let now = hub.state.clock.now();
    let mut expired = dave(&idp, now - 7200);
    expired["exp"] = (now - 3600).into();
    let mut tokens = vec![
        idp.sign(TestAlg::Rs256, &dave(&idp, now)),
        idp.sign(TestAlg::EdDsa, &expired),
        idp.hs256_with_public_key(&dave(&idp, now)),
        idp.unsigned(&dave(&idp, now)),
    ];
    let send_all = |token: String| {
        let hub = &hub;
        async move {
            for endpoint in openxet_hub::endpoints() {
                let path = concrete(endpoint.path, "dave", "x");
                for auth in [
                    format!("{BEARER} {token}"),
                    format!("Basic {}", base64_password(&token)),
                ] {
                    let request = Request::builder()
                        .method(endpoint.method.clone())
                        .uri(&path)
                        .header(AUTHORIZATION, auth)
                        .body(axum::body::Body::empty())
                        .unwrap();
                    hub.send(request).await;
                }
            }
        }
    };
    for token in &tokens {
        send_all(token.clone()).await;
    }
    idp.set_up(false);
    idp.rotate(TestAlg::Es256);
    let unverifiable = idp.sign(TestAlg::Es256, &dave(&idp, now));
    send_all(unverifiable.clone()).await;
    tokens.push(unverifiable);

    let logs = String::from_utf8_lossy(&logs.0.lock().unwrap_or_else(PoisonError::into_inner))
        .into_owned();
    assert!(
        logs.contains("could not refresh the OIDC issuer's JWKS"),
        "the failed refresh is logged: {logs}"
    );
    for token in &tokens {
        for part in token.split('.').filter(|p| p.len() > 8) {
            assert!(!logs.contains(part), "a token part is in the log");
        }
        assert!(!logs.contains(&base64_password(token)));
    }
}

/// HTTP Basic credentials with `token` as the password, as git sends them.
fn base64_password(token: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(format!("git:{token}"))
}
