use std::num::NonZeroU16;

use bosun_types::id::{ExecutionId, RunId};
use bosun_types::status::{ExecutionStatus, RunStatus};
use sea_orm::prelude::{DateTimeUtc, Uuid};
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait, Value};

use crate::Error;
use crate::periodic::{self, Task};
use crate::row::{self, statement};

const EXECUTIONS: &str = "execution";

const RUNS: &str = "run";

const AGENT_STATUS: &str = "agent.status";

const GIVEN_UP: &str =
    "retention removed the payload of the delivery before the delivery could be processed";

pub struct Round {
    transaction: DatabaseTransaction,
}

impl Round {
    pub async fn end(self) -> Result<(), Error> {
        self.transaction.commit().await?;
        Ok(())
    }
}

pub async fn begin<C: TransactionTrait>(db: &C) -> Result<Option<Round>, Error> {
    let transaction = db.begin().await?;
    if !periodic::try_lock(&transaction, Task::Retention).await? {
        return Ok(None);
    }
    Ok(Some(Round { transaction }))
}

fn days(max: Option<NonZeroU16>) -> Value {
    max.map(|days| i32::from(days.get())).into()
}

fn limit(table: &'static str, limit: u64) -> Result<Value, Error> {
    Ok(row::signed(table, limit)?.into())
}

pub async fn expired_logs<C: ConnectionTrait>(
    db: &C,
    now: DateTimeUtc,
    max: NonZeroU16,
    batch: u64,
) -> Result<Vec<ExecutionId>, Error> {
    db.query_all(statement(
        "SELECT e.id FROM execution e JOIN job j ON j.id = e.job_id \
         JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id \
         JOIN project p ON p.id = r.project_id \
         WHERE e.status = $1 AND e.logs_expired_at IS NULL \
         AND e.completed_at <= $2 - make_interval(days => \
         (SELECT min(least(log_retention, $3)) FROM project)) \
         AND e.completed_at <= $2 - make_interval(days => least(p.log_retention, $3)) \
         ORDER BY e.completed_at, e.id LIMIT $4",
        [
            row::text(EXECUTIONS, ExecutionStatus::Completed)?.into(),
            now.into(),
            days(Some(max)),
            limit(EXECUTIONS, batch)?,
        ],
    ))
    .await?
    .into_iter()
    .map(|found| row::id(EXECUTIONS, found.try_get::<Uuid>("", "id")?))
    .collect()
}

pub async fn expire_logs<C: TransactionTrait>(
    db: &C,
    execution: ExecutionId,
    now: DateTimeUtc,
    max: NonZeroU16,
) -> Result<bool, Error> {
    let id = Uuid::from(execution);
    let transaction = db.begin().await?;
    let still = transaction
        .query_one(statement(
            "SELECT e.id FROM execution e JOIN job j ON j.id = e.job_id \
             JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id \
             JOIN project p ON p.id = r.project_id \
             WHERE e.id = $1 AND e.status = $2 AND e.logs_expired_at IS NULL \
             AND e.completed_at <= $3 - make_interval(days => least(p.log_retention, $4)) \
             FOR NO KEY UPDATE OF e SKIP LOCKED",
            [
                id.into(),
                row::text(EXECUTIONS, ExecutionStatus::Completed)?.into(),
                now.into(),
                days(Some(max)),
            ],
        ))
        .await?;
    if still.is_none() {
        return Ok(false);
    }
    for sql in [
        "INSERT INTO expired_object (key) \
         SELECT o.key FROM step s JOIN log_object o ON o.step_id = s.id WHERE s.execution_id = $1 \
         UNION SELECT m.key FROM step s JOIN step_summary m ON m.step_id = s.id \
         WHERE s.execution_id = $1 ON CONFLICT DO NOTHING",
        "DELETE FROM log_object WHERE step_id IN (SELECT id FROM step WHERE execution_id = $1)",
        "DELETE FROM step_summary WHERE step_id IN (SELECT id FROM step WHERE execution_id = $1)",
        "DELETE FROM log_chunk WHERE execution_id = $1",
    ] {
        transaction.execute(statement(sql, [id.into()])).await?;
    }
    transaction
        .execute(statement(
            "UPDATE execution SET logs_expired_at = $2, \
             log_compacted_at = coalesce(log_compacted_at, $2) WHERE id = $1",
            [id.into(), now.into()],
        ))
        .await?;
    transaction.commit().await?;
    Ok(true)
}

