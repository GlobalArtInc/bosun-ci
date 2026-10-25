use std::fs;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{self, Body};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use bosun_db::entity::{oidc_state, team, user, user_identity};
use bosun_testkit::TestDatabase;
use bosun_testkit::oidc::{Authorization, CLIENT_ID, FakeIdentityProvider, Person, Spoiled};
use bosun_types::id::OrganisationId;
use chrono::{TimeDelta, Utc};
use sea_orm::{EntityTrait, PaginatorTrait};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;

use crate::clock::Clock;
use crate::config::{Oidc, TeamMapping};
use crate::oidc::Providers;
use crate::{AppState, api, sign_in};

pub(super) const PROVIDER: &str = "fake";

pub(super) const PUBLIC_URL: &str = "https://ci.example.org";

const SESSION: &str = "__Host-bosun_session";

const STATE: &str = "__Host-bosun_oidc";

struct Site {
    database: TestDatabase,
    organisation: OrganisationId,
    clock: Clock,
    router: Router,
    provider: FakeIdentityProvider,
    _directory: TempDir,
}

#[derive(Debug)]
struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

struct Begun {
    location: String,
    cookie: String,
}

pub(super) fn providers(provider: &FakeIdentityProvider, directory: &Path) -> Providers {
    let secret = directory.join("oidc-secret");
    fs::write(&secret, format!("{}\n", provider.client_secret())).expect("the client secret");
    let config = Oidc {
        name: PROVIDER.to_owned(),
        issuer: provider.issuer(),
        client_id: CLIENT_ID.to_owned(),
        client_secret_file: secret,
        scopes: ["openid", "profile", "email"].map(str::to_owned).to_vec(),
        login_claim: "preferred_username".to_owned(),
        email_claim: "email".to_owned(),
        groups_claim: "groups".to_owned(),
        allowed_groups: vec!["bosun".to_owned()],
        allowed_email_domains: vec!["globalart.dev".to_owned()],
        teams: vec![
            TeamMapping {
                group: "bosun-keel".to_owned(),
                organisation: "globalart".to_owned(),
                team: "keel".to_owned(),
            },
            TeamMapping {
                group: "bosun-elsewhere".to_owned(),
                organisation: "nowhere".to_owned(),
                team: "ghosts".to_owned(),
            },
        ],
    };
    Providers::load(
        &[config],
        &crate::site::Site::new(Some(PUBLIC_URL.to_owned())),
    )
    .expect("the providers")
}

pub(super) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{name}=")))
        .map(str::to_owned)
}

pub(super) fn pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("a cookie pair")
        .to_owned()
}

pub(super) fn target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

impl Site {
    async fn new() -> Site {
        let database = TestDatabase::migrated().await;
        let organisation =
            bosun_db::organisations::create(database.connection(), "globalart", "GlobalArt")
                .await
                .expect("an organisation")
                .id;
        let provider = FakeIdentityProvider::start().await;
        let directory = tempfile::tempdir().expect("a directory");
        let clock = Clock::manual(Utc::now());
        let mut state: AppState = super::state(
            &database,
            directory.path(),
            clock.clone(),
            sign_in::Settings::new(Vec::new(), 10, directory.path().join("data")),
        )
        .await;
        state.oidc = Arc::new(providers(&provider, directory.path()));
        let router = api().into_router(&state).with_state(state);
        Site {
            database,
            organisation,
            clock,
            router,
            provider,
            _directory: directory,
        }
    }

    async fn send(&self, path: &str, cookies: &[&str]) -> Answer {
        let mut builder = Request::builder().method(Method::GET).uri(path);
        for cookie in cookies {
            builder = builder.header("cookie", *cookie);
        }
        let response = self
            .router
            .clone()
            .oneshot(builder.body(Body::empty()).expect("a request"))
            .await
            .expect("an answer");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        Answer {
            status,
            headers,
            body: if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).expect("a JSON body")
            },
        }
    }

    async fn begin(&self) -> Begun {
        let answer = self
            .send(&format!("/api/v1/auth/oidc/{PROVIDER}/start"), &[])
            .await;
        assert_eq!(answer.status, StatusCode::SEE_OTHER, "{answer:?}");
        let cookie = cookie(&answer.headers, STATE).expect("the cookie of the sign-in");
        Begun {
            location: answer.headers["location"]
                .to_str()
                .expect("a Location")
                .to_owned(),
            cookie: pair(&cookie),
        }
    }

    async fn sign_in(&self, login: &str) -> Answer {
        let begun = self.begin().await;
        let back = self.provider.authorize(&begun.location, login).await;
        self.send(&target(&back), &[&begun.cookie]).await
    }

    async fn users(&self) -> u64 {
        user::Entity::find()
            .count(self.database.connection())
            .await
            .expect("the users")
    }

    async fn members(&self, slug: &str) -> Option<Vec<String>> {
        let db = self.database.connection();
        let team = bosun_db::teams::find_by_slug(db, self.organisation, slug)
            .await
            .expect("a lookup")?;
        Some(
            bosun_db::teams::members(db, team.id, None, 100)
                .await
                .expect("the members")
                .into_iter()
                .map(|member| member.login)
                .collect(),
        )
    }
}

