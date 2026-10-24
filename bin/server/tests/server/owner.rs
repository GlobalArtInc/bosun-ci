use bosun_testkit::TestDatabase;
use sea_orm::ConnectionTrait;
use serde_json::Value;

use crate::{Launch, errors, me, message, owner, strings};

const CREATED: &str = "created the owner and its first personal token";

const BOOTSTRAPPED: &str =
    "created the owner from bootstrap.admin; its token is shown only this once";

const ROMAN: &str = "roman roman t t bootstrap t t t";

const SKIPPED: &str = "the database has users already, so bootstrap.admin creates no owner";

async fn stored<C: ConnectionTrait>(db: &C, token: &str) -> Vec<String> {
    strings(
        db,
        &format!(
            "SELECT concat_ws(' ', u.login, u.name, u.email IS NULL, u.owner, t.name, \
             t.sha256 = sha256(convert_to('{token}', 'UTF8')), t.revoked_at IS NULL, \
             abs(extract(epoch FROM t.expires_at - t.created_at) - 365 * 86400) < 60) AS value \
             FROM bosun.api_token t JOIN bosun.\"user\" u ON u.id = t.user_id ORDER BY u.login"
        ),
    )
    .await
}

fn bootstrapped_tokens(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| message(line) == BOOTSTRAPPED)
        .map(|line| line["token"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn admin_create_migrates_creates_the_owner_and_prints_only_its_token() {
    let database = TestDatabase::empty().await;
    let finished = Launch::create_owner(&database.url(), "roman")
        .finish()
        .await;
    assert!(finished.status.success(), "{finished:#?}");

    let lines: Vec<&str> = finished.stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{finished:#?}");
    let token = lines[0];
    assert!(token.starts_with("bst_"), "{token}");
    assert_eq!(token.len(), 53, "{token}");
    assert!(
        finished
            .logs
            .iter()
            .any(|line| message(line) == "applied a migration"),
        "{finished:#?}"
    );
    let created = finished
        .logs
        .iter()
        .find(|line| message(line) == CREATED)
        .unwrap_or_else(|| panic!("no line that the owner was created: {finished:#?}"));
    assert_eq!(created["login"], "roman");
    assert!(
        !finished
            .logs
            .iter()
            .any(|line| line.to_string().contains(token)),
        "the token reached the log: {finished:#?}"
    );
    assert_eq!(stored(database.connection(), token).await, [ROMAN]);

    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;
    let (status, body) = me(address, Some(&format!("Bearer {token}"))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user"]["login"], "roman");
    assert_eq!(body["user"]["name"], "roman");
    assert_eq!(body["user"]["email"], Value::Null);
    assert_eq!(body["user"]["owner"], true);
    assert!(body["user"]["id"].is_string(), "{body}");
    assert!(body["user"]["created_at"].is_string(), "{body}");
}

#[tokio::test]
async fn admin_create_refuses_a_login_that_exists_or_is_not_a_login() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "roman").await;

    for (login, expected) in [
        (
            "roman",
            "could not create the owner: a user with the login `roman` already exists",
        ),
        ("Roman X", "`Roman X` is not a login: a login is 1 to 64"),
        ("", "`` is not a login"),
    ] {
        let finished = Launch::create_owner(&database.url(), login).finish().await;
        assert!(!finished.status.success(), "{finished:#?}");
        assert_eq!(finished.stdout, "", "{finished:#?}");
        assert!(
            errors(&finished.logs)
                .iter()
                .any(|error| error.contains(expected)),
            "expected an error containing `{expected}`: {finished:#?}"
        );
    }
    assert_eq!(stored(database.connection(), &token).await, [ROMAN]);
}

#[tokio::test]
async fn two_servers_started_at_once_with_a_bootstrap_admin_create_one_owner_and_show_one_token() {
    let database = TestDatabase::empty().await;
    let mut first = Launch::serve(&database.url())
        .env("BOSUN_BOOTSTRAP_ADMIN", "roman")
        .spawn();
    let mut second = Launch::serve(&database.url())
        .env("BOSUN_BOOTSTRAP_ADMIN", "roman")
        .spawn();
    let outcome = |line: &Value| message(line) == BOOTSTRAPPED || message(line) == SKIPPED;
    let (one, other) = tokio::join!(
        first.expect("what bootstrap.admin did", outcome),
        second.expect("what bootstrap.admin did", outcome),
    );
    let mut outcomes = [message(&one), message(&other)];
    outcomes.sort();
    assert_eq!(outcomes, [BOOTSTRAPPED, SKIPPED]);
    let created = if message(&one) == BOOTSTRAPPED {
        &one
    } else {
        &other
    };
    assert_eq!(created["login"], "roman");
    assert!(created["expires_at"].is_string(), "{created}");
    let token = created["token"].as_str().expect("the token").to_owned();
    assert_eq!(stored(database.connection(), &token).await, [ROMAN]);

    let (one, other) = tokio::join!(first.listening(), second.listening());
    for address in [one, other] {
        let (status, body) = me(address, Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["user"]["login"], "roman");
    }

    first.terminate();
    second.terminate();
    let (first_status, first_lines) = first.exit().await;
    let (second_status, second_lines) = second.exit().await;
    assert!(first_status.success(), "{first_lines:#?}");
    assert!(second_status.success(), "{second_lines:#?}");
    let lines: Vec<Value> = first_lines.into_iter().chain(second_lines).collect();
    assert_eq!(bootstrapped_tokens(&lines), [token], "{lines:#?}");
}

#[tokio::test]
async fn a_bootstrap_admin_creates_no_owner_when_the_database_has_users() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "roman").await;

    let mut server = Launch::serve(&database.url())
        .env("BOSUN_BOOTSTRAP_ADMIN", "someone")
        .spawn();
    let skipped = server
        .expect("that bootstrap.admin creates no owner", |line| {
            message(line) == SKIPPED
        })
        .await;
    assert_eq!(skipped["login"], "someone");
    let address = server.listening().await;
    let (status, body) = me(address, Some(&format!("Bearer {token}"))).await;
    assert_eq!(status, 200, "{body}");
    server.terminate();
    let (_, lines) = server.exit().await;
    assert!(bootstrapped_tokens(&lines).is_empty(), "{lines:#?}");
    assert_eq!(stored(database.connection(), &token).await, [ROMAN]);
}