pub async fn expired_runs<C: ConnectionTrait>(
    db: &C,
    now: DateTimeUtc,
    max: Option<NonZeroU16>,
    batch: u64,
) -> Result<Vec<RunId>, Error> {
    db.query_all(statement(
        "SELECT r.id FROM run r JOIN project p ON p.id = r.project_id \
         WHERE r.status = $1 \
         AND r.completed_at <= $2 - make_interval(days => \
         (SELECT min(least(run_retention, $3)) FROM project)) \
         AND r.completed_at <= $2 - make_interval(days => least(p.run_retention, $3)) \
         ORDER BY r.completed_at, r.id LIMIT $4",
        [
            row::text(RUNS, RunStatus::Completed)?.into(),
            now.into(),
            days(max),
            limit(RUNS, batch)?,
        ],
    ))
    .await?
    .into_iter()
    .map(|found| row::id(RUNS, found.try_get::<Uuid>("", "id")?))
    .collect()
}

pub async fn delete_run<C: TransactionTrait>(
    db: &C,
    run: RunId,
    now: DateTimeUtc,
    max: Option<NonZeroU16>,
) -> Result<bool, Error> {
    let id = Uuid::from(run);
    let transaction = db.begin().await?;
    let still = transaction
        .query_one(statement(
            "SELECT r.id FROM run r JOIN project p ON p.id = r.project_id \
             WHERE r.id = $1 AND r.status = $2 \
             AND r.completed_at <= $3 - make_interval(days => least(p.run_retention, $4)) \
             FOR UPDATE OF r SKIP LOCKED",
            [
                id.into(),
                row::text(RUNS, RunStatus::Completed)?.into(),
                now.into(),
                days(max),
            ],
        ))
        .await?;
    if still.is_none() {
        return Ok(false);
    }
    transaction
        .execute(statement(
            "INSERT INTO expired_object (key) \
             SELECT o.key FROM attempt a JOIN job j ON j.attempt_id = a.id \
             JOIN execution e ON e.job_id = j.id JOIN step s ON s.execution_id = e.id \
             JOIN log_object o ON o.step_id = s.id WHERE a.run_id = $1 \
             UNION SELECT m.key FROM attempt a JOIN job j ON j.attempt_id = a.id \
             JOIN execution e ON e.job_id = j.id JOIN step s ON s.execution_id = e.id \
             JOIN step_summary m ON m.step_id = s.id WHERE a.run_id = $1 \
             ON CONFLICT DO NOTHING",
            [id.into()],
        ))
        .await?;
    transaction
        .execute(statement("DELETE FROM run WHERE id = $1", [id.into()]))
        .await?;
    transaction.commit().await?;
    Ok(true)
}

pub async fn remove_payloads<C: ConnectionTrait>(
    db: &C,
    now: DateTimeUtc,
    received_before: DateTimeUtc,
    batch: u64,
) -> Result<u64, Error> {
    Ok(db
        .execute(statement(
            "UPDATE webhook_delivery SET payload = 'null'::jsonb, \
             processed_at = coalesce(processed_at, $1), \
             last_error = CASE WHEN processed_at IS NULL THEN $2 ELSE last_error END \
             WHERE id IN (SELECT id FROM webhook_delivery \
             WHERE payload <> 'null'::jsonb AND received_at <= $3 \
             ORDER BY received_at, id LIMIT $4 FOR UPDATE SKIP LOCKED)",
            [
                now.into(),
                GIVEN_UP.into(),
                received_before.into(),
                limit("webhook_delivery", batch)?,
            ],
        ))
        .await?
        .rows_affected())
}

pub async fn remove_agent_events<C: ConnectionTrait>(
    db: &C,
    before: DateTimeUtc,
    batch: u64,
) -> Result<u64, Error> {
    Ok(db
        .execute(statement(
            "DELETE FROM stream_event WHERE seq IN (SELECT seq FROM stream_event \
             WHERE kind = $1 AND at <= $2 ORDER BY at, seq LIMIT $3)",
            [
                AGENT_STATUS.into(),
                before.into(),
                limit("stream_event", batch)?,
            ],
        ))
        .await?
        .rows_affected())
}

pub async fn expired_objects<C: ConnectionTrait>(db: &C, batch: u64) -> Result<Vec<String>, Error> {
    db.query_all(statement(
        "SELECT key FROM expired_object ORDER BY key LIMIT $1",
        [limit("expired_object", batch)?],
    ))
    .await?
    .into_iter()
    .map(|found| Ok(found.try_get::<String>("", "key")?))
    .collect()
}

pub async fn forget_objects<C: ConnectionTrait>(db: &C, keys: &[String]) -> Result<(), Error> {
    db.execute(statement(
        "DELETE FROM expired_object WHERE key = ANY($1)",
        [keys.to_vec().into()],
    ))
    .await?;
    Ok(())
}