fn signed_in(answer: &Answer) -> String {
    assert_eq!(answer.status, StatusCode::SEE_OTHER, "{answer:?}");
    assert_eq!(answer.headers["location"], format!("{PUBLIC_URL}/"));
    let cleared = cookie(&answer.headers, STATE).expect("the cookie of the sign-in, emptied");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    let session = cookie(&answer.headers, SESSION).expect("a session cookie");
    assert!(
        session.ends_with("; Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Lax"),
        "{session}"
    );
    pair(&session)
}

fn refused(answer: &Answer, status: StatusCode, code: &str, message: &str) {
    assert_eq!(answer.status, status, "{answer:?}");
    assert_eq!(answer.body["error"]["code"], code, "{answer:?}");
    let text = answer.body["error"]["message"].as_str().expect("a message");
    assert!(text.contains(message), "`{text}` does not say `{message}`");
    assert!(cookie(&answer.headers, SESSION).is_none(), "{answer:?}");
}

#[tokio::test]
async fn a_person_in_an_allowed_group_signs_in_with_a_session_and_joins_the_mapped_team() {
    let site = Site::new().await;
    site.provider.add_person(Person::new(
        "dana",
        &["bosun", "bosun-keel", "bosun-elsewhere"],
    ));
    assert_eq!(site.members("keel").await, None);

    let methods = site.send("/api/v1/auth/methods", &[]).await;
    assert_eq!(methods.status, StatusCode::OK);
    assert_eq!(
        methods.body,
        serde_json::json!({ "password": true, "providers": [{ "name": PROVIDER }] })
    );

    let begun = site.begin().await;
    let location = Url::parse(&begun.location).expect("an address");
    assert!(
        begun.location.starts_with(&format!(
            "{}/protocol/openid-connect/auth?",
            site.provider.issuer()
        )),
        "{location}"
    );
    let query: std::collections::BTreeMap<String, String> =
        location.query_pairs().into_owned().collect();
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(
        query["redirect_uri"],
        format!("{PUBLIC_URL}/api/v1/auth/oidc/{PROVIDER}/callback")
    );
    assert_eq!(query["scope"], "openid profile email");
    assert_eq!(query["code_challenge_method"], "S256");
    for random in ["state", "nonce", "code_challenge"] {
        assert_eq!(query[random].len(), 43, "{random}");
    }
    assert!(begun.cookie.starts_with(&format!("{STATE}=")));
    let back = site.provider.authorize(&begun.location, "dana").await;
    let session = signed_in(&site.send(&target(&back), &[&begun.cookie]).await);
    assert_eq!(
        site.provider.authorizations(),
        [Authorization {
            redirect_uri: format!("{PUBLIC_URL}/api/v1/auth/oidc/{PROVIDER}/callback"),
            scope: "openid profile email".to_owned(),
            code_challenge_method: Some("S256".to_owned()),
        }]
    );

    let me = site.send("/api/v1/me", &[&session]).await;
    assert_eq!(me.status, StatusCode::OK, "{me:?}");
    assert_eq!(me.body["user"]["login"], "dana");
    assert_eq!(me.body["user"]["name"], "dana of the fake provider");
    assert_eq!(me.body["user"]["email"], "dana@example.org");
    assert_eq!(me.body["user"]["owner"], false);
    assert_eq!(me.body["password_change_required"], false);
    assert_eq!(site.members("keel").await, Some(vec!["dana".to_owned()]));
    let created = team::Entity::find()
        .all(site.database.connection())
        .await
        .expect("the teams");
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0].name, "keel");
    let identities = user_identity::Entity::find()
        .all(site.database.connection())
        .await
        .expect("the identities");
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].issuer, site.provider.issuer());

    let again = site.sign_in("dana").await;
    signed_in(&again);
    assert_eq!(site.users().await, 1);
}

#[tokio::test]
async fn a_person_outside_the_rules_is_refused_and_no_user_is_created() {
    let site = Site::new().await;
    site.provider
        .add_person(Person::new("eve", &["ops", "bosun-keel"]));
    let mut unverified = Person::new("mallory", &[]);
    unverified.email = "mallory@globalart.dev".to_owned();
    unverified.email_verified = false;
    site.provider.add_person(unverified);
    let mut by_domain = Person::new("fox", &[]);
    by_domain.email = "fox@GlobalArt.dev".to_owned();
    site.provider.add_person(by_domain);

    for login in ["eve", "mallory"] {
        refused(
            &site.sign_in(login).await,
            StatusCode::FORBIDDEN,
            "forbidden",
            &format!(
                "`{login}` is in no group, and has no verified email address of a domain, that `{PROVIDER}` admits"
            ),
        );
    }
    assert_eq!(site.users().await, 0);
    assert_eq!(site.members("keel").await, None);

    signed_in(&site.sign_in("fox").await);
    assert_eq!(site.users().await, 1);
    assert_eq!(site.members("keel").await, None);
}

