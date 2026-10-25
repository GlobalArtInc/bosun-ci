use std::time::Duration;

use bosun_types::id::{AgentId, AttemptId, ExecutionId, RunId, StepId};
use bosun_types::status::StepStatus;
use sea_orm::prelude::{DateTimeUtc, Uuid};
use sea_orm::sqlx::postgres::PgRow;
use sea_orm::sqlx::{self, Postgres, Row, Transaction};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};

use crate::periodic::{self, Task};
use crate::row::statement;
use crate::{Error, row, sqlx_error};

pub(crate) const CHANNEL: &str = "bosun_log";

const EXECUTIONS: &str = "execution";

const CHUNKS: &str = "log_chunk";

const OBJECTS: &str = "log_object";

const CURSOR: &str = "log_chunks";

pub struct Chunk<'a> {
    pub agent: AgentId,
    pub execution: ExecutionId,
    pub step: u32,
    pub seq: u64,
    pub lines: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    Stored,
    Duplicate,
    Dropped { first: bool },
    Closed,
    NoExecution,
    OtherAgent,
    NoStep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub execution_id: ExecutionId,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub completed_at: Option<DateTimeUtc>,
    pub log: Log,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Log {
    Live(Vec<StoredChunk>),
    Compacted(Option<StoredObject>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChunk {
    pub seq: u64,
    pub step_id: StepId,
    pub data: Vec<u8>,
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub key: String,
    pub size: u64,
    pub lines: u64,
    pub frames: Vec<u64>,
}

pub enum Compaction {
    Started(Compactor),
    Skipped(Compacted),
}

pub struct Compactor {
    transaction: Transaction<'static, Postgres>,
    execution: Uuid,
    read: u64,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub steps: Vec<StepLog>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepLog {
    pub id: StepId,
    pub completed_at: Option<DateTimeUtc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewObject {
    pub step_id: StepId,
    pub key: String,
    pub size: u64,
    pub lines: u64,
    pub frames: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compacted {
    Done,
    Already,
    Busy,
    Changed,
}

pub async fn ingest(
    db: &DatabaseConnection,
    chunk: &Chunk<'_>,
    limit: u64,
) -> Result<Ingested, Error> {
    let seq = signed(CHUNKS, chunk.seq)?;
    let bytes = signed(CHUNKS, chunk.lines.len() as u64)?;
    let limit = signed(EXECUTIONS, limit)?;
    let execution = Uuid::from(chunk.execution);
    let mut transaction = db
        .get_postgres_connection_pool()
        .begin()
        .await
        .map_err(failed)?;
    let Some(found) = sqlx::query(
        "SELECT e.agent_id, e.log_bytes, e.log_compacted_at IS NOT NULL AS compacted, \
         s.id AS step_id FROM execution e \
         LEFT JOIN step s ON s.execution_id = e.id AND s.index = $2 \
         WHERE e.id = $1 FOR NO KEY UPDATE OF e",
    )
    .bind(execution)
    .bind(i64::from(chunk.step))
    .fetch_optional(&mut *transaction)
    .await
    .map_err(failed)?
    else {
        return Ok(Ingested::NoExecution);
    };
    if found.try_get::<Uuid, _>("agent_id").map_err(failed)? != Uuid::from(chunk.agent) {
        return Ok(Ingested::OtherAgent);
    }
    if found.try_get::<bool, _>("compacted").map_err(failed)? {
        return Ok(Ingested::Closed);
    }
    let Some(step) = found
        .try_get::<Option<Uuid>, _>("step_id")
        .map_err(failed)?
    else {
        return Ok(Ingested::NoStep);
    };
    let received: i64 = found.try_get("log_bytes").map_err(failed)?;
    let fits = received.saturating_add(bytes) <= limit;
    let (data, dropped) = if fits {
        (chunk.lines, 0)
    } else {
        (&[][..], bytes)
    };
    let inserted = sqlx::query(
        "INSERT INTO log_chunk (execution_id, seq, step_id, data, dropped) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
    )
    .bind(execution)
    .bind(seq)
    .bind(step)
    .bind(data)
    .bind(dropped)
    .execute(&mut *transaction)
    .await
    .map_err(failed)?
    .rows_affected()
        == 1;
    if !inserted {
        return Ok(Ingested::Duplicate);
    }
    sqlx::query("UPDATE execution SET log_bytes = log_bytes + $2 WHERE id = $1")
        .bind(execution)
        .bind(bytes)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    if fits {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(format!("{} {}", chunk.execution, chunk.seq))
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
    }
    transaction.commit().await.map_err(failed)?;
    Ok(if fits {
        Ingested::Stored
    } else {
        Ingested::Dropped {
            first: received <= limit,
        }
    })
}

pub(crate) fn notice(payload: &str) -> Option<Notice> {
    let (execution, seq) = payload.split_once(' ')?;
    Some(Notice {
        execution_id: execution.parse().ok()?,
        seq: seq.parse().ok()?,
    })
}

pub async fn window(
    db: &DatabaseConnection,
    execution: ExecutionId,
    step: StepId,
    after: u64,
    page: u64,
) -> Result<Option<Window>, Error> {
    let rows = sqlx::query(
        "SELECT e.log_compacted_at IS NOT NULL AS compacted, s.status = $5 AS completed, \
         s.completed_at, o.key, o.size, o.lines, o.frames, c.seq, c.data, c.dropped \
         FROM execution e JOIN step s ON s.execution_id = e.id \
         LEFT JOIN log_object o ON o.step_id = s.id \
         LEFT JOIN LATERAL (SELECT seq, data, dropped FROM log_chunk \
         WHERE execution_id = e.id AND step_id = s.id AND seq > $3 ORDER BY seq LIMIT $4) c \
         ON e.log_compacted_at IS NULL \
         WHERE e.id = $1 AND s.id = $2 ORDER BY c.seq",
    )
    .bind(Uuid::from(execution))
    .bind(Uuid::from(step))
    .bind(signed(CHUNKS, after)?)
    .bind(signed(CHUNKS, page)?)
    .bind(row::text("step", StepStatus::Completed)?)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(failed)?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let completed_at = if first.try_get::<bool, _>("completed").map_err(failed)? {
        Some(
            first
                .try_get::<Option<DateTimeUtc>, _>("completed_at")
                .map_err(failed)?
                .unwrap_or(DateTimeUtc::UNIX_EPOCH),
        )
    } else {
        None
    };
    let log = if first.try_get::<bool, _>("compacted").map_err(failed)? {
        Log::Compacted(object(first)?)
    } else {
        let mut chunks = Vec::with_capacity(rows.len());
        for found in &rows {
            let Some(seq) = found.try_get::<Option<i64>, _>("seq").map_err(failed)? else {
                continue;
            };
            chunks.push(StoredChunk {
                seq: row::unsigned(CHUNKS, seq)?,
                step_id: step,
                data: found.try_get("data").map_err(failed)?,
                dropped: row::unsigned(CHUNKS, found.try_get("dropped").map_err(failed)?)?,
            });
        }
        Log::Live(chunks)
    };
    Ok(Some(Window { completed_at, log }))
}

fn object(found: &PgRow) -> Result<Option<StoredObject>, Error> {
    let Some(key) = found.try_get::<Option<String>, _>("key").map_err(failed)? else {
        return Ok(None);
    };
    Ok(Some(StoredObject {
        key,
        size: row::unsigned(OBJECTS, found.try_get("size").map_err(failed)?)?,
        lines: row::unsigned(OBJECTS, found.try_get("lines").map_err(failed)?)?,
        frames: found
            .try_get::<Vec<i64>, _>("frames")
            .map_err(failed)?
            .into_iter()
            .map(|offset| row::unsigned(OBJECTS, offset))
            .collect::<Result<Vec<u64>, Error>>()?,
    }))
}

pub async fn compactable<C: TransactionTrait>(
    db: &C,
    settled: Duration,
    limit: u64,
) -> Result<Option<Vec<ExecutionId>>, Error> {
    let transaction = db.begin().await?;
    if !periodic::try_lock(&transaction, Task::LogCompaction).await? {
        return Ok(None);
    }
    let found = transaction
        .query_all(statement(
            "SELECT id FROM execution WHERE status = 'completed' AND log_compacted_at IS NULL \
             AND completed_at <= now() - make_interval(secs => $1) \
             ORDER BY completed_at, id LIMIT $2",
            [
                settled.as_secs_f64().into(),
                signed(EXECUTIONS, limit)?.into(),
            ],
        ))
        .await?;
    transaction.commit().await?;
    found
        .into_iter()
        .map(|found| row::id(EXECUTIONS, found.try_get::<Uuid>("", "id")?))
        .collect::<Result<Vec<ExecutionId>, Error>>()
        .map(Some)
}

pub async fn compaction(
    db: &DatabaseConnection,
    execution: ExecutionId,
) -> Result<Compaction, Error> {
    let id = Uuid::from(execution);
    let mut transaction = db
        .get_postgres_connection_pool()
        .begin()
        .await
        .map_err(failed)?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(lock_key(id))
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
    if !locked {
        return Ok(Compaction::Skipped(Compacted::Busy));
    }
    let Some(found) = sqlx::query(
        "SELECT a.run_id, j.attempt_id FROM execution e JOIN job j ON j.id = e.job_id \
         JOIN attempt a ON a.id = j.attempt_id \
         WHERE e.id = $1 AND e.status = 'completed' AND e.log_compacted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(failed)?
    else {
        return Ok(Compaction::Skipped(Compacted::Already));
    };
    let steps =
        sqlx::query("SELECT id, completed_at FROM step WHERE execution_id = $1 ORDER BY index")
            .bind(id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(failed)?
            .iter()
            .map(|found| {
                Ok(StepLog {
                    id: row::id("step", found.try_get::<Uuid, _>("id").map_err(failed)?)?,
                    completed_at: found.try_get("completed_at").map_err(failed)?,
                })
            })
            .collect::<Result<Vec<StepLog>, Error>>()?;
    let declare = format!(
        "DECLARE {CURSOR} NO SCROLL CURSOR FOR SELECT seq, step_id, data, dropped \
         FROM log_chunk WHERE execution_id = $1 ORDER BY seq"
    );
    sqlx::query(&declare)
        .persistent(false)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
    Ok(Compaction::Started(Compactor {
        transaction,
        execution: id,
        read: 0,
        run_id: row::id("run", found.try_get::<Uuid, _>("run_id").map_err(failed)?)?,
        attempt_id: row::id(
            "attempt",
            found.try_get::<Uuid, _>("attempt_id").map_err(failed)?,
        )?,
        steps,
    }))
}

fn lock_key(execution: Uuid) -> i64 {
    execution.as_u64_pair().1.cast_signed()
}

impl Compactor {
    pub async fn chunks(&mut self, page: u64) -> Result<Vec<StoredChunk>, Error> {
        let fetch = format!("FETCH FORWARD {page} FROM {CURSOR}");
        let found = sqlx::query(&fetch)
            .persistent(false)
            .fetch_all(&mut *self.transaction)
            .await
            .map_err(failed)?;
        self.read += found.len() as u64;
        found
            .iter()
            .map(|found| {
                Ok(StoredChunk {
                    seq: row::unsigned(CHUNKS, found.try_get("seq").map_err(failed)?)?,
                    step_id: row::id(CHUNKS, found.try_get::<Uuid, _>("step_id").map_err(failed)?)?,
                    data: found.try_get("data").map_err(failed)?,
                    dropped: row::unsigned(CHUNKS, found.try_get("dropped").map_err(failed)?)?,
                })
            })
            .collect()
    }

    pub async fn finish(mut self, objects: &[NewObject]) -> Result<Compacted, Error> {
        let id = self.execution;
        let Some(found) = sqlx::query(
            "SELECT log_compacted_at IS NOT NULL AS compacted FROM execution WHERE id = $1 \
             FOR NO KEY UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(failed)?
        else {
            return Ok(Compacted::Already);
        };
        if found.try_get::<bool, _>("compacted").map_err(failed)? {
            return Ok(Compacted::Already);
        }
        let stored: i64 =
            sqlx::query_scalar("SELECT count(*) FROM log_chunk WHERE execution_id = $1")
                .bind(id)
                .fetch_one(&mut *self.transaction)
                .await
                .map_err(failed)?;
        if stored != signed(CHUNKS, self.read)? {
            return Ok(Compacted::Changed);
        }
        for object in objects {
            let frames = object
                .frames
                .iter()
                .map(|offset| signed(OBJECTS, *offset))
                .collect::<Result<Vec<i64>, Error>>()?;
            sqlx::query(
                "INSERT INTO log_object (step_id, execution_id, key, size, lines, frames) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(Uuid::from(object.step_id))
            .bind(id)
            .bind(&object.key)
            .bind(signed(OBJECTS, object.size)?)
            .bind(signed(OBJECTS, object.lines)?)
            .bind(frames)
            .execute(&mut *self.transaction)
            .await
            .map_err(failed)?;
        }
        sqlx::query("DELETE FROM log_chunk WHERE execution_id = $1")
            .bind(id)
            .execute(&mut *self.transaction)
            .await
            .map_err(failed)?;
        sqlx::query("UPDATE execution SET log_compacted_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *self.transaction)
            .await
            .map_err(failed)?;
        self.transaction.commit().await.map_err(failed)?;
        Ok(Compacted::Done)
    }
}

fn signed(table: &'static str, value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}

fn failed(error: sqlx::Error) -> Error {
    Error::Database(sqlx_error(error))
}
