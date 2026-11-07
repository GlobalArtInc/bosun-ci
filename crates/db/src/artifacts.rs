use bosun_types::id::{AttemptId, ExecutionId, ProjectId, RunId};
use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, QueryResult, TransactionTrait};

use crate::Error;
use crate::row::{self, statement};

const TABLE: &str = "artifact";

const SELECT: &str = "SELECT a.id, a.project_id, t.run_id, a.attempt_id, a.execution_id, a.name, \
     a.key, a.content_type, a.created_at, a.expires_at, a.finalized_at, a.size, a.sha256 \
     FROM artifact a JOIN attempt t ON t.id = a.attempt_id";

pub struct New<'a> {
    pub attempt: AttemptId,
    pub execution: ExecutionId,
    pub name: &'a str,
    pub content_type: &'a str,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub size: u64,
    pub sha256: Option<[u8; 32]>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: i64,
    pub project_id: ProjectId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub execution_id: ExecutionId,
    pub name: String,
    pub key: String,
    pub content_type: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub finalized: Option<Done>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finalizing {
    Done(Box<Stored>),
    Differs,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    Accepted,
    Full,
    Missing,
}

pub async fn create<C: ConnectionTrait>(db: &C, new: &New<'_>) -> Result<Option<Stored>, Error> {
    let project = db
        .query_one(statement(
            "SELECT r.project_id FROM execution e JOIN job j ON j.id = e.job_id \
             JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id \
             WHERE e.id = $1 AND a.id = $2",
            [
                Uuid::from(new.execution).into(),
                Uuid::from(new.attempt).into(),
            ],
        ))
        .await?
        .ok_or(Error::NoExecution(new.execution))?
        .try_get::<Uuid>("", "project_id")?;
    let inserted = db
        .query_one(statement(
            "WITH next AS (SELECT nextval('artifact_id_seq') AS id) \
             INSERT INTO artifact (id, project_id, attempt_id, execution_id, name, key, \
             content_type, created_at, expires_at) \
             SELECT next.id, $1::uuid, $2, $3, $4, 'artifacts/' || ($1::uuid)::text || '/' || next.id, \
             $5, $6, $7 FROM next \
             ON CONFLICT (attempt_id, name) DO NOTHING RETURNING id",
            [
                project.into(),
                Uuid::from(new.attempt).into(),
                Uuid::from(new.execution).into(),
                new.name.into(),
                new.content_type.into(),
                new.created_at.into(),
                new.expires_at.into(),
            ],
        ))
        .await?;
    let Some(inserted) = inserted else {
        return Ok(None);
    };
    let id = inserted.try_get::<i64>("", "id")?;
    find(db, id)
        .await?
        .map(Some)
        .ok_or_else(|| Error::Vanished(format!("artifact {id}")))
}

pub async fn find<C: ConnectionTrait>(db: &C, id: i64) -> Result<Option<Stored>, Error> {
    db.query_one(statement(&format!("{SELECT} WHERE a.id = $1"), [id.into()]))
        .await?
        .map(|found| decode(&found))
        .transpose()
}

pub async fn of_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: AttemptId,
    name: Option<&str>,
    id: Option<i64>,
) -> Result<Vec<Stored>, Error> {
    db.query_all(statement(
        &format!(
            "{SELECT} WHERE a.attempt_id = $1 AND ($2::text IS NULL OR a.name = $2) \
             AND ($3::bigint IS NULL OR a.id = $3) ORDER BY a.id"
        ),
        [
            Uuid::from(attempt).into(),
            name.map(str::to_owned).into(),
            id.into(),
        ],
    ))
    .await?
    .iter()
    .map(decode)
    .collect()
}

pub async fn of_run<C: ConnectionTrait>(
    db: &C,
    run: RunId,
    after: i64,
    limit: u64,
) -> Result<Vec<Stored>, Error> {
    db.query_all(statement(
        &format!(
            "{SELECT} WHERE t.run_id = $1 AND a.finalized_at IS NOT NULL AND a.id > $2 \
             ORDER BY a.id LIMIT $3"
        ),
        [
            Uuid::from(run).into(),
            after.into(),
            row::signed(TABLE, limit)?.into(),
        ],
    ))
    .await?
    .iter()
    .map(decode)
    .collect()
}

