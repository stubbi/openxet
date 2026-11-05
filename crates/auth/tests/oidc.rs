//! OIDC authentication (design §5.4, §5.9; ruling M3-R7; ADR 0016 item 1),
//! against the in-process issuer of `openxet_auth::test_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_auth::test_support::{AUDIENCE, TestAlg, TestIssuer};
use openxet_auth::{
    AuthError, Authenticator, Credential, Mode, OidcConfig, OidcIssuer, TokenSigner,
    UNKNOWN_KID_REFRESH_INTERVAL, UserPrincipal, generate_keypair,
};
use serde_json::{Value, json};

const ALGS: [TestAlg; 3] = [TestAlg::Rs256, TestAlg::Es256, TestAlg::EdDsa];

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// `cfg` for `issuer` under the name `corp`, changed by `change`.
fn config(issuer: &TestIssuer, change: impl FnOnce(&mut OidcConfig)) -> OidcConfig {
    let mut cfg = OidcConfig::new("corp", issuer.issuer(), &[AUDIENCE]);
    change(&mut cfg);
    cfg
}

async fn discover(issuer: &TestIssuer, change: impl FnOnce(&mut OidcConfig)) -> OidcIssuer {
    OidcIssuer::discover(config(issuer, change), reqwest::Client::new())
        .await
        .expect("discovery")
}

/// `claims` with `key` set to `value`, or removed for `Value::Null`.
fn with(mut claims: Value, key: &str, value: Value) -> Value {
    if value.is_null() {
        claims.as_object_mut().unwrap().remove(key);
    } else {
        claims[key] = value;
    }
    claims
}

