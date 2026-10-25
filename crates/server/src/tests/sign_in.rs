use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{self, Body};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use bosun_blob::Directory;
use bosun_db::entity::session;
use bosun_db::tokens::NewToken;
use bosun_testkit::TestDatabase;
use bosun_types::id::UserId;
use chrono::{TimeDelta, Utc};
use sea_orm::{EntityTrait, PaginatorTrait};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::clock::Clock;
use crate::store::Backend;
use crate::token::{PERSONAL, Token};
use crate::{AppState, api, logs, sessions, shutdown, sign_in};

const PASSWORD: &str = "correct horse battery staple";

const WRONG: &str = "correct horse battery stapler";

const HOME: &str = "192.0.2.10";

const ELSEWHERE: &str = "192.0.2.20";

struct Site {
    database: TestDatabase,
    state: AppState,
    clock: Clock,
    router: Router,
    _directory: TempDir,
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

struct Signed {
    cookie: String,
    csrf: String,
}

impl Site {
    async fn new(failed_sign_ins: u32, trusted_proxies: &[&str]) -> Site {
        let database = TestDatabase::migrated().await;
        let directory = tempfile::tempdir().expect("a directory");
        let store = Directory::open(directory.path().join("store"))
            .await
            .expect("a directory store");
        let clock = Clock::manual(Utc::now());
        let state = AppState {
            db: database.connection().clone(),
            store: Arc::new(Backend::Directory(store)),
            followers: Arc::new(logs::Followers::default()),
            shutdown: shutdown::channel().1,
            agent_binaries: Arc::from(directory.path().join("agent")),
            clock: clock.clone(),
            site: crate::site::Site::default(),
            sign_in: Arc::new(sign_in::Settings::new(
                trusted_proxies
                    .iter()
                    .map(|text| text.parse().expect("a network"))
                    .collect(),
                failed_sign_ins,
                directory.path().join("data"),
            )),
        };
        let router = api().into_router(&state).with_state(state.clone());
        Site {
            database,
            state,
            clock,
            router,
            _directory: directory,
        }
    }

    async fn user(&self, login: &str) -> (UserId, String) {
        let token = Token::generate(PERSONAL).expect("random bytes");
        let user = bosun_db::users::create_owner(
            self.database.connection(),
            login,
            NewToken {
                name: "cli".to_owned(),
                sha256: token.sha256(),
                expires_at: Utc::now() + TimeDelta::days(400),
            },
        )
        .await
        .expect("an owner");
        let hash = self
            .state
            .sign_in
            .hasher
            .hash(PASSWORD.to_owned())
            .await
            .expect("a hash");
        bosun_db::users::set_password(self.database.connection(), user.id, hash)
            .await
            .expect("a password");
        (user.id, token.into_string())
    }

