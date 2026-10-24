use std::collections::HashMap;
use std::time::Duration;

use bosun_types::actor::Actor;
use bosun_types::api::{Agent, Capability};
use bosun_types::event::{Event, ExecutionStatusChanged, JobStatusChanged};
use bosun_types::id::{AgentId, ExecutionId, PoolId, RegistrationTokenId};
use bosun_types::status::{Conclusion, ConclusionSource, ExecutionStatus, JobStatus, StatusReason};
use sea_orm::prelude::{DateTimeUtc, Uuid};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, QueryResult, QuerySelect, Statement, TransactionTrait, Value,
};
use serde::Serialize;

use crate::entity::{agent, execution, registration_token};
use crate::journal::{self, NewEvent};
use crate::{Error, row};

const TABLE: &str = "agent";

const SWEEP_LOCK: i64 = i64::from_be_bytes(*b"bosunagt");

const RUNNING: [ExecutionStatus; 3] = [
    ExecutionStatus::InProgress,
    ExecutionStatus::Paused,
    ExecutionStatus::Held,
];

pub struct NewRegistrationToken {
    pub pool_id: PoolId,
    pub sha256: Vec<u8>,
    pub reusable: bool,
    pub expires_at: DateTimeUtc,
}

pub struct RegistrationToken {
    pub id: RegistrationTokenId,
    pub pool_id: PoolId,
    pub reusable: bool,
    pub expires_at: DateTimeUtc,
}

pub struct NewAgent {
    pub name: String,
    pub labels: Vec<String>,
    pub credential_sha256: Vec<u8>,
}

pub enum Registration {
    Registered(Agent),
    UnknownToken,
    ExpiredToken,
    UsedToken,
}

pub struct Connection {
    pub replica_id: Uuid,
    pub version: String,
    pub labels: Vec<String>,
    pub capabilities: Vec<Capability>,
}

pub struct Sweep {
    pub offline: Vec<(AgentId, String)>,
    pub lost: Vec<ExecutionId>,
}

pub async fn create_registration_token<C: ConnectionTrait>(
    db: &C,
    token: NewRegistrationToken,
) -> Result<RegistrationToken, Error> {
    let stored = registration_token::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        pool_id: ActiveValue::Set(token.pool_id.into()),
        sha256: ActiveValue::Set(token.sha256),
        reusable: ActiveValue::Set(token.reusable),
        expires_at: ActiveValue::Set(token.expires_at),
        used_at: ActiveValue::Set(None),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    Ok(RegistrationToken {
        id: row::id("registration_token", stored.id)?,
        pool_id: row::id("registration_token", stored.pool_id)?,
        reusable: stored.reusable,
        expires_at: stored.expires_at,
    })
}

pub async fn register<C: TransactionTrait>(
    db: &C,
    token_sha256: &[u8],
    now: DateTimeUtc,
    agent: NewAgent,
) -> Result<Registration, Error> {
    let transaction = db.begin().await?;
    let Some(token) = registration_token::Entity::find()
        .filter(registration_token::Column::Sha256.eq(token_sha256.to_vec()))
        .lock_exclusive()
        .one(&transaction)
        .await?
    else {
        return Ok(Registration::UnknownToken);
    };
    if token.expires_at <= now {
        return Ok(Registration::ExpiredToken);
    }
    if !token.reusable && token.used_at.is_some() {
        return Ok(Registration::UsedToken);
    }
    let pool_id = token.pool_id;
    let mut used: registration_token::ActiveModel = token.into();
    used.used_at = ActiveValue::Set(Some(now));
    used.update(&transaction).await?;
    let stored = agent::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        pool_id: ActiveValue::Set(pool_id),
        name: ActiveValue::Set(agent.name),
        credential_sha256: ActiveValue::Set(agent.credential_sha256),
        labels: ActiveValue::Set(agent.labels),
        capabilities: ActiveValue::Set(Vec::new()),
        version: ActiveValue::Set(None),
        online: ActiveValue::Set(false),
        last_seen_at: ActiveValue::Set(None),
        created_at: ActiveValue::NotSet,
        replica_id: ActiveValue::Set(None),
    }
    .insert(&transaction)
    .await?;
    let registered = decode(stored, None)?;
    transaction.commit().await?;
    Ok(Registration::Registered(registered))
}

pub async fn authenticate<C: ConnectionTrait>(
    db: &C,
    credential_sha256: &[u8],
) -> Result<Option<Agent>, Error> {
    let found = agent::Entity::find()
        .filter(agent::Column::CredentialSha256.eq(credential_sha256.to_vec()))
        .one(db)
        .await?;
    match found {
        Some(found) => {
            let running = running(db, &[found.id]).await?;
            let execution = running.get(&found.id).copied();
            Ok(Some(decode(found, execution)?))
        }
        None => Ok(None),
    }
}

