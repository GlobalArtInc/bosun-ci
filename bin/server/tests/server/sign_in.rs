use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bosun_testkit::TestDatabase;
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

use crate::api::send;
use crate::proxy::Proxy;
use crate::{Launch, Server, client, message};

const FIRST_START: &str = "created the owner admin on the first start and wrote its token and password to the data \
     directory";

const NEW_PASSWORD: &str = "a passphrase of my own";

const PENDING: &str =
    "`admin` must set a new password before anything else; POST /api/v1/auth/password sets it";

const AT_ONCE: Duration = Duration::from_secs(5);

struct Fresh {
    _server: Server,
    address: SocketAddr,
    data: PathBuf,
    token: String,
    password: String,
}

struct Session {
    cookie: String,
    csrf: String,
}

async fn fresh(launch: Launch) -> Fresh {
    let data = launch.data_dir();
    let mut server = launch.spawn();
    server
        .expect("the first start", |line| message(line) == FIRST_START)
        .await;
    let address = server.listening().await;
    Fresh {
        _server: server,
        address,
        token: secret(&data.join("admin.token")),
        password: secret(&data.join("admin.password")),
        data,
    }
}

fn secret(path: &Path) -> String {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mode = fs::metadata(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{}", path.display());
    text.trim_end().to_owned()
}

fn url(address: SocketAddr, path: &str) -> String {
    format!("http://{address}{path}")
}

fn login_request(address: SocketAddr, login: &str, password: &str) -> RequestBuilder {
    client()
        .post(url(address, "/api/v1/auth/login"))
        .json(&json!({ "login": login, "password": password }))
}

async fn sign_in(address: SocketAddr, login: &str, password: &str) -> Session {
    let response = login_request(address, login, password)
        .send()
        .await
        .expect("POST /api/v1/auth/login");
    assert_eq!(response.status(), 200);
    session(response).await
}

async fn session(response: reqwest::Response) -> Session {
    let set = response.headers()["set-cookie"]
        .to_str()
        .expect("a cookie header")
        .to_owned();
    let body: Value = response.json().await.expect("a JSON body");
    Session {
        cookie: set.split(';').next().expect("a cookie").to_owned(),
        csrf: body["csrf_token"]
            .as_str()
            .expect("a CSRF token")
            .to_owned(),
    }
}

fn with(request: RequestBuilder, session: &Session) -> RequestBuilder {
    request
        .header("cookie", &session.cookie)
        .header("x-bosun-csrf", &session.csrf)
}

async fn change_password(address: SocketAddr, session: &Session, current: &str) -> (u16, Value) {
    send(
        with(
            client().post(url(address, "/api/v1/auth/password")),
            session,
        )
        .json(&json!({ "current_password": current, "new_password": NEW_PASSWORD })),
    )
    .await
}

fn organisation(address: SocketAddr, slug: &str) -> RequestBuilder {
    client()
        .post(url(address, "/api/v1/organisations"))
        .json(&json!({ "slug": slug, "name": slug }))
}

fn rate_limited(status: u16, body: &Value, whose: &str) {
    assert_eq!(status, 429, "{body}");
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(
        body["error"]["message"],
        format!("too many failed sign-ins {whose}; sign-in is locked for 60 more seconds")
    );
}

#[tokio::test]
async fn a_fresh_data_directory_gets_admin_password_whose_sign_in_allows_only_a_new_password() {
    let database = TestDatabase::migrated().await;
    let fresh = fresh(Launch::serve(&database.url())).await;
    let address = fresh.address;
    assert!(fresh.token.starts_with("bst_"), "{}", fresh.token);
    assert_eq!(fresh.password.len(), 43, "{}", fresh.password);

    let response = login_request(address, "admin", &fresh.password)
        .send()
        .await
        .expect("POST /api/v1/auth/login");
    assert_eq!(response.status(), 200);
    let set = response.headers()["set-cookie"]
        .to_str()
        .expect("a cookie header")
        .to_owned();
    let mut parts = set.split(';').map(str::trim);
    let (name, value) = parts
        .next()
        .and_then(|pair| pair.split_once('='))
        .expect("a cookie");
    assert_eq!(name, "__Host-bosun_session");
    assert!(value.starts_with("bss_") && value.len() == 53, "{value}");
    let mut attributes: Vec<&str> = parts.collect();
    attributes.sort_unstable();
    assert_eq!(
        attributes,
        [
            "HttpOnly",
            "Max-Age=2592000",
            "Path=/",
            "SameSite=Lax",
            "Secure"
        ]
    );
    let body: Value = response.json().await.expect("a JSON body");
    assert_eq!(body["user"]["login"], "admin");
    assert_eq!(body["password_change_required"], true);
    let session = Session {
        cookie: format!("{name}={value}"),
        csrf: body["csrf_token"]
            .as_str()
            .expect("a CSRF token")
            .to_owned(),
    };

    let (status, me) = send(
        client()
            .get(url(address, "/api/v1/me"))
            .header("cookie", &session.cookie),
    )
    .await;
    assert_eq!(status, 200, "{me}");
    assert_eq!(me["user"]["login"], "admin");
    assert_eq!(me["csrf_token"], session.csrf.as_str());
    assert_eq!(me["password_change_required"], true);
    for request in [
        with(organisation(address, "globalart"), &session),
        with(
            client().get(url(address, "/api/v1/organisations/globalart")),
            &session,
        ),
        with(client().get(url(address, "/api/v1/agents")), &session),
    ] {
        let (status, body) = send(request).await;
        assert_eq!(status, 403, "{body}");
        assert_eq!(body["error"]["code"], "forbidden");
        assert_eq!(body["error"]["message"], PENDING);
    }
    assert!(fresh.data.join("admin.token").exists());
    assert!(fresh.data.join("admin.password").exists());

    let (status, changed) = change_password(address, &session, &fresh.password).await;
    assert_eq!(status, 200, "{changed}");
    assert_eq!(changed["password_change_required"], false);
    assert!(
        !fresh.data.join("admin.token").exists(),
        "admin.token stayed"
    );
    assert!(
        !fresh.data.join("admin.password").exists(),
        "admin.password stayed"
    );

    let (status, created) = send(with(organisation(address, "globalart"), &session)).await;
    assert_eq!(status, 201, "{created}");
    let (status, body) = send(login_request(address, "admin", &fresh.password)).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(
        body["error"]["message"],
        "the login or the password is not right"
    );
    let again = sign_in(address, "admin", NEW_PASSWORD).await;
    let (status, me) = send(
        client()
            .get(url(address, "/api/v1/me"))
            .header("cookie", &again.cookie),
    )
    .await;
    assert_eq!(status, 200, "{me}");
    assert_eq!(me["password_change_required"], false);

    let mut second = Launch::serve(&database.url())
        .env("BOSUN_SERVER_DATA_DIR", &fresh.data.to_string_lossy())
        .spawn();
    second.listening().await;
    assert!(!fresh.data.join("admin.token").exists());
    assert!(!fresh.data.join("admin.password").exists());
}

#[tokio::test]
async fn a_mutating_request_with_the_session_cookie_needs_the_csrf_token_of_that_session() {
    let database = TestDatabase::migrated().await;
    let fresh = fresh(Launch::serve(&database.url())).await;
    let address = fresh.address;
    let (status, methods) = send(client().get(url(address, "/api/v1/auth/methods"))).await;
    assert_eq!(status, 200, "{methods}");
    assert_eq!(methods, json!({ "password": true, "providers": [] }));
    let first = sign_in(address, "admin", &fresh.password).await;
    let (status, changed) = change_password(address, &first, &fresh.password).await;
    assert_eq!(status, 200, "{changed}");

    let one = sign_in(address, "admin", NEW_PASSWORD).await;
    let other = sign_in(address, "admin", NEW_PASSWORD).await;
    assert_ne!(one.csrf, other.csrf);
    let (status, body) = send(organisation(address, "one").header("cookie", &one.cookie)).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        body["error"]["message"],
        "a request that the session cookie authenticates and that changes something needs the \
         header X-Bosun-CSRF with the token of GET /api/v1/me"
    );
    let (status, body) = send(
        organisation(address, "one")
            .header("cookie", &one.cookie)
            .header("x-bosun-csrf", &other.csrf),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        body["error"]["message"],
        "the header X-Bosun-CSRF does not hold the token of this session"
    );
    let (status, body) = send(
        client()
            .get(url(address, "/api/v1/organisations/one"))
            .header("cookie", &one.cookie),
    )
    .await;
    assert_eq!(status, 404, "a read needs no CSRF token: {body}");

    let (status, body) = send(with(organisation(address, "one"), &one)).await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = send(organisation(address, "token").bearer_auth(&fresh.token)).await;
    assert_eq!(status, 201, "a bearer token needs no CSRF token: {body}");

    let response = with(client().post(url(address, "/api/v1/auth/logout")), &one)
        .send()
        .await
        .expect("POST /api/v1/auth/logout");
    assert_eq!(response.status(), 204);
    assert_eq!(
        response.headers()["set-cookie"],
        "__Host-bosun_session=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax"
    );
    let (status, body) = send(with(organisation(address, "after"), &one)).await;
    assert_eq!(status, 401, "{body}");
    let (status, body) = send(
        client()
            .post(url(address, "/api/v1/auth/logout"))
            .bearer_auth(&fresh.token),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "invalid_state");
}

