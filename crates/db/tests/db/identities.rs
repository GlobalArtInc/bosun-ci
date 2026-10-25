use bosun_db::identities::{self, NewIdentity};
use bosun_db::{Error, organisations, teams, users};
use bosun_testkit::TestDatabase;

use crate::rows;

const ISSUER: &str = "https://sso.example.org/realms/main";

fn dana<'a>(subject: &'a str, login: &'a str) -> NewIdentity<'a> {
    NewIdentity {
        issuer: ISSUER,
        subject,
        login,
        name: "Dana Scully",
        email: Some("dana@example.org"),
    }
}

#[tokio::test]
async fn a_linked_identity_finds_its_user_and_a_taken_login_links_nothing() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let created = identities::create_user(db, dana("f81d4fae", "dana"))
        .await
        .expect("a user");
    assert_eq!(created.login, "dana");
    assert_eq!(created.name, "Dana Scully");
    assert_eq!(created.email.as_deref(), Some("dana@example.org"));
    assert!(!created.owner);
    let credentials = users::credentials(db, created.id)
        .await
        .expect("a lookup")
        .expect("the user");
    assert_eq!(credentials.password_hash, None);
    assert!(!credentials.password_change_required);
    assert_eq!(
        identities::find_user(db, ISSUER, "f81d4fae")
            .await
            .expect("a lookup"),
        Some(created.clone())
    );
    assert_eq!(
        identities::find_user(db, "https://elsewhere.example.org", "f81d4fae")
            .await
            .expect("a lookup"),
        None
    );
    assert!(matches!(
        identities::create_user(db, dana("7dec11d0", "dana")).await,
        Err(Error::LoginTaken(login)) if login == "dana"
    ));
    assert_eq!(
        identities::find_user(db, ISSUER, "7dec11d0")
            .await
            .expect("a lookup"),
        None
    );
    assert!(users::delete(db, created.id).await.expect("a deletion"));
    assert_eq!(
        identities::find_user(db, ISSUER, "f81d4fae")
            .await
            .expect("a lookup"),
        None
    );
}

#[tokio::test]
async fn a_state_is_spent_once() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let expires_at = rows::at("2026-10-25T12:10:00Z");
    assert!(
        identities::spend_state(db, &[1; 32], expires_at)
            .await
            .expect("a state")
    );
    assert!(
        !identities::spend_state(db, &[1; 32], expires_at)
            .await
            .expect("a state")
    );
    assert!(
        identities::spend_state(db, &[2; 32], expires_at)
            .await
            .expect("a state")
    );
}

#[tokio::test]
async fn ensure_creates_a_team_named_by_its_slug_once() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let organisation = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let first = teams::ensure(db, organisation.id, "keel")
        .await
        .expect("a team");
    assert_eq!(first.slug, "keel");
    assert_eq!(first.name, "keel");
    let again = teams::ensure(db, organisation.id, "keel")
        .await
        .expect("the team");
    assert_eq!(again, first);
}