#[tokio::test]
async fn an_id_token_is_refused_for_its_audience_issuer_nonce_expiry_or_key() {
    let site = Site::new().await;
    site.provider.add_person(Person::new("dana", &["bosun"]));
    for (spoiled, message) in [
        (
            Spoiled::Audience,
            "is meant for `someone-else`, and the client of Bosun is `bosun`",
        ),
        (Spoiled::Issuer, "names the issuer"),
        (
            Spoiled::Nonce,
            "carries another nonce than the sign-in this browser started",
        ),
        (Spoiled::Expired, "expired at"),
        (
            Spoiled::ForeignKey,
            "carries a signature that the provider's key does not make",
        ),
    ] {
        site.provider.spoil_next_token(spoiled);
        refused(
            &site.sign_in("dana").await,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            &format!("the ID token of `{PROVIDER}` is refused: it {message}"),
        );
    }
    assert_eq!(site.users().await, 0);
    signed_in(&site.sign_in("dana").await);
}

#[tokio::test]
async fn a_callback_is_refused_when_its_state_was_used_or_belongs_to_another_browser() {
    let site = Site::new().await;
    site.provider.add_person(Person::new("dana", &["bosun"]));
    let first = site.begin().await;
    let back = site.provider.authorize(&first.location, "dana").await;
    let second = site.begin().await;

    refused(
        &site.send(&target(&back), &[&second.cookie]).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "the state of the callback is not the state of the sign-in that this browser started",
    );
    refused(
        &site.send(&target(&back), &[]).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        &format!("the sign-in through `{PROVIDER}` did not start in this browser"),
    );
    let position = STATE.len() + 6;
    let changed = if first.cookie.as_bytes()[position] == b'A' {
        "B"
    } else {
        "A"
    };
    let mut forged = first.cookie.clone();
    forged.replace_range(position..=position, changed);
    refused(
        &site.send(&target(&back), &[&forged]).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "the cookie of the sign-in is not one that Bosun wrote",
    );
    signed_in(
        &site
            .send(&target(&back), &["theme=dark", &first.cookie])
            .await,
    );
    refused(
        &site.send(&target(&back), &[&first.cookie]).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "the state of the callback was used already",
    );
    assert_eq!(
        oidc_state::Entity::find()
            .count(site.database.connection())
            .await
            .expect("the states"),
        1
    );

    let late = site.begin().await;
    let back = site.provider.authorize(&late.location, "dana").await;
    site.clock.advance(TimeDelta::minutes(10));
    refused(
        &site.send(&target(&back), &[&late.cookie]).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        &format!("the sign-in through `{PROVIDER}` started more than 10 minutes ago"),
    );

    let refusing = site.begin().await;
    let back = site.provider.authorize(&refusing.location, "nobody").await;
    refused(
        &site.send(&target(&back), &[&refusing.cookie]).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        &format!("the identity provider `{PROVIDER}` refused the sign-in: access_denied"),
    );
}

#[tokio::test]
async fn leaving_the_group_at_the_provider_leaves_the_mapped_team_at_the_next_sign_in() {
    let site = Site::new().await;
    site.provider
        .add_person(Person::new("dana", &["bosun", "bosun-keel"]));
    let session = signed_in(&site.sign_in("dana").await);
    assert_eq!(site.members("keel").await, Some(vec!["dana".to_owned()]));

    site.provider.set_groups("dana", &["bosun"]);
    assert_eq!(site.members("keel").await, Some(vec!["dana".to_owned()]));
    signed_in(&site.sign_in("dana").await);
    assert_eq!(site.members("keel").await, Some(Vec::new()));
    assert_eq!(site.users().await, 1);

    site.provider.set_groups("dana", &["bosun-keel"]);
    refused(
        &site.sign_in("dana").await,
        StatusCode::FORBIDDEN,
        "forbidden",
        "`dana` is in no group",
    );
    assert_eq!(site.members("keel").await, Some(Vec::new()));
    assert_eq!(
        site.send("/api/v1/me", &[&session]).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_first_sign_in_never_takes_over_a_user_by_their_login() {
    let site = Site::new().await;
    bosun_db::users::create(site.database.connection(), "dana")
        .await
        .expect("a local user");
    site.provider.add_person(Person::new("dana", &["bosun"]));
    refused(
        &site.sign_in("dana").await,
        StatusCode::CONFLICT,
        "conflict",
        &format!("the login `dana` belongs to a user who did not sign in through `{PROVIDER}`"),
    );
    assert_eq!(site.users().await, 1);
    assert_eq!(
        user_identity::Entity::find()
            .count(site.database.connection())
            .await
            .expect("the identities"),
        0
    );
}

#[tokio::test]
async fn a_provider_that_is_not_configured_is_not_found() {
    let site = Site::new().await;
    for path in [
        "/api/v1/auth/oidc/keycloak/start",
        "/api/v1/auth/oidc/keycloak/callback?code=a&state=b",
    ] {
        let answer = site.send(path, &[]).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{answer:?}");
        assert_eq!(
            answer.body["error"]["message"],
            "there is no identity provider `keycloak`"
        );
    }
}
