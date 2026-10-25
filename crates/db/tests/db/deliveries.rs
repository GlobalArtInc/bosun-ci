use std::time::Duration;

use bosun_db::deliveries::{self, NewDelivery};
use bosun_db::entity::webhook_delivery;
use bosun_testkit::TestDatabase;
use sea_orm::{DatabaseConnection, EntityTrait, QueryOrder, TransactionTrait};
use serde_json::json;

async fn store(db: &DatabaseConnection, delivery: &str) -> bool {
    deliveries::store(
        db,
        NewDelivery {
            delivery,
            event: "installation",
            payload: &json!({ "action": "created", "delivery": delivery }),
        },
    )
    .await
    .expect("a stored delivery")
}

async fn rows(db: &DatabaseConnection) -> Vec<webhook_delivery::Model> {
    webhook_delivery::Entity::find()
        .order_by_asc(webhook_delivery::Column::Id)
        .all(db)
        .await
        .expect("the deliveries")
}

#[tokio::test]
async fn a_delivery_is_stored_once_however_often_it_arrives() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    assert!(store(db, "72d3162e-cc78-11e3-81ab-4c9367dc0958").await);
    assert!(!store(db, "72d3162e-cc78-11e3-81ab-4c9367dc0958").await);
    assert!(store(db, "72d3162e-cc78-11e3-81ab-4c9367dc0959").await);
    let stored = rows(db).await;
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].delivery, "72d3162e-cc78-11e3-81ab-4c9367dc0958");
    assert_eq!(stored[0].event, "installation");
    assert_eq!(
        stored[0].payload,
        json!({ "action": "created", "delivery": "72d3162e-cc78-11e3-81ab-4c9367dc0958" })
    );
    assert_eq!(stored[0].attempts, 0);
    assert_eq!(stored[0].processed_at, None);
}

#[tokio::test]
async fn claims_at_once_take_different_deliveries_and_skip_locked_ones() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    store(db, "first").await;
    store(db, "second").await;
    let left = db.begin().await.expect("a transaction");
    let right = db.begin().await.expect("a transaction");
    let third = db.begin().await.expect("a transaction");
    let first = deliveries::claim(&left)
        .await
        .expect("a claim")
        .expect("a delivery");
    let second = deliveries::claim(&right)
        .await
        .expect("a claim")
        .expect("a delivery");
    assert_eq!(
        (first.delivery.as_str(), second.delivery.as_str()),
        ("first", "second")
    );
    assert_eq!(deliveries::claim(&third).await.expect("a claim"), None);
    deliveries::finish(&left, first.id, None)
        .await
        .expect("a finished delivery");
    left.commit().await.expect("a commit");
    right.rollback().await.expect("a rollback");
    let again = deliveries::claim(&third)
        .await
        .expect("a claim")
        .expect("the released delivery");
    assert_eq!(again.delivery, "second");
    third.rollback().await.expect("a rollback");

    let stored = rows(db).await;
    assert!(stored[0].processed_at.is_some());
    assert_eq!(stored[0].attempts, 1);
    assert_eq!(stored[0].last_error, None);
    assert_eq!(stored[1].processed_at, None);
}

#[tokio::test]
async fn a_postponed_delivery_waits_and_a_finished_one_is_never_claimed_again() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    store(db, "flaky").await;
    let transaction = db.begin().await.expect("a transaction");
    let claimed = deliveries::claim(&transaction)
        .await
        .expect("a claim")
        .expect("a delivery");
    assert_eq!(claimed.attempts, 0);
    deliveries::postpone(
        &transaction,
        claimed.id,
        "the forge could not be reached",
        Duration::from_secs(3600),
    )
    .await
    .expect("a postponed delivery");
    transaction.commit().await.expect("a commit");

    let transaction = db.begin().await.expect("a transaction");
    assert_eq!(
        deliveries::claim(&transaction).await.expect("a claim"),
        None
    );
    transaction.rollback().await.expect("a rollback");
    let postponed = rows(db).await.remove(0);
    assert_eq!(postponed.attempts, 1);
    assert_eq!(
        postponed.last_error.as_deref(),
        Some("the forge could not be reached")
    );
    let wait = postponed.next_attempt_at - postponed.received_at;
    assert!(wait.num_minutes() >= 59, "{wait}");

    store(db, "refused").await;
    let transaction = db.begin().await.expect("a transaction");
    let refused = deliveries::claim(&transaction)
        .await
        .expect("a claim")
        .expect("a delivery");
    assert_eq!(refused.delivery, "refused");
    deliveries::finish(
        &transaction,
        refused.id,
        Some("the payload has no installation"),
    )
    .await
    .expect("a finished delivery");
    transaction.commit().await.expect("a commit");
    let transaction = db.begin().await.expect("a transaction");
    assert_eq!(
        deliveries::claim(&transaction).await.expect("a claim"),
        None
    );
    let finished = rows(db).await.remove(1);
    assert!(finished.processed_at.is_some());
    assert_eq!(
        finished.last_error.as_deref(),
        Some("the payload has no installation")
    );
}
