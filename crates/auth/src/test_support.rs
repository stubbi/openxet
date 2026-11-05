//! An in-process OIDC issuer for tests ([`TestIssuer`]): it serves a
//! discovery document and a JWKS over loopback HTTP, signs RS256, ES256 and
//! `EdDSA` tokens with keys generated per run, and can rotate a key, go down
//! and count the JWKS fetches it answered. Only with the `test-support`
//! feature.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

use std::sync::{Arc, Mutex, PoisonError};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair as _, RSA_PKCS1_SHA256,
    RsaKeyPair, RsaPublicKeyComponents,
};
use axum::Router;
use axum::extract::State as Shared;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{KeyInit as _, Mac as _};
use serde_json::{Value, json};

use crate::{OidcConfig, OidcIssuer};

/// The audience every [`TestIssuer::claims`] token is issued for.
pub const AUDIENCE: &str = "openxet";

/// A signing algorithm the issuer has a key for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestAlg {
    /// `RSASSA-PKCS1-v1_5` with SHA-256, a 2048-bit key.
    Rs256,
    /// ECDSA on P-256 with SHA-256.
    Es256,
    /// Ed25519.
    EdDsa,
}

impl TestAlg {
    /// The JOSE `alg` name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Es256 => "ES256",
            Self::EdDsa => "EdDSA",
        }
    }
}

enum Signer {
    Rsa(RsaKeyPair),
    Ec(EcdsaKeyPair),
    Ed(Ed25519KeyPair),
}

struct Key {
    kid: String,
    alg: TestAlg,
    signer: Signer,
    jwk: Value,
}

impl Key {
    fn generate(alg: TestAlg, kid: String) -> Self {
        let b64 = |bytes: &[u8]| URL_SAFE_NO_PAD.encode(bytes);
        let (signer, jwk) = match alg {
            TestAlg::Rs256 => {
                let key = RsaKeyPair::generate(KeySize::Rsa2048).unwrap();
                let public = RsaPublicKeyComponents::<Vec<u8>>::from(key.public_key());
                let jwk = json!({"kty": "RSA", "n": b64(&public.n), "e": b64(&public.e)});
                (Signer::Rsa(key), jwk)
            }
            TestAlg::Es256 => {
                let key = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
                // An uncompressed point: 0x04, then x and y (SEC 1 §2.3.3).
                let point = key.public_key().as_ref();
                let jwk = json!({
                    "kty": "EC", "crv": "P-256",
                    "x": b64(&point[1..33]), "y": b64(&point[33..65]),
                });
                (Signer::Ec(key), jwk)
            }
            TestAlg::EdDsa => {
                let key = Ed25519KeyPair::generate().unwrap();
                let jwk =
                    json!({"kty": "OKP", "crv": "Ed25519", "x": b64(key.public_key().as_ref())});
                (Signer::Ed(key), jwk)
            }
        };
        let mut jwk = jwk;
        jwk["kid"] = kid.clone().into();
        jwk["alg"] = alg.name().into();
        jwk["use"] = "sig".into();
        Self {
            kid,
            alg,
            signer,
            jwk,
        }
    }

    fn sign(&self, input: &[u8]) -> Vec<u8> {
        match &self.signer {
            Signer::Rsa(key) => {
                let mut signature = vec![0; key.public_modulus_len()];
                key.sign(
                    &RSA_PKCS1_SHA256,
                    &SystemRandom::new(),
                    input,
                    &mut signature,
                )
                .unwrap();
                signature
            }
            Signer::Ec(key) => key
                .sign(&SystemRandom::new(), input)
                .unwrap()
                .as_ref()
                .to_vec(),
            Signer::Ed(key) => key.sign(input).as_ref().to_vec(),
        }
    }
}

struct State {
    issuer: String,
    keys: Vec<Key>,
    up: bool,
    jwks_fetches: usize,
    rotations: usize,
}