    async fn send(&self, request: Request<Body>) -> Answer {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("an answer");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("a JSON body")
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    async fn sign_in(&self, login: &str, password: &str, peer: &str) -> Answer {
        self.sign_in_through(login, password, peer, None).await
    }

    async fn sign_in_through(
        &self,
        login: &str,
        password: &str,
        peer: &str,
        forwarded_for: Option<&str>,
    ) -> Answer {
        let mut request = request(Method::POST, "/api/v1/auth/login", peer)
            .header("content-type", "application/json");
        if let Some(forwarded_for) = forwarded_for {
            request = request.header("x-forwarded-for", forwarded_for);
        }
        self.send(
            request
                .body(Body::from(
                    json!({ "login": login, "password": password }).to_string(),
                ))
                .expect("a request"),
        )
        .await
    }

    async fn signed(&self, login: &str) -> Signed {
        let answer = self.sign_in(login, PASSWORD, HOME).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        let set = answer.headers["set-cookie"]
            .to_str()
            .expect("a cookie header");
        let cookie = set.split(';').next().expect("a cookie").to_owned();
        Signed {
            cookie,
            csrf: answer.body["csrf_token"]
                .as_str()
                .expect("a CSRF token")
                .to_owned(),
        }
    }

    async fn me(&self, signed: &Signed) -> Answer {
        self.send(
            request(Method::GET, "/api/v1/me", HOME)
                .header("cookie", &signed.cookie)
                .body(Body::empty())
                .expect("a request"),
        )
        .await
    }

    async fn post(&self, path: &str, signed: &Signed, body: Value) -> Answer {
        self.send(
            request(Method::POST, path, HOME)
                .header("cookie", &signed.cookie)
                .header("x-bosun-csrf", &signed.csrf)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("a request"),
        )
        .await
    }

    async fn sessions(&self) -> u64 {
        session::Entity::find()
            .count(self.database.connection())
            .await
            .expect("the sessions")
    }
}

fn request(method: Method, path: &str, peer: &str) -> axum::http::request::Builder {
    let peer: SocketAddr = format!("{peer}:41234").parse().expect("a peer");
    Request::builder()
        .method(method)
        .uri(path)
        .extension(ConnectInfo(peer))
}

fn retry_after(answer: &Answer) -> i64 {
    answer.headers["retry-after"]
        .to_str()
        .expect("a Retry-After header")
        .parse()
        .expect("a number of seconds")
}

fn locked(answer: &Answer, whose: &str, seconds: i64) {
    assert_eq!(
        answer.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        answer.body
    );
    assert_eq!(answer.body["error"]["code"], "rate_limited");
    assert_eq!(
        answer.body["error"]["message"],
        format!("too many failed sign-ins {whose}; sign-in is locked for {seconds} more seconds")
    );
    assert_eq!(retry_after(answer), seconds);
}

fn refused(answer: &Answer) {
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{}", answer.body);
    assert_eq!(answer.body["error"]["code"], "unauthenticated");
    assert_eq!(
        answer.body["error"]["message"],
        "the login or the password is not right"
    );
}

#[tokio::test]
async fn a_session_ends_after_12_idle_hours_or_30_days_and_at_once_after_logout() {
    let site = Site::new(10, &[]).await;
    site.user("roman").await;

    let idle = site.signed("roman").await;
    site.clock
        .advance(TimeDelta::hours(12) - TimeDelta::seconds(1));
    assert_eq!(site.me(&idle).await.status, StatusCode::OK);
    site.clock
        .advance(TimeDelta::hours(12) - TimeDelta::seconds(1));
    assert_eq!(site.me(&idle).await.status, StatusCode::OK);
    site.clock.advance(TimeDelta::hours(12));
    let answer = site.me(&idle).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{}", answer.body);
    assert_eq!(
        answer.body["error"]["message"],
        "the session ended after 12 hours without a request"
    );

    let busy = site.signed("roman").await;
    let mut elapsed = TimeDelta::zero();
    loop {
        site.clock.advance(TimeDelta::hours(11));
        elapsed += TimeDelta::hours(11);
        let answer = site.me(&busy).await;
        if elapsed < TimeDelta::days(30) {
            assert_eq!(
                answer.status,
                StatusCode::OK,
                "after {elapsed}: {}",
                answer.body
            );
            continue;
        }
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{}", answer.body);
        assert_eq!(
            answer.body["error"]["message"],
            "the session ended 30 days after it began"
        );
        break;
    }

    let ending = site.signed("roman").await;
    let kept = site.signed("roman").await;
    assert_eq!(site.me(&ending).await.status, StatusCode::OK);
    let answer = site.post("/api/v1/auth/logout", &ending, json!({})).await;
    assert_eq!(answer.status, StatusCode::NO_CONTENT, "{}", answer.body);
    assert_eq!(
        answer.headers["set-cookie"],
        "__Host-bosun_session=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax"
    );
    let answer = site.me(&ending).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{}", answer.body);
    assert_eq!(
        answer.body["error"]["message"],
        "the session is not known; it ended or never began"
    );
    assert_eq!(site.me(&kept).await.status, StatusCode::OK);

    assert_eq!(site.sessions().await, 3);
    let swept = bosun_db::sessions::sweep(
        site.database.connection(),
        sessions::expiry(site.clock.now()),
    )
    .await
    .expect("a sweep")
    .expect("the lock of the sweep");
    assert_eq!(swept.sessions, 2);
    assert_eq!(site.sessions().await, 1);
    assert_eq!(site.me(&kept).await.status, StatusCode::OK);
}

#[tokio::test]
async fn failed_sign_ins_lock_the_account_for_a_growing_period() {
    let site = Site::new(3, &[]).await;
    site.user("roman").await;
    let mut peers = (1..).map(|host| format!("198.51.100.{host}"));
    let mut peer = move || peers.next().expect("another address");

    for _ in 0..3 {
        refused(&site.sign_in("roman", WRONG, &peer()).await);
    }
    let whose = "for `roman`";
    locked(&site.sign_in("roman", PASSWORD, HOME).await, whose, 60);
    locked(&site.sign_in("roman", PASSWORD, ELSEWHERE).await, whose, 60);
    site.clock.advance(TimeDelta::seconds(59));
    locked(&site.sign_in("roman", PASSWORD, ELSEWHERE).await, whose, 1);

    for minutes in [2, 4, 8] {
        site.clock.advance(TimeDelta::minutes(minutes / 2));
        refused(&site.sign_in("roman", WRONG, &peer()).await);
        locked(
            &site.sign_in("roman", PASSWORD, ELSEWHERE).await,
            whose,
            minutes * 60,
        );
    }

    site.clock.advance(TimeDelta::minutes(8));
    assert_eq!(
        site.sign_in("roman", PASSWORD, ELSEWHERE).await.status,
        StatusCode::OK
    );
    refused(&site.sign_in("roman", WRONG, &peer()).await);
    assert_eq!(
        site.sign_in("roman", PASSWORD, ELSEWHERE).await.status,
        StatusCode::OK,
        "a sign-in starts the count of the account afresh"
    );
}

#[tokio::test]
async fn failed_sign_ins_lock_the_address_and_successful_ones_do_not_count() {
    let site = Site::new(3, &[]).await;
    site.user("roman").await;

    refused(&site.sign_in("nobody", WRONG, HOME).await);
    refused(&site.sign_in("someone", WRONG, HOME).await);
    for _ in 0..5 {
        assert_eq!(
            site.sign_in("roman", PASSWORD, HOME).await.status,
            StatusCode::OK
        );
    }
    refused(&site.sign_in("Not A Login", WRONG, HOME).await);
    let whose = format!("from {HOME}");
    locked(&site.sign_in("roman", PASSWORD, HOME).await, &whose, 60);
    locked(&site.sign_in("anyone", WRONG, HOME).await, &whose, 60);
    assert_eq!(
        site.sign_in("roman", PASSWORD, ELSEWHERE).await.status,
        StatusCode::OK
    );

    site.clock.advance(TimeDelta::minutes(1));
    assert_eq!(
        site.sign_in("roman", PASSWORD, HOME).await.status,
        StatusCode::OK
    );
    refused(&site.sign_in("nobody", WRONG, HOME).await);
    locked(&site.sign_in("roman", PASSWORD, HOME).await, &whose, 120);

    site.clock
        .advance(TimeDelta::days(1) + TimeDelta::minutes(2));
    refused(&site.sign_in("nobody", WRONG, HOME).await);
    assert_eq!(
        site.sign_in("roman", PASSWORD, HOME).await.status,
        StatusCode::OK,
        "failures a day old are forgotten"
    );
}

#[tokio::test]
async fn the_address_comes_from_x_forwarded_for_only_behind_a_trusted_proxy() {
    let site = Site::new(2, &["10.0.0.0/8"]).await;
    site.user("roman").await;
    let proxy = "10.1.1.1";
    let stranger = "203.0.113.5";

    for login in ["nobody", "someone"] {
        refused(
            &site
                .sign_in_through(login, WRONG, proxy, Some("198.51.100.7"))
                .await,
        );
    }
    locked(
        &site
            .sign_in_through("roman", PASSWORD, proxy, Some("198.51.100.7"))
            .await,
        "from 198.51.100.7",
        60,
    );
    assert_eq!(
        site.sign_in_through("roman", PASSWORD, proxy, Some("198.51.100.8"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        site.sign_in_through("roman", PASSWORD, stranger, Some("198.51.100.7"))
            .await
            .status,
        StatusCode::OK,
        "an untrusted peer names its own address, whatever it forwards"
    );

    for login in ["nobody", "someone"] {
        refused(
            &site
                .sign_in_through(login, WRONG, stranger, Some("198.51.100.9"))
                .await,
        );
    }
    locked(
        &site
            .sign_in_through("roman", PASSWORD, stranger, Some("198.51.100.10"))
            .await,
        &format!("from {stranger}"),
        60,
    );
}

#[tokio::test]
async fn a_change_of_password_checks_the_current_one_and_ends_the_other_sessions() {
    let site = Site::new(3, &[]).await;
    let (_, token) = site.user("roman").await;
    let changing = site.signed("roman").await;
    let other = site.signed("roman").await;

    for (body, field, message) in [
        (
            json!({ "current_password": PASSWORD, "new_password": "too short" }),
            "new_password",
            "a password is 12 to 1024 characters",
        ),
        (
            json!({ "current_password": PASSWORD, "new_password": PASSWORD }),
            "new_password",
            "is the current password, and a new password differs from it",
        ),
        (
            json!({ "current_password": WRONG, "new_password": "a new passphrase of mine" }),
            "current_password",
            "is not the password of `roman`",
        ),
    ] {
        let answer = site.post("/api/v1/auth/password", &changing, body).await;
        assert_eq!(
            answer.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            answer.body
        );
        assert_eq!(
            answer.body["error"]["fields"],
            json!([{ "field": field, "message": message }])
        );
    }

    let answer = site
        .post(
            "/api/v1/auth/password",
            &changing,
            json!({ "current_password": PASSWORD, "new_password": "a new passphrase of mine" }),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.body["user"]["login"], "roman");
    assert_eq!(answer.body["csrf_token"], changing.csrf.as_str());
    assert_eq!(answer.body["password_change_required"], false);
    assert_eq!(site.me(&changing).await.status, StatusCode::OK);
    assert_eq!(site.me(&other).await.status, StatusCode::UNAUTHORIZED);
    refused(&site.sign_in("roman", PASSWORD, ELSEWHERE).await);
    assert_eq!(
        site.sign_in("roman", "a new passphrase of mine", ELSEWHERE)
            .await
            .status,
        StatusCode::OK
    );

    let answer = site
        .send(
            request(Method::POST, "/api/v1/auth/password", HOME)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "current_password": "a new passphrase of mine",
                        "new_password": "yet another passphrase"
                    })
                    .to_string(),
                ))
                .expect("a request"),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.body["csrf_token"], Value::Null);
    assert_eq!(
        site.sessions().await,
        0,
        "a change by a token ends every session"
    );
}