#[tokio::test]
async fn a_malformed_session_cookie_is_refused_before_any_database_query() {
    let database = TestDatabase::migrated().await;
    let proxy = Proxy::start(&database.url()).await;
    let fresh = fresh(Launch::serve(&proxy.url(&database.url()))).await;
    let address = fresh.address;
    let session = sign_in(address, "admin", &fresh.password).await;
    let value = session
        .cookie
        .strip_prefix("__Host-bosun_session=")
        .expect("the session cookie")
        .to_owned();

    proxy.stall();
    let looking_up = {
        let cookie = session.cookie.clone();
        tokio::spawn(async move {
            send(
                client()
                    .get(url(address, "/api/v1/me"))
                    .header("cookie", cookie),
            )
            .await
            .0
        })
    };
    sleep(Duration::from_millis(300)).await;
    let mut misspelt = value.clone().into_bytes();
    misspelt[20] = if misspelt[20] == b'a' { b'b' } else { b'a' };
    let misspelt = String::from_utf8(misspelt).expect("ASCII");
    for malformed in [
        misspelt,
        format!("bst_{}", &value[4..]),
        value[..value.len() - 1].to_owned(),
        format!("{value}0"),
        String::new(),
    ] {
        let (status, body) = timeout(
            AT_ONCE,
            send(client().get(url(address, "/api/v1/me")).header(
                "cookie",
                format!("theme=dark; __Host-bosun_session={malformed}"),
            )),
        )
        .await
        .expect("a malformed cookie is refused at once, without the database");
        assert_eq!(status, 401, "{malformed}: {body}");
        assert_eq!(
            body["error"]["message"],
            "the session cookie does not hold a well-formed session"
        );
    }
    sleep(Duration::from_millis(700)).await;
    assert!(
        !looking_up.is_finished(),
        "a well-formed session was answered without the database"
    );

    proxy.forward();
    let status = looking_up
        .await
        .expect("the request that looks the session up");
    assert!(matches!(status, 200 | 500), "{status}");
}