/// An OIDC issuer on a loopback port, serving
/// `/.well-known/openid-configuration` and `/jwks`, until dropped.
pub struct TestIssuer {
    issuer: String,
    state: Arc<Mutex<State>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for TestIssuer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl TestIssuer {
    /// An issuer with one key per [`TestAlg`], up.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let keys = [TestAlg::Rs256, TestAlg::Es256, TestAlg::EdDsa]
            .into_iter()
            .map(|alg| Key::generate(alg, format!("{}-0", alg.name())))
            .collect();
        let state = Arc::new(Mutex::new(State {
            issuer: issuer.clone(),
            keys,
            up: true,
            jwks_fetches: 0,
            rotations: 0,
        }));
        #[allow(
            clippy::disallowed_methods,
            reason = "the test issuer stands in for an identity provider; it serves no OpenXet route"
        )]
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            issuer,
            state,
            server,
        }
    }

    /// This issuer, discovered as `openxet serve` discovers an
    /// `[[auth.oidc]]` entry named `corp` for [`AUDIENCE`], changed by
    /// `change`. Its client keeps no idle connections, so none outlives
    /// the runtime it was opened on when a server on another runtime
    /// verifies with it.
    pub async fn discover(&self, change: impl FnOnce(&mut OidcConfig)) -> Arc<OidcIssuer> {
        let mut cfg = OidcConfig::new("corp", &self.issuer, &[AUDIENCE]);
        change(&mut cfg);
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        Arc::new(OidcIssuer::discover(cfg, http).await.unwrap())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The issuer URL, as tokens carry it in `iss`.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The `kid` of the current `alg` key.
    #[must_use]
    pub fn kid(&self, alg: TestAlg) -> String {
        let state = self.state();
        state
            .keys
            .iter()
            .find(|k| k.alg == alg)
            .unwrap()
            .kid
            .clone()
    }

    /// Replaces the `alg` key with a new one under a new `kid`, as an
    /// identity provider rotating its signing key does; returns the `kid`.
    pub fn rotate(&self, alg: TestAlg) -> String {
        let mut state = self.state();
        state.rotations += 1;
        let kid = format!("{}-{}", alg.name(), state.rotations);
        let key = Key::generate(alg, kid.clone());
        state.keys.retain(|k| k.alg != alg);
        state.keys.push(key);
        kid
    }

    /// Takes the issuer down (every request answers `503`) or brings it
    /// back up.
    pub fn set_up(&self, up: bool) {
        self.state().up = up;
    }

    /// How many JWKS requests the issuer answered while up.
    #[must_use]
    pub fn jwks_fetches(&self) -> usize {
        self.state().jwks_fetches
    }

    /// Claims for `sub`: this issuer, [`AUDIENCE`], issued at `now` and
    /// valid for an hour.
    #[must_use]
    pub fn claims(&self, sub: &str, now: u64) -> Value {
        json!({
            "iss": self.issuer,
            "aud": AUDIENCE,
            "sub": sub,
            "iat": now,
            "exp": now + 3600,
        })
    }

    /// `claims` signed with the current `alg` key, the header naming it.
    #[must_use]
    pub fn sign(&self, alg: TestAlg, claims: &Value) -> String {
        let header = json!({"alg": alg.name(), "typ": "JWT", "kid": self.kid(alg)});
        self.sign_with_header(alg, &header, claims)
    }

    /// `claims` under `header` as given, signed with the current `alg` key,
    /// whatever the header says.
    #[must_use]
    pub fn sign_with_header(&self, alg: TestAlg, header: &Value, claims: &Value) -> String {
        let input = signing_input(header, claims);
        let state = self.state();
        let key = state.keys.iter().find(|k| k.alg == alg).unwrap();
        format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()))
        )
    }

    /// `claims` under `alg: none` with an empty signature, the header
    /// naming the RS256 key.
    #[must_use]
    pub fn unsigned(&self, claims: &Value) -> String {
        let header = json!({"alg": "none", "typ": "JWT", "kid": self.kid(TestAlg::Rs256)});
        format!("{}.", signing_input(&header, claims))
    }

    /// `claims` under `alg: HS256`, keyed with the RS256 key's public
    /// modulus as published: the key-confusion forgery anyone holding the
    /// JWKS can make (RFC 8725 §2.1).
    #[must_use]
    pub fn hs256_with_public_key(&self, claims: &Value) -> String {
        let kid = self.kid(TestAlg::Rs256);
        let modulus = {
            let state = self.state();
            let key = state.keys.iter().find(|k| k.kid == kid).unwrap();
            key.jwk["n"].as_str().unwrap().to_owned()
        };
        let header = json!({"alg": "HS256", "typ": "JWT", "kid": kid});
        let input = signing_input(&header, claims);
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(modulus.as_bytes()).unwrap();
        mac.update(input.as_bytes());
        let tag = mac.finalize().into_bytes();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(tag))
    }
}

fn signing_input(header: &Value, claims: &Value) -> String {
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

async fn discovery(Shared(state): Shared<Arc<Mutex<State>>>) -> Response {
    let state = state.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.up {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    axum::Json(json!({
        "issuer": state.issuer,
        "jwks_uri": format!("{}/jwks", state.issuer),
        "id_token_signing_alg_values_supported": ["RS256", "ES256", "EdDSA"],
    }))
    .into_response()
}

async fn jwks(Shared(state): Shared<Arc<Mutex<State>>>) -> Response {
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.up {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    state.jwks_fetches += 1;
    let keys: Vec<Value> = state.keys.iter().map(|k| k.jwk.clone()).collect();
    axum::Json(json!({ "keys": keys })).into_response()
}