#[tokio::test]
async fn wrong_current_passwords_count_as_failed_sign_ins() {
    let site = Site::new(2, &[]).await;
    site.user("roman").await;
    let signed = site.signed("roman").await;
    for _ in 0..2 {
        let answer = site
            .post(
                "/api/v1/auth/password",
                &signed,
                json!({ "current_password": WRONG, "new_password": "a new passphrase of mine" }),
            )
            .await;
        assert_eq!(
            answer.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            answer.body
        );
    }
    let answer = site
        .post(
            "/api/v1/auth/password",
            &signed,
            json!({ "current_password": PASSWORD, "new_password": "a new passphrase of mine" }),
        )
        .await;
    locked(&answer, "for `roman`", 60);
    locked(
        &site.sign_in("roman", PASSWORD, ELSEWHERE).await,
        "for `roman`",
        60,
    );
}

#[tokio::test]
async fn a_sign_in_needs_a_json_body_with_its_content_type() {
    let site = Site::new(3, &[]).await;
    site.user("roman").await;
    let answer = site
        .send(
            request(Method::POST, "/api/v1/auth/login", HOME)
                .header("content-type", "text/plain")
                .body(Body::from(
                    json!({ "login": "roman", "password": PASSWORD }).to_string(),
                ))
                .expect("a request"),
        )
        .await;
    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        answer.body
    );
    assert_eq!(
        answer.body["error"]["message"],
        "a request that checks a password has a JSON body with the content type \
         application/json"
    );
    let answer = site
        .send(
            request(Method::POST, "/api/v1/auth/login", HOME)
                .header("content-type", "Application/JSON; charset=utf-8")
                .body(Body::from(
                    json!({ "login": "roman", "password": PASSWORD }).to_string(),
                ))
                .expect("a request"),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
}