#[tokio::test(flavor = "multi_thread")]
async fn oidc_token_becomes_user_principal() {
    let idp = TestIssuer::start().await;
    let oidc = Arc::new(
        discover(&idp, |cfg| {
            cfg.claims = vec!["orgs".to_owned(), "groups".to_owned()];
        })
        .await,
    );
    let signer = TokenSigner::from_pem(generate_keypair().unwrap().private_pem()).unwrap();
    let auth = Authenticator::new(vec![], vec![signer.public_key()], Duration::from_hours(1))
        .unwrap()
        .with_oidc(vec![Arc::clone(&oidc)]);
    let t = now();
    let mut claims = idp.claims("u-42", t);
    claims["preferred_username"] = "alice".into();
    claims["email"] = "alice@example.com".into();
    claims["email_verified"] = true.into();
    claims["orgs"] = json!(["acme", "labs"]);
    claims["groups"] = "ml-team".into();
    claims["ignored"] = json!(["not", "mapped"]);
    let expected = UserPrincipal {
        subject: "corp:u-42".to_owned(),
        name: "alice".to_owned(),
        email: Some("alice@example.com".to_owned()),
        claims: BTreeMap::from([
            ("groups".to_owned(), vec!["ml-team".to_owned()]),
            (
                "orgs".to_owned(),
                vec!["acme".to_owned(), "labs".to_owned()],
            ),
        ]),
    };
    for alg in ALGS {
        let token = idp.sign(alg, &claims);
        assert_eq!(
            oidc.verify(&token, t).await,
            Ok(expected.clone()),
            "{alg:?}"
        );
        // Through the authenticator, an OIDC token is a user credential
        // only, never a CAS one.
        assert_eq!(
            auth.authenticate(&token, t).await,
            Ok(Credential::User(expected.clone())),
            "{alg:?}"
        );
    }

    // Without a name claim the name is the subject; a value that could
    // forge a log line is dropped, never copied.
    let mut bare = idp.claims("u-7", t);
    bare["orgs"] = json!(["ok", "bad\nline"]);
    bare["email"] = "evil\u{202e}@example.com".into();
    let user = oidc
        .verify(&idp.sign(TestAlg::EdDsa, &bare), t)
        .await
        .unwrap();
    assert_eq!(user.name, "corp:u-7");
    assert_eq!(user.email, None);
    assert_eq!(user.claims["orgs"], ["ok"]);

    // Hub commits are authored with the email (ruling M2-R7), so it is
    // taken only when the issuer says it verified it: `email_verified`
    // exactly `true`.
    for verified in [Value::Null, json!(false), json!("true"), json!(1)] {
        let mut unverified = with(claims.clone(), "email_verified", verified.clone());
        unverified["sub"] = "u-9".into();
        let user = oidc
            .verify(&idp.sign(TestAlg::Es256, &unverified), t)
            .await
            .unwrap();
        assert_eq!(user.email, None, "{verified}");
        assert_eq!(user.name, "alice", "{verified}");
    }

    // A subject that isn't a string, or that carries control characters,
    // makes no principal.
    for sub in [json!(42), json!("a\u{0}b"), json!(" ")] {
        let token = idp.sign(
            TestAlg::Rs256,
            &with(idp.claims("x", t), "sub", sub.clone()),
        );
        assert_eq!(
            oidc.verify(&token, t).await,
            Err(AuthError::Invalid),
            "{sub}"
        );
    }

    // CAS tokens still verify beside OIDC.
    let cas = signer.sign("ci", "default", Mode::Read, t + 60, t).unwrap();
    assert!(matches!(
        auth.authenticate(&cas, t).await,
        Ok(Credential::Cas(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_issuer_or_audience_refused() {
    let idp = TestIssuer::start().await;
    let oidc = discover(&idp, |_| {}).await;
    let t = now();
    let ok = idp.claims("alice", t);
    let refused = [
        with(ok.clone(), "iss", "https://elsewhere.example.com".into()),
        with(ok.clone(), "iss", format!("{}/", idp.issuer()).into()),
        with(ok.clone(), "iss", Value::Null),
        with(ok.clone(), "aud", "someone-else".into()),
        with(ok.clone(), "aud", json!(["someone-else", "another"])),
        with(ok.clone(), "aud", json!([])),
        with(ok.clone(), "aud", Value::Null),
    ];
    for claims in refused {
        let token = idp.sign(TestAlg::Rs256, &claims);
        assert_eq!(
            oidc.verify(&token, t).await,
            Err(AuthError::Invalid),
            "{claims}"
        );
    }
    // One configured audience among several is enough.
    let token = idp.sign(TestAlg::Rs256, &with(ok, "aud", json!(["other", AUDIENCE])));
    assert!(oidc.verify(&token, t).await.is_ok());

    // An issuer configured other than its discovery document names it is
    // refused at startup, naming the issuer and what to set.
    let err = OidcIssuer::discover(
        OidcConfig::new("corp", &format!("{}/", idp.issuer()), &[AUDIENCE]),
        reqwest::Client::new(),
    )
    .await
    .unwrap_err();
    let AuthError::Oidc { name, reason } = err else {
        panic!("{err:?}")
    };
    assert_eq!(name, "corp");
    assert!(
        reason.contains("exactly what the provider's tokens carry in `iss`"),
        "{reason}"
    );
    // So is one that can't be reached.
    idp.set_up(false);
    let err = OidcIssuer::discover(config(&idp, |_| {}), reqwest::Client::new())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("503"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn alg_none_and_hs256_refused() {
    let idp = TestIssuer::start().await;
    let oidc = discover(&idp, |_| {}).await;
    let t = now();
    let claims = idp.claims("alice", t);
    assert_eq!(
        oidc.verify(&idp.unsigned(&claims), t).await,
        Err(AuthError::Invalid)
    );
    assert_eq!(
        oidc.verify(&idp.hs256_with_public_key(&claims), t).await,
        Err(AuthError::Invalid)
    );
    // A critical header OpenXet can't honour is refused too (RFC 7515
    // §4.1.11).
    let header = json!({"alg": "RS256", "kid": idp.kid(TestAlg::Rs256), "crit": ["exp"], "exp": 1});
    let token = idp.sign_with_header(TestAlg::Rs256, &header, &claims);
    assert_eq!(oidc.verify(&token, t).await, Err(AuthError::Invalid));
    // Without a `kid` no key is chosen.
    let header = json!({"alg": "RS256", "typ": "JWT"});
    let token = idp.sign_with_header(TestAlg::Rs256, &header, &claims);
    assert_eq!(oidc.verify(&token, t).await, Err(AuthError::Invalid));
}

#[tokio::test(flavor = "multi_thread")]
async fn alg_must_match_jwks_key() {
    let idp = TestIssuer::start().await;
    let oidc = discover(&idp, |_| {}).await;
    let t = now();
    let claims = idp.claims("alice", t);
    // Each header names another algorithm's key; each token is signed with
    // the key of the algorithm it claims.
    for (alg, other) in [
        (TestAlg::Es256, TestAlg::Rs256),
        (TestAlg::Rs256, TestAlg::EdDsa),
        (TestAlg::EdDsa, TestAlg::Es256),
    ] {
        let header = json!({"alg": alg.name(), "typ": "JWT", "kid": idp.kid(other)});
        let token = idp.sign_with_header(alg, &header, &claims);
        assert_eq!(
            oidc.verify(&token, t).await,
            Err(AuthError::Invalid),
            "{alg:?}"
        );
    }
    // The right algorithm with another key's signature fails too.
    let header = json!({"alg": "ES256", "typ": "JWT", "kid": idp.kid(TestAlg::Es256)});
    let forged = idp.sign_with_header(TestAlg::Es256, &header, &claims);
    let mut parts: Vec<&str> = forged.split('.').collect();
    let other = idp.sign(TestAlg::Es256, &idp.claims("mallory", t));
    parts[2] = other.split('.').nth(2).unwrap();
    assert_eq!(
        oidc.verify(&parts.join("."), t).await,
        Err(AuthError::Invalid)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exp_exact_nbf_iat_skew() {
    let idp = TestIssuer::start().await;
    let oidc = discover(&idp, |_| {}).await;
    let t = now();
    let skew = openxet_auth::MAX_CLOCK_SKEW.as_secs();
    let verify = |claims: Value| {
        let token = idp.sign(TestAlg::Rs256, &claims);
        let oidc = &oidc;
        async move { oidc.verify(&token, t).await.map(|_| ()) }
    };
    let base = idp.claims("alice", t - 60);
    // `exp` is exact: valid before it, expired from it on, with no leeway.
    assert_eq!(
        verify(with(base.clone(), "exp", (t + 1).into())).await,
        Ok(())
    );
    assert_eq!(
        verify(with(base.clone(), "exp", t.into())).await,
        Err(AuthError::Expired)
    );
    assert_eq!(
        verify(with(base.clone(), "exp", Value::Null)).await,
        Err(AuthError::Invalid)
    );
    assert_eq!(
        verify(with(base.clone(), "exp", "soon".into())).await,
        Err(AuthError::Invalid)
    );
    // `nbf` and `iat` may be up to the clock skew ahead, and no further.
    for claim in ["nbf", "iat"] {
        assert_eq!(
            verify(with(base.clone(), claim, (t + skew).into())).await,
            Ok(()),
            "{claim}"
        );
        assert_eq!(
            verify(with(base.clone(), claim, (t + skew + 1).into())).await,
            Err(AuthError::Invalid),
            "{claim}"
        );
    }
    assert_eq!(verify(with(base, "iat", Value::Null)).await, Ok(()));
}

#[tokio::test(flavor = "multi_thread")]
async fn jwks_unknown_kid_refreshes_once() {
    let idp = TestIssuer::start().await;
    let oidc = Arc::new(discover(&idp, |_| {}).await);
    assert_eq!(idp.jwks_fetches(), 1);
    let t = now();
    // The provider rotates its RS256 key: tokens under the new `kid` are
    // accepted after one refresh, however many arrive at once.
    idp.rotate(TestAlg::Rs256);
    let token = idp.sign(TestAlg::Rs256, &idp.claims("alice", t));
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let (oidc, token) = (Arc::clone(&oidc), token.clone());
            tokio::spawn(async move { oidc.verify(&token, t).await })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap().subject, "corp:alice");
    }
    assert_eq!(idp.jwks_fetches(), 2);
    // The new key is cached from then on.
    oidc.verify(&token, t + 1).await.unwrap();
    assert_eq!(idp.jwks_fetches(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn jwks_refresh_rate_limited() {
    let idp = TestIssuer::start().await;
    let oidc = discover(&idp, |_| {}).await;
    let t = now();
    let interval = UNKNOWN_KID_REFRESH_INTERVAL.as_secs();
    let made_up = |kid: &str, at: u64| {
        let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": kid});
        idp.sign_with_header(TestAlg::EdDsa, &header, &idp.claims("alice", at))
    };
    // A made-up `kid` costs one fetch, then none until the interval passes.
    for (i, at) in [t, t + 1, t + interval - 1].into_iter().enumerate() {
        let token = made_up(&format!("nope-{i}"), at);
        assert_eq!(oidc.verify(&token, at).await, Err(AuthError::Invalid));
        assert_eq!(idp.jwks_fetches(), 2, "at +{}", at - t);
    }
    let token = made_up("nope-again", t + interval);
    assert_eq!(
        oidc.verify(&token, t + interval).await,
        Err(AuthError::Invalid)
    );
    assert_eq!(idp.jwks_fetches(), 3);
    // A real rotation inside the interval waits for it to pass.
    idp.rotate(TestAlg::EdDsa);
    let rotated = idp.sign(TestAlg::EdDsa, &idp.claims("alice", t + interval));
    assert_eq!(
        oidc.verify(&rotated, t + interval + 1).await,
        Err(AuthError::Invalid)
    );
    assert_eq!(idp.jwks_fetches(), 3);
    assert!(oidc.verify(&rotated, t + 2 * interval).await.is_ok());
    assert_eq!(idp.jwks_fetches(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn jwks_outage_serves_cached_then_fails_closed() {
    let idp = TestIssuer::start().await;
    let oidc = discover(&idp, |_| {}).await;
    let t = now();
    let max_stale = OidcConfig::DEFAULT_JWKS_MAX_STALE.as_secs();
    idp.set_up(false);
    // Down: cached keys keep verifying.
    let cached = |at: u64| idp.sign(TestAlg::Es256, &idp.claims("alice", at));
    assert!(oidc.verify(&cached(t + 60), t + 60).await.is_ok());
    // A key the cache lacks can't be fetched: 503, never 401 and never an
    // accept.
    idp.rotate(TestAlg::Rs256);
    let rotated = idp.sign(TestAlg::Rs256, &idp.claims("alice", t + 60));
    assert_eq!(
        oidc.verify(&rotated, t + 60).await,
        Err(AuthError::Unavailable)
    );
    // Still inside `jwks_max_stale`, the cached keys verify.
    let late = t + max_stale - 120;
    assert!(oidc.verify(&cached(late), late).await.is_ok());
    // Past it, every token is refused with 503 until a refresh succeeds.
    let past = t + max_stale + 1;
    assert_eq!(
        oidc.verify(&cached(past), past).await,
        Err(AuthError::Unavailable)
    );
    idp.set_up(true);
    let back = past + UNKNOWN_KID_REFRESH_INTERVAL.as_secs();
    assert!(oidc.verify(&cached(back), back).await.is_ok());
    let rotated = idp.sign(TestAlg::Rs256, &idp.claims("alice", back));
    assert!(oidc.verify(&rotated, back).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn two_issuers_never_share_a_subject() {
    let (a, b) = (TestIssuer::start().await, TestIssuer::start().await);
    let oidc_a = Arc::new(discover(&a, |cfg| cfg.name = "a".to_owned()).await);
    let oidc_b = Arc::new(discover(&b, |cfg| cfg.name = "b".to_owned()).await);
    let auth = Authenticator::new(vec![], vec![], Duration::from_hours(1))
        .unwrap()
        .with_oidc(vec![oidc_a, oidc_b]);
    let t = now();
    let subject = |token: String| {
        let auth = &auth;
        async move {
            match auth.authenticate(&token, t).await {
                Ok(Credential::User(user)) => Ok(user.subject),
                other => Err(other),
            }
        }
    };
    let from_a = subject(a.sign(TestAlg::Rs256, &a.claims("alice", t))).await;
    let from_b = subject(b.sign(TestAlg::Rs256, &b.claims("alice", t))).await;
    assert_eq!(from_a, Ok("a:alice".to_owned()));
    assert_eq!(from_b, Ok("b:alice".to_owned()));
    // B can't speak for A: a token naming A as issuer but signed by B's
    // key (under a `kid` A also uses) is refused.
    for alg in ALGS {
        let forged = b.sign(alg, &a.claims("alice", t));
        assert_eq!(
            subject(forged).await,
            Err(Err(AuthError::Invalid)),
            "{alg:?}"
        );
    }
}

/// One change that makes an issuer config invalid.
type BreakConfig = fn(&mut OidcConfig);

#[test]
fn issuer_config_is_checked() {
    let ok = OidcConfig::new("corp", "https://id.example.com/realms/x", &["openxet"]);
    assert_eq!(ok.check(), Ok(()));
    let cases: [(&str, BreakConfig); 7] = [
        ("delegated", |c| c.name = "delegated".to_owned()),
        ("colon", |c| c.name = "a:b".to_owned()),
        ("http", |c| c.issuer = "http://id.example.com".to_owned()),
        ("query", |c| {
            c.issuer = "https://id.example.com/?x=1".to_owned();
        }),
        ("no audience", |c| c.audiences.clear()),
        ("empty claim", |c| c.claims = vec![String::new()]),
        ("stale", |c| c.jwks_max_stale = Duration::from_mins(1)),
    ];
    for (what, change) in cases {
        let mut cfg = ok.clone();
        change(&mut cfg);
        assert!(cfg.check().is_err(), "{what}");
    }
}

/// Discovery and JWKS fetches never follow a redirect, which could lead to
/// `http://` on any host past the `https`-or-loopback check: discovery
/// fails, and a refresh fails closed, keeping the last keys, whether the
/// client follows redirects or not.
#[tokio::test(flavor = "multi_thread")]
async fn issuer_redirect_refused() {
    let idp = TestIssuer::start().await;
    let elsewhere = TestIssuer::start().await;
    let no_redirects = || {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    };
    for target in ["http://keys.example.invalid", elsewhere.issuer()] {
        idp.redirect_to(Some(target));
        let err = OidcIssuer::discover(config(&idp, |_| {}), no_redirects())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("redirect"), "{target}: {err}");
        // A following client reaches the target, or fails to, and either
        // way discovery fails.
        let followed = OidcIssuer::discover(config(&idp, |_| {}), reqwest::Client::new()).await;
        assert!(followed.is_err(), "{target}");
    }

    idp.redirect_to(None);
    let t = now();
    for http in [no_redirects(), reqwest::Client::new()] {
        let oidc = OidcIssuer::discover(config(&idp, |_| {}), http)
            .await
            .unwrap();
        // The JWKS now redirects to keys `idp` never published, under a
        // `kid` the cache lacks; following it would accept their tokens.
        idp.redirect_to(Some(elsewhere.issuer()));
        elsewhere.rotate(TestAlg::EdDsa);
        let foreign = elsewhere.sign(TestAlg::EdDsa, &idp.claims("mallory", t));
        assert_eq!(oidc.verify(&foreign, t).await, Err(AuthError::Unavailable));
        // The last keys still verify.
        let own = idp.sign(TestAlg::Es256, &idp.claims("alice", t));
        assert_eq!(oidc.verify(&own, t).await.unwrap().subject, "corp:alice");
        idp.redirect_to(None);
    }
}

/// A JWKS without a key a token could be verified with is refused at
/// discovery, and on a refresh it is a failed refresh: the last keys stay.
#[tokio::test(flavor = "multi_thread")]
async fn jwks_without_keys_keeps_the_last_keys() {
    let idp = TestIssuer::start().await;
    idp.serve_empty_jwks(true);
    let err = OidcIssuer::discover(config(&idp, |_| {}), reqwest::Client::new())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("holds no signing key"), "{err}");

    idp.serve_empty_jwks(false);
    let oidc = discover(&idp, |_| {}).await;
    let fetched = idp.jwks_fetches();
    let t = now();
    idp.serve_empty_jwks(true);
    // An unknown `kid` refreshes, gets no keys, and fails closed.
    idp.rotate(TestAlg::Rs256);
    let rotated = |at: u64| idp.sign(TestAlg::Rs256, &idp.claims("alice", at));
    assert_eq!(
        oidc.verify(&rotated(t), t).await,
        Err(AuthError::Unavailable)
    );
    assert_eq!(idp.jwks_fetches(), fetched + 1);
    // The keys from before the refresh still verify.
    let cached = idp.sign(TestAlg::Es256, &idp.claims("alice", t));
    assert!(oidc.verify(&cached, t).await.is_ok());
    // Once the provider serves keys again, the next refresh picks them up.
    idp.serve_empty_jwks(false);
    let later = t + UNKNOWN_KID_REFRESH_INTERVAL.as_secs();
    assert!(oidc.verify(&rotated(later), later).await.is_ok());
}
