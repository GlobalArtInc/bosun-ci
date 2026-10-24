use bosun_db::Error;
use bosun_db::entity::{api_token, user};
use bosun_db::tokens::{self, NewToken};
use bosun_db::users;
use bosun_testkit::TestDatabase;
use sea_orm::{EntityTrait, PaginatorTrait};
use tokio::task::JoinSet;

use crate::rows;

const EXPIRES_AT: &str = "2027-10-24T18:34:50Z";

fn new_token(byte: u8) -> NewToken {
    NewToken {
        name: "bootstrap".to_owned(),
        sha256: vec![byte; 32],
        expires_at: rows::at(EXPIRES_AT),
    }
}

#[tokio::test]
async fn an_owner_is_found_by_the_hash_of_its_token() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let owner = users::create_owner(db, "roman", new_token(1))
        .await
        .expect("the owner");
    assert_eq!(owner.login, "roman");
    assert_eq!(owner.name, "roman");
    assert_eq!(owner.email, None);
    assert!(owner.owner);

    let found = tokens::find_personal(db, &[1; 32])
        .await
        .expect("a lookup")
        .expect("the token");
    assert_eq!(found.user, owner);
    assert_eq!(found.expires_at, rows::at(EXPIRES_AT));
    assert_eq!(found.revoked_at, None);
    let stored = api_token::Entity::find()
        .one(db)
        .await
        .expect("a token row")
        .expect("the token row");
    assert_eq!(stored.name, "bootstrap");
    assert_eq!(stored.user_id, owner.id.as_uuid());

    assert!(
        tokens::find_personal(db, &[2; 32])
            .await
            .expect("a lookup")
            .is_none()
    );
}

#[tokio::test]
async fn a_login_that_exists_creates_neither_a_user_nor_a_token() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    users::create_owner(db, "roman", new_token(1))
        .await
        .expect("the owner");
    let again = users::create_owner(db, "roman", new_token(2)).await;
    assert!(
        matches!(&again, Err(Error::LoginTaken(login)) if login == "roman"),
        "{again:?}"
    );
    assert_eq!(user::Entity::find().count(db).await.expect("a count"), 1);
    assert_eq!(
        api_token::Entity::find().count(db).await.expect("a count"),
        1
    );
}

#[tokio::test]
async fn concurrent_bootstraps_create_one_owner() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let pool = bosun_db::connect(&database.url(), 8)
        .await
        .expect("a pool of eight connections");
    let mut attempts = JoinSet::new();
    for byte in 0..8 {
        let pool = pool.clone();
        attempts
            .spawn(async move { users::bootstrap_owner(&pool, "roman", new_token(byte)).await });
    }
    let mut created = Vec::new();
    while let Some(outcome) = attempts.join_next().await {
        created.extend(outcome.expect("a bootstrap task").expect("a bootstrap"));
    }
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(user::Entity::find().count(db).await.expect("a count"), 1);
    assert_eq!(
        api_token::Entity::find().count(db).await.expect("a count"),
        1
    );

    let later = users::bootstrap_owner(db, "someone", new_token(9))
        .await
        .expect("a bootstrap");
    assert_eq!(later, None);
    assert_eq!(user::Entity::find().count(db).await.expect("a count"), 1);
    pool.close().await.expect("the pool closes");
}
