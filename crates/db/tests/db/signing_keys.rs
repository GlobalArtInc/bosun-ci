use bosun_db::secrets::Sealed;
use bosun_db::signing_keys::{self, NewSigningKey};
use bosun_testkit::TestDatabase;
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, TransactionTrait};

fn sealed() -> Sealed {
    Sealed {
        ciphertext: vec![1; 20],
        data_key: vec![2; 60],
        kek_id: "0123456789abcdef".to_owned(),
    }
}

fn new(id: Uuid, byte: u8, sealed: &Sealed, created_at: DateTime<Utc>) -> NewSigningKey<'_> {
    NewSigningKey {
        id,
        public_key: [byte; 32],
        sealed,
        created_at,
    }
}

#[tokio::test]
async fn the_table_holds_one_live_key_and_a_retired_key_keeps_its_public_key_alone() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let sealed = sealed();
    let now = DateTime::from_timestamp(Utc::now().timestamp(), 0).expect("a time");
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    signing_keys::insert(db, &new(first, 1, &sealed, now))
        .await
        .expect("the first key");
    assert!(
        signing_keys::insert(db, &new(second, 2, &sealed, now))
            .await
            .is_err(),
        "a second live key was accepted"
    );
    assert!(
        db.execute_unprepared(&format!(
            "UPDATE signing_key SET retired_at = now() WHERE id = '{first}'"
        ))
        .await
        .is_err(),
        "a retired key kept its private key"
    );
    assert!(
        signing_keys::retire(db, first, now + TimeDelta::days(30))
            .await
            .expect("a retirement")
    );
    assert!(
        !signing_keys::retire(db, first, now + TimeDelta::days(31))
            .await
            .expect("a second retirement")
    );
    assert!(signing_keys::live(db).await.expect("a query").is_none());
    signing_keys::insert(db, &new(second, 2, &sealed, now + TimeDelta::days(30)))
        .await
        .expect("the second key");
    assert_eq!(
        signing_keys::public_keys(db)
            .await
            .expect("the public keys"),
        vec![[1; 32], [2; 32]]
    );
    let transaction = db.begin().await.expect("a transaction");
    assert!(
        signing_keys::try_lock(&transaction)
            .await
            .expect("the lock")
    );
    let other = bosun_db::connect(&database.url(), 2)
        .await
        .expect("a second pool");
    let rival = other.begin().await.expect("a transaction");
    assert!(
        !signing_keys::try_lock(&rival).await.expect("the lock"),
        "two transactions held the lock of the signing key"
    );
    rival.rollback().await.expect("a rollback");
    transaction.rollback().await.expect("a rollback");
    other.close().await.expect("the second pool closes");
    assert_eq!(
        signing_keys::purge(db, now + TimeDelta::days(30))
            .await
            .expect("a purge"),
        0
    );
    assert_eq!(
        signing_keys::purge(db, now + TimeDelta::days(31))
            .await
            .expect("a purge"),
        1
    );
    assert_eq!(
        signing_keys::public_keys(db)
            .await
            .expect("the public keys"),
        vec![[2; 32]]
    );
}