#[tokio::test]
async fn failed_sign_ins_lock_the_account_and_the_address_named_by_a_trusted_proxy() {
    let trusting = TestDatabase::migrated().await;
    let trusted = fresh(
        Launch::serve(&trusting.url())
            .env("BOSUN_LIMITS_FAILED_SIGN_INS", "2")
            .env("BOSUN_SERVER_TRUSTED_PROXIES", "127.0.0.1, ::1"),
    )
    .await;
    let address = trusted.address;
    for _ in 0..2 {
        let (status, body) = send(
            login_request(address, "admin", "not the password")
                .header("x-forwarded-for", "198.51.100.1"),
        )
        .await;
        assert_eq!(status, 401, "{body}");
    }
    let (status, body) = send(
        login_request(address, "admin", &trusted.password)
            .header("x-forwarded-for", "198.51.100.2"),
    )
    .await;
    rate_limited(status, &body, "for `admin`");
    let (status, body) = send(
        login_request(address, "nobody", "not the password")
            .header("x-forwarded-for", "198.51.100.1"),
    )
    .await;
    rate_limited(status, &body, "from 198.51.100.1");
    let (status, body) = send(
        login_request(address, "nobody", "not the password")
            .header("x-forwarded-for", "198.51.100.3"),
    )
    .await;
    assert_eq!(status, 401, "{body}");

    let untrusting = TestDatabase::migrated().await;
    let untrusted =
        fresh(Launch::serve(&untrusting.url()).env("BOSUN_LIMITS_FAILED_SIGN_INS", "2")).await;
    let address = untrusted.address;
    for login in ["nobody", "someone"] {
        let (status, body) = send(
            login_request(address, login, "not the password")
                .header("x-forwarded-for", "198.51.100.1"),
        )
        .await;
        assert_eq!(status, 401, "{body}");
    }
    let (status, body) = send(
        login_request(address, "admin", &untrusted.password)
            .header("x-forwarded-for", "198.51.100.2"),
    )
    .await;
    rate_limited(status, &body, "from 127.0.0.1");
}
