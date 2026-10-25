use bosun_testkit::TestDatabase;
use bosun_testkit::oidc::{CLIENT_ID, FakeIdentityProvider, Person};
use reqwest::StatusCode;
use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use serde_json::{Value, json};

use crate::{CONFIG, Launch, WAIT, message};

const PUBLIC_URL: &str = "https://ci.example.org";

fn browser() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(WAIT)
        .build()
        .expect("an HTTP client that follows no redirect")
}

fn cookie(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{name}=")))
        .and_then(|value| value.split(';').next())
        .unwrap_or_else(|| panic!("no cookie {name} in {response:?}"))
        .to_owned()
}

#[tokio::test]
async fn a_person_signs_in_through_a_configured_provider_of_the_served_binary() {
    let database = TestDatabase::migrated().await;
    let provider = FakeIdentityProvider::start().await;
    provider.add_person(Person::new("dana", &["bosun"]));
    let launch = Launch::serve(&database.url());
    let secret = launch.file("oidc-secret", &format!("{}\n", provider.client_secret()));
    let mut server = launch
        .config(&format!(
            "{CONFIG}\n[[oidc]]\nname = \"sso\"\nissuer = \"{}\"\nclient_id = \"{CLIENT_ID}\"\n\
             client_secret_file = \"{}\"\nallowed_groups = [\"bosun\"]\n",
            provider.issuer(),
            secret.display()
        ))
        .env("BOSUN_SERVER_PUBLIC_URL", PUBLIC_URL)
        .spawn();
    let address = server.listening().await;
    let http = browser();

    let methods: Value = http
        .get(format!("http://{address}/api/v1/auth/methods"))
        .send()
        .await
        .expect("GET /api/v1/auth/methods")
        .json()
        .await
        .expect("a JSON body");
    assert_eq!(
        methods,
        json!({ "password": true, "providers": [{ "name": "sso" }] })
    );

    let begun = http
        .get(format!("http://{address}/api/v1/auth/oidc/sso/start"))
        .send()
        .await
        .expect("the start");
    assert_eq!(begun.status(), StatusCode::SEE_OTHER, "{begun:?}");
    let state = cookie(&begun, "__Host-bosun_oidc");
    let location = begun.headers()[LOCATION].to_str().expect("a Location");
    let back = provider.authorize(location, "dana").await;
    assert!(
        back.as_str()
            .starts_with(&format!("{PUBLIC_URL}/api/v1/auth/oidc/sso/callback?")),
        "{back}"
    );
    let mut callback = back.clone();
    callback.set_scheme("http").expect("an http address");
    callback
        .set_host(Some(&address.ip().to_string()))
        .expect("a host");
    callback.set_port(Some(address.port())).expect("a port");
    let signed = http
        .get(callback)
        .header(COOKIE, state)
        .send()
        .await
        .expect("the callback");
    assert_eq!(signed.status(), StatusCode::SEE_OTHER, "{signed:?}");
    assert_eq!(signed.headers()[LOCATION], format!("{PUBLIC_URL}/"));
    let session = cookie(&signed, "__Host-bosun_session");

    let me: Value = http
        .get(format!("http://{address}/api/v1/me"))
        .header(COOKIE, session)
        .send()
        .await
        .expect("GET /api/v1/me")
        .json()
        .await
        .expect("a JSON body");
    assert_eq!(me["user"]["login"], "dana", "{me}");
    server
        .expect("the sign-in in the log", |line| {
            message(line) == "signed in through an identity provider" && line["login"] == "dana"
        })
        .await;
}
