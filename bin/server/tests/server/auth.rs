use std::net::SocketAddr;
use std::time::Duration;

use bosun_testkit::TestDatabase;
use sea_orm::ConnectionTrait;
use serde_json::Value;
use tokio::time::{sleep, timeout};

use crate::proxy::Proxy;
use crate::{Launch, client, eventually, me, owner};

const AT_ONCE: Duration = Duration::from_secs(5);

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn misspelt(token: &str, position: usize) -> String {
    let mut bytes = token.as_bytes().to_vec();
    bytes[position] = if bytes[position] == b'a' { b'b' } else { b'a' };
    String::from_utf8(bytes).expect("an ASCII token")
}

async fn refused(address: SocketAddr, authorization: Option<&str>, expected: &str) {
    let mut request = client().get(format!("http://{address}/api/v1/me"));
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let response = request.send().await.expect("GET /api/v1/me");
    assert_eq!(response.status(), 401, "{authorization:?}");
    assert_eq!(
        response.headers()["www-authenticate"],
        "Bearer",
        "{authorization:?}"
    );
    let body: Value = response.json().await.expect("a JSON body");
    assert_eq!(
        body["error"]["code"], "unauthenticated",
        "{authorization:?}"
    );
    assert_eq!(body["error"]["message"], expected, "{authorization:?}");
    assert_eq!(body["error"]["fields"], Value::Array(Vec::new()));
    assert_eq!(body["error"]["diagnostics"], Value::Array(Vec::new()));
}

async fn execute<C: ConnectionTrait>(db: &C, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

#[tokio::test]
async fn a_request_without_a_well_formed_bearer_token_is_unauthenticated() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "roman").await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;

    refused(address, None, "the request carries no bearer token").await;
    for authorization in ["Basic cm9tYW46c2VjcmV0", "Bearer", "Token abc", &token] {
        refused(
            address,
            Some(authorization),
            "the Authorization header does not hold a bearer token",
        )
        .await;
    }
    for authorization in [
        bearer("not-a-token"),
        bearer(&format!("brt_{}", &token[4..])),
        bearer(&token[..token.len() - 1]),
        bearer(&format!("{token}0")),
        bearer(&misspelt(&token, 10)),
        bearer(&misspelt(&token, token.len() - 1)),
    ] {
        refused(
            address,
            Some(&authorization),
            "the bearer token is not a well-formed personal token",
        )
        .await;
    }

    for authorization in [bearer(&token), format!("bearer  {token}")] {
        let (status, body) = me(address, Some(&authorization)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["user"]["login"], "roman");
    }

    let wrong_method = client()
        .delete(format!("http://{address}/api/v1/me"))
        .send()
        .await
        .expect("DELETE /api/v1/me");
    assert_eq!(wrong_method.status(), 404);
    let body: Value = wrong_method.json().await.expect("a JSON body");
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(body["error"]["message"], "there is no DELETE /api/v1/me");
}

#[tokio::test]
async fn a_request_with_an_unknown_a_revoked_or_an_expired_token_is_unauthenticated() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let unknown = owner(&database.url(), "unknown").await;
    let revoked = owner(&database.url(), "revoked").await;
    let expired = owner(&database.url(), "expired").await;
    let valid = owner(&database.url(), "valid").await;
    execute(
        db,
        "DELETE FROM bosun.api_token WHERE user_id = \
         (SELECT id FROM bosun.\"user\" WHERE login = 'unknown')",
    )
    .await;
    execute(
        db,
        "UPDATE bosun.api_token SET revoked_at = now() WHERE user_id = \
         (SELECT id FROM bosun.\"user\" WHERE login = 'revoked')",
    )
    .await;
    execute(
        db,
        "UPDATE bosun.api_token SET expires_at = now() - interval '1 second' WHERE user_id = \
         (SELECT id FROM bosun.\"user\" WHERE login = 'expired')",
    )
    .await;

    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;
    refused(
        address,
        Some(&bearer(&unknown)),
        "the bearer token is not known",
    )
    .await;
    refused(
        address,
        Some(&bearer(&revoked)),
        "the bearer token was revoked",
    )
    .await;
    refused(address, Some(&bearer(&expired)), "the bearer token expired").await;
    let (status, body) = me(address, Some(&bearer(&valid))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user"]["login"], "valid");
}

#[tokio::test]
async fn the_checksum_of_a_token_is_verified_before_any_database_lookup() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "roman").await;
    let proxy = Proxy::start(&database.url()).await;
    let mut server = Launch::serve(&proxy.url(&database.url())).spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;
    assert_eq!(me(address, Some(&bearer(&token))).await.0, 200);

    proxy.stall();
    let looking_up = {
        let authorization = bearer(&token);
        tokio::spawn(async move { me(address, Some(&authorization)).await.0 })
    };
    sleep(Duration::from_millis(300)).await;
    timeout(
        AT_ONCE,
        refused(
            address,
            Some(&bearer(&misspelt(&token, 20))),
            "the bearer token is not a well-formed personal token",
        ),
    )
    .await
    .expect("a token whose checksum fails is refused at once, without the database");
    sleep(Duration::from_millis(700)).await;
    assert!(
        !looking_up.is_finished(),
        "a token whose checksum holds was answered without the database"
    );

    proxy.forward();
    let status = looking_up
        .await
        .expect("the request that looks the token up");
    assert!(matches!(status, 200 | 500), "{status}");
}

#[tokio::test]
async fn a_token_of_a_user_who_is_not_an_owner_is_forbidden() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "someone").await;
    execute(
        database.connection(),
        "UPDATE bosun.\"user\" SET owner = false WHERE login = 'someone'",
    )
    .await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;

    let (status, body) = me(address, Some(&bearer(&token))).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
    assert_eq!(
        body["error"]["message"],
        "`someone` is not an owner of this installation, and only owners may act until roles \
         exist"
    );
}
