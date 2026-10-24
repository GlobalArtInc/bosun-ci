use bosun_db::idempotency::{self, Stored};
use bosun_testkit::TestDatabase;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value, json};

const PRINCIPAL: &str = "user:0192f3a4-5b6c-7d8e-9f01-000000000003";

fn stored(byte: u8, response: Value) -> Stored {
    Stored {
        request_sha256: vec![byte; 32],
        status: 201,
        response,
    }
}

async fn found(
    db: &DatabaseConnection,
    principal: &str,
    key: &str,
) -> Option<(Vec<u8>, u16, Value)> {
    idempotency::find(db, principal, key)
        .await
        .expect("a lookup")
        .map(|stored| (stored.request_sha256, stored.status, stored.response))
}

#[tokio::test]
async fn a_recorded_response_is_found_by_its_principal_and_key_and_never_replaced_while_kept() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    assert_eq!(found(db, PRINCIPAL, "dispatch-1").await, None);

    assert!(
        idempotency::record(db, PRINCIPAL, "dispatch-1", stored(1, json!({ "run": 1 })))
            .await
            .expect("a record")
    );
    assert_eq!(
        found(db, PRINCIPAL, "dispatch-1").await,
        Some((vec![1; 32], 201, json!({ "run": 1 })))
    );
    assert!(
        !idempotency::record(db, PRINCIPAL, "dispatch-1", stored(2, json!({ "run": 2 })))
            .await
            .expect("a second record")
    );
    assert_eq!(
        found(db, PRINCIPAL, "dispatch-1").await,
        Some((vec![1; 32], 201, json!({ "run": 1 })))
    );
    assert_eq!(found(db, "user:someone-else", "dispatch-1").await, None);
    assert_eq!(found(db, PRINCIPAL, "dispatch-2").await, None);
}

#[tokio::test]
async fn a_response_older_than_24_hours_is_gone_and_its_key_can_be_used_again() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    assert!(
        idempotency::record(db, PRINCIPAL, "dispatch-1", stored(1, json!({ "run": 1 })))
            .await
            .expect("a record")
    );
    db.execute_unprepared(
        "UPDATE bosun.idempotency_key SET created_at = now() - interval '24 hours 1 second'",
    )
    .await
    .expect("an older record");
    assert_eq!(found(db, PRINCIPAL, "dispatch-1").await, None);

    assert!(
        idempotency::record(db, PRINCIPAL, "dispatch-1", stored(2, json!({ "run": 2 })))
            .await
            .expect("a record over the old one")
    );
    assert_eq!(
        found(db, PRINCIPAL, "dispatch-1").await,
        Some((vec![2; 32], 201, json!({ "run": 2 })))
    );
}