pub async fn finalize<C: TransactionTrait>(
    db: &C,
    id: i64,
    done: &Done,
) -> Result<Finalizing, Error> {
    let transaction = db.begin().await?;
    let updated = transaction
        .execute(statement(
            "UPDATE artifact SET finalized_at = $2, size = $3, sha256 = $4, blocks = 0 \
             WHERE id = $1 AND finalized_at IS NULL",
            [
                id.into(),
                done.at.into(),
                row::signed(TABLE, done.size)?.into(),
                done.sha256.map(|digest| digest.to_vec()).into(),
            ],
        ))
        .await?
        .rows_affected();
    if updated == 1 {
        transaction
            .execute(statement(
                "DELETE FROM artifact_block WHERE artifact_id = $1",
                [id.into()],
            ))
            .await?;
    }
    let found = find(&transaction, id).await?;
    transaction.commit().await?;
    Ok(match found {
        None => Finalizing::Missing,
        Some(stored) if updated == 1 => Finalizing::Done(Box::new(stored)),
        Some(stored) => match &stored.finalized {
            Some(earlier) if earlier.size == done.size && earlier.sha256 == done.sha256 => {
                Finalizing::Done(Box::new(stored))
            }
            _ => Finalizing::Differs,
        },
    })
}

pub async fn delete_in_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: AttemptId,
    id: i64,
) -> Result<bool, Error> {
    Ok(db
        .execute(statement(
            "DELETE FROM artifact WHERE id = $1 AND attempt_id = $2",
            [id.into(), Uuid::from(attempt).into()],
        ))
        .await?
        .rows_affected()
        == 1)
}

pub async fn delete<C: ConnectionTrait>(db: &C, id: i64) -> Result<bool, Error> {
    Ok(db
        .execute(statement("DELETE FROM artifact WHERE id = $1", [id.into()]))
        .await?
        .rows_affected()
        == 1)
}

pub async fn register_block<C: TransactionTrait>(
    db: &C,
    id: i64,
    block: &str,
    limit: usize,
) -> Result<Registration, Error> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let transaction = db.begin().await?;
    let Some(locked) = transaction
        .query_one(statement(
            "SELECT blocks FROM artifact WHERE id = $1 AND finalized_at IS NULL \
             FOR NO KEY UPDATE",
            [id.into()],
        ))
        .await?
    else {
        transaction.rollback().await?;
        return Ok(Registration::Missing);
    };
    let held = i64::from(locked.try_get::<i32>("", "blocks")?);
    let outcome = if held >= limit {
        let present = transaction
            .query_one(statement(
                "SELECT 1 AS present FROM artifact_block WHERE artifact_id = $1 AND block = $2",
                [id.into(), block.into()],
            ))
            .await?
            .is_some();
        if present {
            Registration::Accepted
        } else {
            Registration::Full
        }
    } else {
        let added = transaction
            .execute(statement(
                "INSERT INTO artifact_block (artifact_id, block) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
                [id.into(), block.into()],
            ))
            .await?
            .rows_affected();
        if added == 1 {
            transaction
                .execute(statement(
                    "UPDATE artifact SET blocks = blocks + 1 WHERE id = $1",
                    [id.into()],
                ))
                .await?;
        }
        Registration::Accepted
    };
    transaction.commit().await?;
    Ok(outcome)
}

fn decode(found: &QueryResult) -> Result<Stored, Error> {
    let finalized_at = found.try_get::<Option<DateTime<Utc>>>("", "finalized_at")?;
    let size = found.try_get::<Option<i64>>("", "size")?;
    let sha256 = found.try_get::<Option<Vec<u8>>>("", "sha256")?;
    let finalized = match (finalized_at, size) {
        (Some(at), Some(size)) => Some(Done {
            size: row::unsigned(TABLE, size)?,
            sha256: sha256
                .map(|bytes| {
                    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| Error::Row {
                        table: TABLE,
                        message: format!("a digest of {} bytes is not a SHA-256", bytes.len()),
                    })
                })
                .transpose()?,
            at,
        }),
        _ => None,
    };
    Ok(Stored {
        id: found.try_get("", "id")?,
        project_id: row::id(TABLE, found.try_get::<Uuid>("", "project_id")?)?,
        run_id: row::id(TABLE, found.try_get::<Uuid>("", "run_id")?)?,
        attempt_id: row::id(TABLE, found.try_get::<Uuid>("", "attempt_id")?)?,
        execution_id: row::id(TABLE, found.try_get::<Uuid>("", "execution_id")?)?,
        name: found.try_get("", "name")?,
        key: found.try_get("", "key")?,
        content_type: found.try_get("", "content_type")?,
        created_at: found.try_get("", "created_at")?,
        expires_at: found.try_get("", "expires_at")?,
        finalized,
    })
}
