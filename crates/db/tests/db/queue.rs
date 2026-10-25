use std::time::Duration;

use bosun_db::queue;
use bosun_testkit::TestDatabase;
use bosun_types::id::{ExecutionId, JobId};
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::rows::{self, EXECUTION, TEST};

const LEASE_LOCK: i64 = i64::from_be_bytes(*b"bosunlse");

const TIMEOUT_LOCK: i64 = i64::from_be_bytes(*b"bosunqto");

async fn execute<C: ConnectionTrait>(db: &C, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

#[tokio::test]
async fn the_lease_expiry_finds_assigned_executions_past_their_lease() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    assert_eq!(
        queue::expired_leases(db).await.expect("an expiry"),
        Some(Vec::new()),
        "an execution in progress was taken for a lapsed assignment"
    );
    execute(
        db,
        &format!(
            "UPDATE bosun.execution SET status = 'assigned' WHERE id = '{}'",
            rows::id(EXECUTION)
        ),
    )
    .await;
    let execution: ExecutionId = rows::id(EXECUTION).try_into().expect("an execution id");
    assert_eq!(
        queue::expired_leases(db).await.expect("an expiry"),
        Some(vec![execution])
    );
    execute(
        db,
        &format!(
            "UPDATE bosun.execution SET lease_until = now() + interval '1 minute' WHERE id = '{}'",
            rows::id(EXECUTION)
        ),
    )
    .await;
    assert_eq!(
        queue::expired_leases(db).await.expect("an expiry"),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn the_queue_timeout_finds_jobs_queued_longer_than_max_queue_time() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let hour = Duration::from_secs(3600);
    assert_eq!(
        queue::overdue(db, hour).await.expect("a timeout"),
        Some(Vec::new())
    );
    execute(
        db,
        &format!(
            "UPDATE bosun.job SET status = 'queued', queued_at = now() - interval '2 hours' \
             WHERE id = '{}'",
            rows::id(TEST)
        ),
    )
    .await;
    let test: JobId = rows::id(TEST).try_into().expect("a job id");
    assert_eq!(
        queue::overdue(db, hour).await.expect("a timeout"),
        Some(vec![test])
    );
    assert_eq!(
        queue::overdue(db, 3 * hour).await.expect("a timeout"),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn one_lease_expiry_and_one_queue_timeout_run_at_a_time_each_under_its_own_lock() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let hour = Duration::from_secs(3600);
    for (lock, expiry_held) in [(LEASE_LOCK, true), (TIMEOUT_LOCK, false)] {
        let holder = db.begin().await.expect("a transaction");
        execute(&holder, &format!("SELECT pg_advisory_xact_lock({lock})")).await;
        assert_eq!(
            queue::expired_leases(db)
                .await
                .expect("an expiry")
                .is_none(),
            expiry_held,
            "the lease expiry ignored its lock or took the other one"
        );
        assert_eq!(
            queue::overdue(db, hour).await.expect("a timeout").is_none(),
            !expiry_held,
            "the queue timeout ignored its lock or took the other one"
        );
        holder.rollback().await.expect("the lock is released");
        assert!(
            queue::expired_leases(db)
                .await
                .expect("an expiry")
                .is_some()
        );
        assert!(queue::overdue(db, hour).await.expect("a timeout").is_some());
    }
}
