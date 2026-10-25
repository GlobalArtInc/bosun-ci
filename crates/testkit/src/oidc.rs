use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::{AUTHORIZATION, LOCATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{TimeDelta, Utc};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use url::Url;
use url::form_urlencoded;
use uuid::Uuid;

use crate::forge::FOREIGN_KEY;

pub const CLIENT_ID: &str = "bosun";

const PROVIDER_KEY: &str = include_str!("../keys/provider.pem");

const KEY_ID: &str = "fake-provider-1";

const REALM: &str = "/realms/bosun";

const DISCOVERY: &str = "/realms/bosun/.well-known/openid-configuration";

const AUTHORIZE: &str = "/realms/bosun/protocol/openid-connect/auth";

const TOKEN: &str = "/realms/bosun/protocol/openid-connect/token";

const KEYS: &str = "/realms/bosun/protocol/openid-connect/certs";

const TOKEN_LIFETIME: TimeDelta = TimeDelta::minutes(5);

pub struct FakeIdentityProvider {
    shared: Arc<Shared>,
    server: JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub subject: String,
    pub login: String,
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoiled {
    Audience,
    Issuer,
    Nonce,
    Expired,
    ForeignKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    pub redirect_uri: String,
    pub scope: String,
    pub code_challenge_method: Option<String>,
}

struct Shared {
    base: String,
    secret: String,
    key: RsaKeyPair,
    foreign: RsaKeyPair,
    http: reqwest::Client,
    state: Mutex<Model>,
}

#[derive(Default)]
struct Model {
    people: BTreeMap<String, Person>,
    grants: HashMap<String, Grant>,
    spoiled: Option<Spoiled>,
    authorizations: Vec<Authorization>,
}

struct Grant {
    login: String,
    redirect_uri: String,
    nonce: Option<String>,
    challenge: Option<String>,
    used: bool,
}

impl Person {
    pub fn new(login: &str, groups: &[&str]) -> Person {
        Person {
            subject: Uuid::now_v7().to_string(),
            login: login.to_owned(),
            name: format!("{login} of the fake provider"),
            email: format!("{login}@example.org"),
            email_verified: true,
            groups: groups.iter().map(|group| (*group).to_owned()).collect(),
        }
    }
}

impl FakeIdentityProvider {
    pub async fn start() -> FakeIdentityProvider {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("the fake identity provider cannot listen: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("the fake identity provider has no address: {error}"));
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|error| panic!("the fake identity provider has no client: {error}"));
        let shared = Arc::new(Shared {
            base: format!("http://{address}"),
            secret: Uuid::now_v7().simple().to_string(),
            key: key_pair(PROVIDER_KEY),
            foreign: key_pair(FOREIGN_KEY),
            http,
            state: Mutex::new(Model::default()),
        });
        let router = Router::new()
            .route(DISCOVERY, get(discovery))
            .route(AUTHORIZE, get(authorize))
            .route(TOKEN, post(token))
            .route(KEYS, get(keys))
            .with_state(Arc::clone(&shared));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        FakeIdentityProvider { shared, server }
    }

    pub fn issuer(&self) -> String {
        self.shared.issuer()
    }

    pub fn client_secret(&self) -> &str {
        &self.shared.secret
    }

    pub fn add_person(&self, person: Person) {
        self.shared
            .lock()
            .people
            .insert(person.login.clone(), person);
    }

    pub fn set_groups(&self, login: &str, groups: &[&str]) {
        let mut state = self.shared.lock();
        let person = state
            .people
            .get_mut(login)
            .unwrap_or_else(|| panic!("the fake identity provider has no person `{login}`"));
        person.groups = groups.iter().map(|group| (*group).to_owned()).collect();
    }

    pub fn spoil_next_token(&self, spoiled: Spoiled) {
        self.shared.lock().spoiled = Some(spoiled);
    }

    pub fn authorizations(&self) -> Vec<Authorization> {
        self.shared.lock().authorizations.clone()
    }

    pub async fn authorize(&self, location: &str, login: &str) -> Url {
        let mut url = Url::parse(location)
            .unwrap_or_else(|error| panic!("`{location}` is not an address: {error}"));
        url.query_pairs_mut().append_pair("login_hint", login);
        let response = self
            .shared
            .http
            .get(url.clone())
            .send()
            .await
            .unwrap_or_else(|error| panic!("GET {url}: {error}"));
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "the fake identity provider did not return the browser from {url}"
        );
        let back = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| {
                panic!("the fake identity provider answered {url} without a Location")
            });
        Url::parse(back).unwrap_or_else(|error| panic!("`{back}` is not an address: {error}"))
    }
}

