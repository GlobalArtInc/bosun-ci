use std::mem;

use bosun_types::api::Reporting;
use bosun_types::event::Event;
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::status::{ExecutionStatus, JobStatus, RunConclusion};
use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::row::statement;
use crate::{Error, row};

pub(crate) const CHANNEL: &str = "bosun_outbox";

const TABLE: &str = "outbox";

const CHECK_RUNS: &str = "forge_check_run";

const EXTENSIONS: [&str; 2] = [".yml", ".yaml"];

const PULL_REQUESTS: [&str; 4] = [
    "pull_request",
    "pull_request_target",
    "pull_request_review",
    "pull_request_review_comment",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CheckRun,
    CommitStatus,
    ForkStatus,
    TokenRevocation,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::CheckRun,
        Kind::CommitStatus,
        Kind::ForkStatus,
        Kind::TokenRevocation,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub repository_id: u64,
    pub sha: String,
    pub run_id: RunId,
    pub job_id: Option<JobId>,
    pub workflow: String,
    pub name: String,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkStatus {
    pub repository_id: u64,
    pub sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRevocation {
    pub execution_id: ExecutionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "conclusion", rename_all = "snake_case")]
pub enum State {
    Queued,
    InProgress,
    Completed(RunConclusion),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportedCheckRun {
    pub id: u64,
    pub annotations: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Claimed {
    pub id: u64,
    pub kind: Kind,
    pub collapse_key: String,
    pub payload: Value,
    pub attempts: u32,
    pub failing_since: Option<DateTime<Utc>>,
    pub lease: DateTime<Utc>,
    pub dropped: u64,
}

pub(crate) async fn record<C: ConnectionTrait>(
    db: &C,
    run: RunId,
    job: Option<JobId>,
    execution: Option<ExecutionId>,
    event: &Event,
) -> Result<(), Error> {
    if let (Event::ExecutionStatusChanged(changed), Some(execution)) = (event, execution)
        && changed.to == ExecutionStatus::Completed
    {
        return revoke_tokens(db, execution).await;
    }
    match (event, job) {
        (Event::JobStatusChanged(changed), Some(job)) => {
            let Some(state) = reported(changed.to, changed.conclusion.map(RunConclusion::from))
            else {
                return Ok(());
            };
            if reported(changed.from, None)
                .is_some_and(|before| mem::discriminant(&before) == mem::discriminant(&state))
            {
                return Ok(());
            }
            job_report(db, job, state).await
        }
        (Event::RunStatusChanged(changed), _)
            if changed.conclusion == Some(RunConclusion::StartupFailure) =>
        {
            run_report(db, run).await
        }
        _ => Ok(()),
    }
}

fn reported(status: JobStatus, conclusion: Option<RunConclusion>) -> Option<State> {
    match status {
        JobStatus::Pending => None,
        JobStatus::Waiting | JobStatus::Queued | JobStatus::Assigned => Some(State::Queued),
        JobStatus::InProgress | JobStatus::Paused | JobStatus::Held => Some(State::InProgress),
        JobStatus::Completed => conclusion.map(State::Completed),
    }
}

const TARGET: &str = "r.id AS run_id, r.sha, r.event, \
     r.trigger #>> '{payload,pull_request,head,sha}' AS head_sha, p.repository_id, p.report, \
     r.ir ->> 'name' AS workflow_name, w.name AS workflow FROM run r \
     JOIN project p ON p.id = r.project_id JOIN workflow w ON w.id = r.workflow_id \
     JOIN forge_repository f ON f.id = p.repository_id";

const BOUND: &str = "r.sha IS NOT NULL AND f.installation_id IS NOT NULL";

async fn job_report<C: ConnectionTrait>(db: &C, job: JobId, state: State) -> Result<(), Error> {
    let Some(found) = db
        .query_one(statement(
            &format!(
                "SELECT j.name AS job_name, \
                 EXISTS (SELECT 1 FROM job m WHERE m.attempt_id = j.attempt_id \
                 AND m.group_id = j.id) AS expanded, {TARGET} \
                 JOIN attempt a ON a.run_id = r.id JOIN job j ON j.attempt_id = a.id \
                 WHERE j.id = $1 AND {BOUND}"
            ),
            [Uuid::from(job).into()],
        ))
        .await?
    else {
        return Ok(());
    };
    if found.try_get::<bool>("", "expanded")? {
        return Ok(());
    }
    let name: String = found.try_get("", "job_name")?;
    store(db, &found, Some(job), name, state).await
}

async fn run_report<C: ConnectionTrait>(db: &C, run: RunId) -> Result<(), Error> {
    let Some(found) = db
        .query_one(statement(
            &format!("SELECT {TARGET} WHERE r.id = $1 AND {BOUND}"),
            [Uuid::from(run).into()],
        ))
        .await?
    else {
        return Ok(());
    };
    let name = workflow(&found)?;
    store(
        db,
        &found,
        None,
        name,
        State::Completed(RunConclusion::StartupFailure),
    )
    .await
}

fn workflow(found: &QueryResult) -> Result<String, Error> {
    if let Some(name) = found.try_get::<Option<String>>("", "workflow_name")?
        && !name.trim().is_empty()
    {
        return Ok(name);
    }
    let path: String = found.try_get("", "workflow")?;
    let file = path.rsplit('/').next().unwrap_or(&path);
    Ok(EXTENSIONS
        .iter()
        .find_map(|extension| file.strip_suffix(extension))
        .unwrap_or(file)
        .to_owned())
}

async fn store<C: ConnectionTrait>(
    db: &C,
    found: &QueryResult,
    job: Option<JobId>,
    name: String,
    state: State,
) -> Result<(), Error> {
    let run: RunId = row::id(TABLE, found.try_get::<Uuid>("", "run_id")?)?;
    let kind = match row::name(TABLE, found.try_get::<String>("", "report")?)? {
        Reporting::Checks => Kind::CheckRun,
        Reporting::Statuses => Kind::CommitStatus,
    };
    let event: String = found.try_get("", "event")?;
    let sha = match found.try_get::<Option<String>>("", "head_sha")? {
        Some(head) if PULL_REQUESTS.contains(&event.as_str()) => head,
        _ => found.try_get("", "sha")?,
    };
    let report = Report {
        repository_id: row::unsigned(TABLE, found.try_get("", "repository_id")?)?,
        sha,
        run_id: run,
        job_id: job,
        workflow: workflow(found)?,
        name,
        state,
    };
    let subject = match job {
        Some(job) => format!("job/{job}"),
        None => format!("run/{run}"),
    };
    insert(db, kind, &subject, serde_json::to_value(&report)?).await
}

pub async fn refuse_fork<C: ConnectionTrait>(db: &C, status: &ForkStatus) -> Result<(), Error> {
    insert(
        db,
        Kind::ForkStatus,
        &format!("{}/{}", status.repository_id, status.sha),
        serde_json::to_value(status)?,
    )
    .await
}

pub async fn revoke_tokens<C: ConnectionTrait>(
    db: &C,
    execution: ExecutionId,
) -> Result<(), Error> {
    let kind = row::text(TABLE, Kind::TokenRevocation)?;
    db.execute(statement(
        "WITH stored AS (INSERT INTO outbox (kind, collapse_key, payload) SELECT $1, $2, $3 \
         WHERE EXISTS (SELECT 1 FROM forge_token WHERE execution_id = $5) RETURNING id) \
         SELECT pg_notify($4, id::text) FROM stored",
        [
            kind.as_str().into(),
            format!("{kind}/execution/{execution}").into(),
            serde_json::to_value(TokenRevocation {
                execution_id: execution,
            })?
            .into(),
            CHANNEL.into(),
            Uuid::from(execution).into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn insert<C: ConnectionTrait>(
    db: &C,
    kind: Kind,
    subject: &str,
    payload: Value,
) -> Result<(), Error> {
    let kind = row::text(TABLE, kind)?;
    db.execute(statement(
        "WITH stored AS (INSERT INTO outbox (kind, collapse_key, payload) VALUES ($1, $2, $3) \
         RETURNING id) SELECT pg_notify($4, id::text) FROM stored",
        [
            kind.as_str().into(),
            format!("{kind}/{subject}").into(),
            payload.into(),
            CHANNEL.into(),
        ],
    ))
    .await?;
    Ok(())
}

pub async fn claim(
    transaction: &DatabaseTransaction,
    now: DateTime<Utc>,
    lease: DateTime<Utc>,
) -> Result<Option<Claimed>, Error> {
    let kinds = Kind::ALL
        .iter()
        .map(|kind| row::text(TABLE, kind))
        .collect::<Result<Vec<String>, Error>>()?;
    let Some(oldest) = transaction
        .query_one(statement(
            "SELECT id, collapse_key, attempts, failing_since, last_error FROM outbox o \
             WHERE dead_at IS NULL AND next_attempt_at <= $1 AND kind = ANY ($2) \
             AND NOT EXISTS (SELECT 1 FROM outbox e WHERE e.collapse_key = o.collapse_key \
             AND e.id < o.id AND e.dead_at IS NULL) \
             ORDER BY next_attempt_at, id LIMIT 1 FOR UPDATE SKIP LOCKED",
            [now.into(), kinds.into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let oldest_id: i64 = oldest.try_get("", "id")?;
    let collapse_key: String = oldest.try_get("", "collapse_key")?;
    let attempts: i32 = oldest.try_get("", "attempts")?;
    let failing_since: Option<DateTime<Utc>> = oldest.try_get("", "failing_since")?;
    let last_error: Option<String> = oldest.try_get("", "last_error")?;
    let newest = transaction
        .query_one(statement(
            "SELECT id, kind, payload FROM outbox WHERE collapse_key = $1 AND dead_at IS NULL \
             ORDER BY id DESC LIMIT 1 FOR UPDATE",
            [collapse_key.as_str().into()],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the outbox row {oldest_id}")))?;
    let id: i64 = newest.try_get("", "id")?;
    let dropped = transaction
        .execute(statement(
            "DELETE FROM outbox WHERE collapse_key = $1 AND dead_at IS NULL AND id < $2",
            [collapse_key.as_str().into(), id.into()],
        ))
        .await?
        .rows_affected();
    transaction
        .execute(statement(
            "UPDATE outbox SET attempts = $2, failing_since = $3, last_error = $4, \
             next_attempt_at = $5 WHERE id = $1",
            [
                id.into(),
                attempts.saturating_add(1).into(),
                failing_since.into(),
                last_error.into(),
                lease.into(),
            ],
        ))
        .await?;
    Ok(Some(Claimed {
        id: row::unsigned(TABLE, id)?,
        kind: row::name(TABLE, newest.try_get("", "kind")?)?,
        collapse_key,
        payload: newest.try_get("", "payload")?,
        attempts: u32::try_from(attempts).map_err(|error| Error::Row {
            table: TABLE,
            message: error.to_string(),
        })?,
        failing_since,
        lease,
        dropped,
    }))
}

pub async fn delivered<C: ConnectionTrait>(db: &C, claimed: &Claimed) -> Result<bool, Error> {
    let id = row::signed(TABLE, claimed.id)?;
    let deleted = db
        .execute(statement(
            "DELETE FROM outbox WHERE id = $1 AND next_attempt_at = $2 AND dead_at IS NULL",
            [id.into(), claimed.lease.into()],
        ))
        .await?
        .rows_affected()
        > 0;
    if deleted {
        db.execute(statement(
            "DELETE FROM outbox WHERE collapse_key = $1 AND id < $2",
            [claimed.collapse_key.as_str().into(), id.into()],
        ))
        .await?;
    }
    Ok(deleted)
}

pub async fn postpone<C: ConnectionTrait>(
    db: &C,
    claimed: &Claimed,
    error: &str,
    failing_since: DateTime<Utc>,
    next_attempt_at: DateTime<Utc>,
) -> Result<bool, Error> {
    settle(
        db,
        claimed,
        "next_attempt_at = $3",
        error,
        failing_since,
        next_attempt_at,
    )
    .await
}

pub async fn kill<C: ConnectionTrait>(
    db: &C,
    claimed: &Claimed,
    error: &str,
    failing_since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    settle(db, claimed, "dead_at = $3", error, failing_since, now).await
}

async fn settle<C: ConnectionTrait>(
    db: &C,
    claimed: &Claimed,
    change: &str,
    error: &str,
    failing_since: DateTime<Utc>,
    at: DateTime<Utc>,
) -> Result<bool, Error> {
    Ok(db
        .execute(statement(
            &format!(
                "UPDATE outbox SET {change}, last_error = $4, failing_since = $5 \
                 WHERE id = $1 AND next_attempt_at = $2 AND dead_at IS NULL"
            ),
            [
                row::signed(TABLE, claimed.id)?.into(),
                claimed.lease.into(),
                at.into(),
                error.into(),
                failing_since.into(),
            ],
        ))
        .await?
        .rows_affected()
        > 0)
}

pub async fn pending<C: ConnectionTrait>(db: &C) -> Result<u64, Error> {
    let found = db
        .query_one(statement(
            "SELECT count(*) AS pending FROM outbox WHERE dead_at IS NULL",
            [],
        ))
        .await?
        .ok_or(Error::Missing(TABLE))?;
    row::unsigned(TABLE, found.try_get("", "pending")?)
}

pub async fn check_run<C: ConnectionTrait>(
    db: &C,
    job: JobId,
) -> Result<Option<ReportedCheckRun>, Error> {
    db.query_one(statement(
        "SELECT id, annotations FROM forge_check_run WHERE job_id = $1",
        [Uuid::from(job).into()],
    ))
    .await?
    .map(|found| {
        Ok(ReportedCheckRun {
            id: row::unsigned(CHECK_RUNS, found.try_get("", "id")?)?,
            annotations: row::unsigned(CHECK_RUNS, found.try_get("", "annotations")?)?,
        })
    })
    .transpose()
}

pub async fn record_check_run<C: ConnectionTrait>(
    db: &C,
    job: JobId,
    reported: &ReportedCheckRun,
) -> Result<(), Error> {
    db.execute(statement(
        "INSERT INTO forge_check_run (job_id, id, annotations) SELECT $1, $2, $3 \
         WHERE EXISTS (SELECT 1 FROM job WHERE id = $1) ON CONFLICT (job_id) DO UPDATE \
         SET annotations = EXCLUDED.annotations WHERE forge_check_run.id = EXCLUDED.id",
        [
            Uuid::from(job).into(),
            row::signed(CHECK_RUNS, reported.id)?.into(),
            row::signed(CHECK_RUNS, reported.annotations)?.into(),
        ],
    ))
    .await?;
    Ok(())
}