pub async fn connect<C: ConnectionTrait>(
    db: &C,
    agent: AgentId,
    connection: Connection,
) -> Result<(), Error> {
    let capabilities = connection
        .capabilities
        .iter()
        .map(name)
        .collect::<Result<Vec<String>, Error>>()?;
    db.execute(statement(
        "UPDATE agent SET online = true, last_seen_at = now(), replica_id = $2, version = $3, \
         labels = $4, capabilities = $5 WHERE id = $1",
        [
            Uuid::from(agent).into(),
            connection.replica_id.into(),
            connection.version.into(),
            connection.labels.into(),
            capabilities.into(),
        ],
    ))
    .await?;
    Ok(())
}

pub async fn heartbeat<C: ConnectionTrait>(db: &C, agent: AgentId) -> Result<(), Error> {
    db.execute(statement(
        "UPDATE agent SET online = true, last_seen_at = now() WHERE id = $1",
        [Uuid::from(agent).into()],
    ))
    .await?;
    Ok(())
}

pub async fn disconnect<C: ConnectionTrait>(
    db: &C,
    agent: AgentId,
    replica_id: Uuid,
) -> Result<(), Error> {
    db.execute(statement(
        "UPDATE agent SET replica_id = NULL WHERE id = $1 AND replica_id = $2",
        [Uuid::from(agent).into(), replica_id.into()],
    ))
    .await?;
    Ok(())
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    after: Option<AgentId>,
    limit: u64,
) -> Result<Vec<Agent>, Error> {
    let mut query = agent::Entity::find()
        .order_by_asc(agent::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(agent::Column::Id.gt(Uuid::from(after)));
    }
    let agents = query.all(db).await?;
    let ids: Vec<Uuid> = agents.iter().map(|agent| agent.id).collect();
    let running = running(db, &ids).await?;
    agents
        .into_iter()
        .map(|agent| {
            let execution = running.get(&agent.id).copied();
            decode(agent, execution)
        })
        .collect()
}

