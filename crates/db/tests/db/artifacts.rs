use bosun_db::artifacts::{self, Done, Finalizing, New, Registration};
use bosun_db::retention;
use bosun_testkit::TestDatabase;
use bosun_types::id::{AttemptId, ExecutionId};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection};

use crate::{rows, strings};

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(Utc::now().timestamp(), 0).expect("a time")
}

async fn create(db: &DatabaseConnection, name: &str, created: DateTime<Utc>) -> i64 {
    artifacts::create(
        db,
        &New {
            attempt: AttemptId::try_from(rows::id(rows::ATTEMPT)).expect("an attempt"),
            execution: ExecutionId::try_from(rows::id(rows::EXECUTION)).expect("an execution"),
            name,
            content_type: "application/zip",
            created_at: created,
            expires_at: created + TimeDelta::days(1),
        },
    )
    .await
    .expect("an insert")
    .expect("a free name")
    .id
}

async fn queued(db: &DatabaseConnection) -> Vec<String> {
    strings(
        db,
        "SELECT key AS value FROM expired_object WHERE key NOT LIKE 'summaries/%' AND key NOT LIKE 'logs/%' ORDER BY key",
    )
    .await
}

#[tokio::test]
async fn a_name_is_taken_once_per_attempt_and_an_execution_outside_the_attempt_is_refused() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let first = create(db, "dist", now()).await;
    let again = artifacts::create(
        db,
        &New {
            attempt: AttemptId::try_from(rows::id(rows::ATTEMPT)).expect("an attempt"),
            execution: ExecutionId::try_from(rows::id(rows::EXECUTION)).expect("an execution"),
            name: "dist",
            content_type: "application/zip",
            created_at: now(),
            expires_at: now(),
        },
    )
    .await
    .expect("an insert");
    assert!(again.is_none());
    let stored = artifacts::find(db, first)
        .await
        .expect("a query")
        .expect("the artifact");
    assert_eq!(
        stored.key,
        format!("artifacts/{}/{first}", stored.project_id)
    );
    let elsewhere = artifacts::create(
        db,
        &New {
            attempt: AttemptId::try_from(rows::id(rows::ATTEMPT)).expect("an attempt"),
            execution: ExecutionId::try_from(rows::id(rows::AGENT)).expect("an id"),
            name: "other",
            content_type: "application/zip",
            created_at: now(),
            expires_at: now(),
        },
    )
    .await;
    assert!(elsewhere.is_err(), "{elsewhere:?}");
}

#[tokio::test]
async fn racing_registrations_never_pass_the_limit_and_a_known_block_always_does() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let id = create(db, "dist", now()).await;
    let attempts: Vec<_> = (0..12)
        .map(|number| {
            let db = db.clone();
            tokio::spawn(async move {
                artifacts::register_block(&db, id, &format!("block-{number}"), 5)
                    .await
                    .expect("a registration")
            })
        })
        .collect();
    let mut accepted = Vec::new();
    for (number, attempt) in attempts.into_iter().enumerate() {
        if attempt.await.expect("a task") == Registration::Accepted {
            accepted.push(format!("block-{number}"));
        }
    }
    assert_eq!(accepted.len(), 5, "{accepted:?}");
    for block in &accepted {
        assert_eq!(
            artifacts::register_block(db, id, block, 5)
                .await
                .expect("a registration"),
            Registration::Accepted
        );
    }
    assert_eq!(
        artifacts::register_block(db, id, "block-new", 5)
            .await
            .expect("a registration"),
        Registration::Full
    );
    assert_eq!(
        artifacts::register_block(db, id + 1_000_000, "block-0", 5)
            .await
            .expect("a registration"),
        Registration::Missing
    );
    let counted = strings(
        db,
        &format!(
            "SELECT (blocks = (SELECT count(*) FROM artifact_block WHERE artifact_id = {id}))::text \
             AS value FROM artifact WHERE id = {id}"
        ),
    )
    .await;
    assert_eq!(counted, ["true"]);
}