impl Drop for FakeIdentityProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Model> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn issuer(&self) -> String {
        format!("{}{REALM}", self.base)
    }
}

type Fake = State<Arc<Shared>>;

async fn discovery(State(shared): Fake) -> Json<Value> {
    let base = &shared.base;
    Json(json!({
        "issuer": shared.issuer(),
        "authorization_endpoint": format!("{base}{AUTHORIZE}"),
        "token_endpoint": format!("{base}{TOKEN}"),
        "jwks_uri": format!("{base}{KEYS}"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "code_challenge_methods_supported": ["S256"]
    }))
}

async fn keys(State(shared): Fake) -> Json<Value> {
    let public = RsaPublicKeyComponents::<Vec<u8>>::from(shared.key.public());
    Json(json!({
        "keys": [
            {
                "kty": "RSA",
                "kid": "fake-encryption",
                "use": "enc",
                "alg": "RSA-OAEP",
                "n": URL_SAFE_NO_PAD.encode(&public.n),
                "e": URL_SAFE_NO_PAD.encode(&public.e)
            },
            {
                "kty": "RSA",
                "kid": KEY_ID,
                "use": "sig",
                "alg": "RS256",
                "n": URL_SAFE_NO_PAD.encode(&public.n),
                "e": URL_SAFE_NO_PAD.encode(&public.e)
            }
        ]
    }))
}

async fn authorize(State(shared): Fake, Query(query): Query<BTreeMap<String, String>>) -> Response {
    let field = |name: &str| query.get(name).cloned();
    let Some(redirect_uri) = field("redirect_uri") else {
        return (StatusCode::BAD_REQUEST, "redirect_uri is missing").into_response();
    };
    if field("client_id").as_deref() != Some(CLIENT_ID) {
        return (StatusCode::BAD_REQUEST, "the client is not known").into_response();
    }
    let Ok(mut back) = Url::parse(&redirect_uri) else {
        return (StatusCode::BAD_REQUEST, "redirect_uri is not an address").into_response();
    };
    let mut state = shared.lock();
    state.authorizations.push(Authorization {
        redirect_uri: redirect_uri.clone(),
        scope: field("scope").unwrap_or_default(),
        code_challenge_method: field("code_challenge_method"),
    });
    {
        let mut pairs = back.query_pairs_mut();
        let known = field("login_hint").filter(|login| state.people.contains_key(login));
        match (field("response_type").as_deref(), known) {
            (Some("code"), Some(login)) => {
                let code = Uuid::now_v7().simple().to_string();
                state.grants.insert(
                    code.clone(),
                    Grant {
                        login,
                        redirect_uri: redirect_uri.clone(),
                        nonce: field("nonce"),
                        challenge: field("code_challenge"),
                        used: false,
                    },
                );
                pairs.append_pair("code", &code);
            }
            (Some("code"), None) => {
                pairs
                    .append_pair("error", "access_denied")
                    .append_pair("error_description", "the person did not sign in");
            }
            _ => {
                pairs.append_pair("error", "unsupported_response_type");
            }
        }
        if let Some(given) = field("state") {
            pairs.append_pair("state", &given);
        }
        pairs.append_pair("iss", &shared.issuer());
    }
    (StatusCode::FOUND, [(LOCATION, back.to_string())]).into_response()
}

async fn token(State(shared): Fake, headers: HeaderMap, body: Bytes) -> Response {
    let refused = |status: StatusCode, error: &str| {
        (status, Json(json!({ "error": error, "error_description": format!("the fake identity provider refuses: {error}") }))).into_response()
    };
    if !authenticated(&headers, &shared.secret) {
        return refused(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    let form: HashMap<String, String> = form_urlencoded::parse(&body).into_owned().collect();
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return refused(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    let mut state = shared.lock();
    let Some(grant) = form
        .get("code")
        .and_then(|code| state.grants.get_mut(code))
        .filter(|grant| !grant.used)
    else {
        return refused(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    grant.used = true;
    if form.get("redirect_uri") != Some(&grant.redirect_uri) {
        return refused(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    if let Some(challenge) = &grant.challenge {
        let proven = form.get("code_verifier").is_some_and(|verifier| {
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == *challenge
        });
        if !proven {
            return refused(StatusCode::BAD_REQUEST, "invalid_grant");
        }
    }
    let login = grant.login.clone();
    let nonce = grant.nonce.clone();
    let spoiled = state.spoiled.take();
    let Some(person) = state.people.get(&login).cloned() else {
        return refused(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    drop(state);
    let now = Utc::now();
    let mut claims = json!({
        "iss": shared.issuer(),
        "sub": person.subject,
        "aud": CLIENT_ID,
        "azp": CLIENT_ID,
        "iat": now.timestamp(),
        "exp": (now + TOKEN_LIFETIME).timestamp(),
        "nonce": nonce,
        "preferred_username": person.login,
        "name": person.name,
        "email": person.email,
        "email_verified": person.email_verified,
        "groups": person.groups
    });
    let mut key = &shared.key;
    match spoiled {
        Some(Spoiled::Audience) => {
            claims["aud"] = json!("someone-else");
            claims["azp"] = json!("someone-else");
        }
        Some(Spoiled::Issuer) => claims["iss"] = json!(format!("{}/elsewhere", shared.issuer())),
        Some(Spoiled::Nonce) => claims["nonce"] = json!("another-nonce"),
        Some(Spoiled::Expired) => claims["exp"] = json!((now - TimeDelta::minutes(1)).timestamp()),
        Some(Spoiled::ForeignKey) => key = &shared.foreign,
        None => {}
    }
    Json(json!({
        "access_token": Uuid::now_v7().simple().to_string(),
        "token_type": "Bearer",
        "expires_in": TOKEN_LIFETIME.num_seconds(),
        "id_token": sign(key, &claims)
    }))
    .into_response()
}

fn authenticated(headers: &HeaderMap, secret: &str) -> bool {
    let Some(credentials) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok())
    else {
        return false;
    };
    let Some((client, given)) = credentials.split_once(':') else {
        return false;
    };
    let decoded = |text: &str| -> String {
        form_urlencoded::parse(format!("value={text}").as_bytes())
            .next()
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default()
    };
    decoded(client) == CLIENT_ID && decoded(given) == secret
}

fn sign(key: &RsaKeyPair, claims: &Value) -> String {
    let header = json!({ "alg": "RS256", "typ": "JWT", "kid": KEY_ID });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .unwrap_or_else(|error| panic!("the fake identity provider cannot sign: {error}"));
    format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn key_pair(pem: &str) -> RsaKeyPair {
    let der = PrivateKeyDer::from_pem_slice(pem.as_bytes())
        .unwrap_or_else(|error| panic!("the fake provider's key is not PEM: {error}"));
    let pair = match der {
        PrivateKeyDer::Pkcs1(key) => RsaKeyPair::from_der(key.secret_pkcs1_der()),
        PrivateKeyDer::Pkcs8(key) => RsaKeyPair::from_pkcs8(key.secret_pkcs8_der()),
        _ => panic!("the fake provider's key is not an RSA key"),
    };
    pair.unwrap_or_else(|error| panic!("the fake provider's key is not usable: {error}"))
}