pub async fn sweep<C: TransactionTrait>(
    db: &C,
    offline_after: Duration,
    grace: Duration,
) -> Result<Option<Sweep>, Error> {
    let transaction = db.begin().await?;
    let locked = transaction
        .query_one(statement(
            "SELECT pg_try_advisory_xact_lock($1) AS locked",
            [SWEEP_LOCK.into()],
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "locked"))
        .transpose()?
        .unwrap_or(false);
    if !locked {
        return Ok(None);
    }
    let offline = transaction
        .query_all(statement(
            "UPDATE agent SET online = false WHERE online \
             AND (last_seen_at IS NULL OR last_seen_at < now() - make_interval(secs => $1)) \
             RETURNING id, name",
            [offline_after.as_secs_f64().into()],
        ))
        .await?
        .into_iter()
        .map(|found| {
            Ok((
                row::id(TABLE, found.try_get::<Uuid>("", "id")?)?,
                found.try_get::<String>("", "name")?,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let abandoned = transaction
        .query_all(statement(
            "SELECT e.id FROM execution e JOIN agent a ON a.id = e.agent_id \
             WHERE e.status = ANY($1) AND NOT a.online \
             AND a.last_seen_at < now() - make_interval(secs => $2) ORDER BY e.id",
            [running_names()?.into(), grace.as_secs_f64().into()],
        ))
        .await?;
    let lost = lose_all(&transaction, abandoned).await?;
    transaction.commit().await?;
    Ok(Some(Sweep { offline, lost }))
}

pub async fn reconcile<C: TransactionTrait>(
    db: &C,
    agent: AgentId,
    running: &[ExecutionId],
) -> Result<Vec<ExecutionId>, Error> {
    let transaction = db.begin().await?;
    let kept: Vec<Uuid> = running.iter().copied().map(Uuid::from).collect();
    let abandoned = transaction
        .query_all(statement(
            "SELECT id FROM execution WHERE agent_id = $1 AND status = ANY($2) \
             AND NOT (id = ANY($3)) ORDER BY id",
            [
                Uuid::from(agent).into(),
                running_names()?.into(),
                kept.into(),
            ],
        ))
        .await?;
    let lost = lose_all(&transaction, abandoned).await?;
    transaction.commit().await?;
    Ok(lost)
}

async fn lose_all<C: ConnectionTrait>(
    db: &C,
    executions: Vec<QueryResult>,
) -> Result<Vec<ExecutionId>, Error> {
    let mut lost = Vec::with_capacity(executions.len());
    for found in executions {
        let id = found.try_get::<Uuid>("", "id")?;
        if lose(db, id).await? {
            lost.push(row::id("execution", id)?);
        }
    }
    Ok(lost)
}

async fn lose<C: ConnectionTrait>(db: &C, execution: Uuid) -> Result<bool, Error> {
    let Some(found) = db
        .query_one(statement(
            "SELECT e.status AS execution_status, e.job_id, j.status AS job_status, \
             j.attempt_id, a.run_id FROM execution e JOIN job j ON j.id = e.job_id \
             JOIN attempt a ON a.id = j.attempt_id WHERE e.id = $1 FOR UPDATE OF e, j",
            [execution.into()],
        ))
        .await?
    else {
        return Ok(false);
    };
    let from: ExecutionStatus = row::name("execution", found.try_get("", "execution_status")?)?;
    if !RUNNING.contains(&from) {
        return Ok(false);
    }
    let job_id: Uuid = found.try_get("", "job_id")?;
    let job_from: JobStatus = row::name("job", found.try_get("", "job_status")?)?;
    let attempt_id: Uuid = found.try_get("", "attempt_id")?;
    let run_id: Uuid = found.try_get("", "run_id")?;
    db.execute(statement(
        "UPDATE execution SET status = $2, conclusion = $3, reason = $4, completed_at = now() \
         WHERE id = $1",
        [
            execution.into(),
            name(ExecutionStatus::Completed)?.into(),
            name(Conclusion::Failure)?.into(),
            name(StatusReason::AgentLost)?.into(),
        ],
    ))
    .await?;
    let run_id = row::id("attempt", run_id)?;
    let attempt_id = row::id("job", attempt_id)?;
    let execution_id = row::id("execution", execution)?;
    let job = row::id("execution", job_id)?;
    let event = |event| NewEvent {
        run_id,
        attempt_id: Some(attempt_id),
        job_id: Some(job),
        execution_id: Some(execution_id),
        step_id: None,
        actor: Actor::System,
        event,
    };
    journal::append(
        db,
        event(Event::ExecutionStatusChanged(ExecutionStatusChanged {
            from,
            to: ExecutionStatus::Completed,
            conclusion: Some(Conclusion::Failure),
            reason: Some(StatusReason::AgentLost),
        })),
    )
    .await?;
    if matches!(
        job_from,
        JobStatus::InProgress | JobStatus::Paused | JobStatus::Held
    ) {
        db.execute(statement(
            "UPDATE job SET status = $2, waiting_reason = NULL, conclusion = $3, \
             conclusion_source = $4, completed_at = now() WHERE id = $1",
            [
                job_id.into(),
                name(JobStatus::Completed)?.into(),
                name(Conclusion::Failure)?.into(),
                name(ConclusionSource::Natural)?.into(),
            ],
        ))
        .await?;
        journal::append(
            db,
            event(Event::JobStatusChanged(JobStatusChanged {
                from: job_from,
                to: JobStatus::Completed,
                waiting_reason: None,
                conclusion: Some(Conclusion::Failure),
                conclusion_source: Some(ConclusionSource::Natural),
                reason: Some(StatusReason::AgentLost),
            })),
        )
        .await?;
    }
    Ok(true)
}

async fn running<C: ConnectionTrait>(
    db: &C,
    agents: &[Uuid],
) -> Result<HashMap<Uuid, ExecutionId>, Error> {
    if agents.is_empty() {
        return Ok(HashMap::new());
    }
    let mut running = HashMap::new();
    for found in execution::Entity::find()
        .filter(execution::Column::AgentId.is_in(agents.iter().copied()))
        .filter(execution::Column::Status.ne(name(ExecutionStatus::Completed)?))
        .order_by_asc(execution::Column::Id)
        .all(db)
        .await?
    {
        running.insert(found.agent_id, row::id("execution", found.id)?);
    }
    Ok(running)
}

fn decode(agent: agent::Model, execution_id: Option<ExecutionId>) -> Result<Agent, Error> {
    Ok(Agent {
        id: row::id(TABLE, agent.id)?,
        name: agent.name,
        pool_id: row::id(TABLE, agent.pool_id)?,
        labels: agent.labels,
        capabilities: agent
            .capabilities
            .into_iter()
            .map(|capability| row::name(TABLE, capability))
            .collect::<Result<Vec<Capability>, Error>>()?,
        version: agent.version,
        online: agent.online,
        last_seen_at: agent.last_seen_at,
        execution_id,
        created_at: agent.created_at,
    })
}

fn running_names() -> Result<Vec<String>, Error> {
    RUNNING.iter().map(name).collect()
}

fn name<T: Serialize>(value: T) -> Result<String, Error> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(name) => Ok(name),
        other => Err(Error::Row {
            table: TABLE,
            message: format!("{other} is not serialised as a name"),
        }),
    }
}

fn statement(sql: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}