#[tokio::test]
async fn finalising_twice_with_the_same_values_is_idempotent_and_drops_the_staged_blocks() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let id = create(db, "dist", now()).await;
    artifacts::register_block(db, id, "block-0", 5)
        .await
        .expect("a registration");
    let done = Done {
        size: 12,
        sha256: Some([7; 32]),
        at: now(),
    };
    assert!(matches!(
        artifacts::finalize(db, id, &done)
            .await
            .expect("a finalise"),
        Finalizing::Done(_)
    ));
    assert!(matches!(
        artifacts::finalize(db, id, &done)
            .await
            .expect("a finalise"),
        Finalizing::Done(_)
    ));
    assert_eq!(
        artifacts::register_block(db, id, "block-1", 5)
            .await
            .expect("a registration"),
        Registration::Missing
    );
    let staged = strings(
        db,
        &format!("SELECT count(*)::text AS value FROM artifact_block WHERE artifact_id = {id}"),
    )
    .await;
    assert_eq!(staged, ["0"]);
    let other = Done { size: 13, ..done };
    assert!(matches!(
        artifacts::finalize(db, id, &other)
            .await
            .expect("a finalise"),
        Finalizing::Differs
    ));
    assert!(matches!(
        artifacts::finalize(db, id + 1_000_000, &other)
            .await
            .expect("a finalise"),
        Finalizing::Missing
    ));
    let left = strings(
        db,
        &format!("SELECT block AS value FROM artifact_block WHERE artifact_id = {id}"),
    )
    .await;
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn retention_queues_the_object_and_the_staged_blocks_of_what_it_removes() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    db.execute_unprepared("DELETE FROM artifact")
        .await
        .expect("a clean-up");
    let start = now();
    let old = create(db, "old", start - TimeDelta::days(3)).await;
    let abandoned = create(db, "abandoned", start - TimeDelta::days(3)).await;
    let fresh = create(db, "fresh", start).await;
    let done = Done {
        size: 1,
        sha256: None,
        at: start - TimeDelta::days(3),
    };
    for id in [old, fresh] {
        artifacts::finalize(db, id, &done)
            .await
            .expect("a finalise");
    }
    db.execute_unprepared(&format!(
        "UPDATE artifact SET expires_at = '{}' WHERE id = {fresh}",
        (start + TimeDelta::days(30)).to_rfc3339()
    ))
    .await
    .expect("an expiry");
    artifacts::register_block(db, abandoned, "QUJD/+==", 5)
        .await
        .expect("a registration");

    assert_eq!(
        retention::expired_artifacts(db, start, 10)
            .await
            .expect("a query"),
        [old]
    );
    assert_eq!(
        retention::abandoned_uploads(db, start - TimeDelta::days(1), 10)
            .await
            .expect("a query"),
        [abandoned]
    );
    assert!(
        retention::expire_artifact(db, old, start)
            .await
            .expect("a removal")
    );
    assert!(
        !retention::expire_artifact(db, old, start)
            .await
            .expect("a removal")
    );
    assert!(
        !retention::expire_artifact(db, fresh, start)
            .await
            .expect("a removal")
    );
    assert!(
        retention::abandon_upload(db, abandoned, start - TimeDelta::days(1))
            .await
            .expect("a removal")
    );

    let project = rows::id(rows::PROJECT);
    assert_eq!(
        queued(db).await,
        [
            format!("artifacts/{project}/{old}"),
            format!("artifacts/{project}/{abandoned}"),
            format!("blocks/{abandoned}/51554a442f2b3d3d"),
        ]
    );
    let left = strings(db, "SELECT name AS value FROM artifact ORDER BY name").await;
    assert_eq!(left, ["fresh"]);
}

#[tokio::test]
async fn deleting_a_run_queues_the_objects_and_the_staged_blocks_of_its_artifacts() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    db.execute_unprepared("DELETE FROM artifact")
        .await
        .expect("a clean-up");
    let id = create(db, "dist", now()).await;
    artifacts::register_block(db, id, "QUJD", 5)
        .await
        .expect("a registration");
    let finished = create(db, "done", now()).await;
    let done = Done {
        size: 1,
        sha256: None,
        at: now(),
    };
    artifacts::finalize(db, finished, &done)
        .await
        .expect("a finalise");
    db.execute_unprepared(
        "UPDATE run SET status = 'completed', conclusion = 'success', \
         completed_at = now() - interval '3 days'",
    )
    .await
    .expect("a completed run");
    let run = bosun_types::id::RunId::try_from(rows::id(rows::RUN)).expect("a run");
    let max = std::num::NonZeroU16::new(1);
    assert!(
        retention::delete_run(db, run, now(), max)
            .await
            .expect("a deletion")
    );
    let project = rows::id(rows::PROJECT);
    assert_eq!(
        queued(db).await,
        [
            format!("artifacts/{project}/{id}"),
            format!("artifacts/{project}/{finished}"),
            format!("blocks/{id}/51554a44"),
        ]
    );
    let left = strings(db, "SELECT name AS value FROM artifact").await;
    assert!(left.is_empty(), "{left:?}");
}
