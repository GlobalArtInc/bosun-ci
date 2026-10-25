use std::collections::BTreeMap;
use std::time::Duration;

use bosun_types::id::{AgentId, AttemptId, ExecutionId, JobId, PoolId, RunId};
use bosun_types::ir::{self, JobTemplate, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{ExecutionStatus, JobStatus};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult, TransactionTrait};
use serde_json::Value;

use crate::periodic::{self, Task};
use crate::row::statement;
use crate::runs::{self, Evaluated, Locked, RUN_STATE};
use crate::{Error, row};

pub(crate) const CHANNEL: &str = "bosun_queue";

pub struct Claim<'a> {
    pub agent: AgentId,
    pub replica_id: Uuid,
    pub lease: Duration,
    pub passed_over: &'a [JobId],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claimed {
    Job(Box<ClaimedJob>),
    Contended,
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub job_id: JobId,
    pub execution_id: ExecutionId,
    pub key: JobKey,
    pub name: String,
    pub organisation: String,
    pub project: String,
    pub env: BTreeMap<String, Template>,
    pub job: JobTemplate,
    pub evaluated: Evaluated,
    pub continue_on_error: bool,
}

const CLAIMABLE: &str = "j.status = $1 AND j.labels <@ $2 AND $3 = ANY (j.pool_ids) \
     AND NOT (j.id = ANY ($4))";

pub async fn claim<C: TransactionTrait>(db: &C, claim: Claim<'_>) -> Result<Claimed, Error> {
    let transaction = db.begin().await?;
    let Some(agent) = transaction
        .query_one(statement(
            "SELECT pool_id, labels FROM agent WHERE id = $1 AND replica_id = $2 \
             FOR NO KEY UPDATE",
            [Uuid::from(claim.agent).into(), claim.replica_id.into()],
        ))
        .await?
    else {
        return Ok(Claimed::Nothing);
    };
    let busy = transaction
        .query_one(statement(
            "SELECT EXISTS (SELECT 1 FROM execution WHERE agent_id = $1 AND status <> $2) \
             AS busy",
            [
                Uuid::from(claim.agent).into(),
                row::text("execution", ExecutionStatus::Completed)?.into(),
            ],
        ))
        .await?
        .map(|found| found.try_get::<bool>("", "busy"))
        .transpose()?
        .unwrap_or(false);
    if busy {
        return Ok(Claimed::Nothing);
    }
    let pool: PoolId = row::id("agent", agent.try_get::<Uuid>("", "pool_id")?)?;
    let labels: Vec<String> = agent.try_get("", "labels")?;
    let passed_over: Vec<Uuid> = claim.passed_over.iter().copied().map(Uuid::from).collect();
    let filter = [
        row::text("job", JobStatus::Queued)?.into(),
        labels.into(),
        Uuid::from(pool).into(),
        passed_over.into(),
    ];
    let Some(candidate) = transaction
        .query_one(statement(
            &format!(
                "SELECT j.id AS job_id, j.key, j.name, a.run_id, {RUN_STATE} FROM job j \
                 JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id \
                 WHERE {CLAIMABLE} ORDER BY j.priority DESC, j.queued_at, j.id LIMIT 1 \
                 FOR UPDATE OF j SKIP LOCKED FOR NO KEY UPDATE OF r SKIP LOCKED"
            ),
            filter.clone(),
        ))
        .await?
    else {
        let contended = transaction
            .query_one(statement(
                &format!("SELECT EXISTS (SELECT 1 FROM job j WHERE {CLAIMABLE}) AS contended"),
                filter,
            ))
            .await?
            .map(|found| found.try_get::<bool>("", "contended"))
            .transpose()?
            .unwrap_or(false);
        return Ok(if contended {
            Claimed::Contended
        } else {
            Claimed::Nothing
        });
    };
    let claimed = assign(&transaction, &candidate, claim.agent, claim.lease).await?;
    transaction.commit().await?;
    Ok(Claimed::Job(Box::new(claimed)))
}

async fn assign(
    transaction: &DatabaseTransaction,
    candidate: &QueryResult,
    agent: AgentId,
    lease: Duration,
) -> Result<ClaimedJob, Error> {
    let run_id: RunId = row::id("run", candidate.try_get::<Uuid>("", "run_id")?)?;
    let job_id: JobId = row::id("job", candidate.try_get::<Uuid>("", "job_id")?)?;
    let key: JobKey = row::name("job", candidate.try_get("", "key")?)?;
    let run = Locked::new(transaction, runs::decode_state(run_id, candidate)?);
    let execution_id = run.assign_job(job_id, agent, lease).await?;
    let found = transaction
        .query_one(statement(
            "SELECT r.ir, o.slug AS organisation, p.slug AS project, j.evaluated, \
             j.continue_on_error FROM run r JOIN project p ON p.id = r.project_id \
             JOIN organisation o ON o.id = p.organisation_id JOIN job j ON j.id = $2 \
             WHERE r.id = $1",
            [Uuid::from(run_id).into(), Uuid::from(job_id).into()],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {run_id}")))?;
    let workflow: ir::Workflow = row::json("run", found.try_get("", "ir")?)?;
    let job = workflow
        .jobs
        .into_iter()
        .find(|template| template.key == key)
        .ok_or_else(|| Error::Row {
            table: "run",
            message: format!("the IR of the run has no job `{key}`"),
        })?;
    let evaluated = found
        .try_get::<Option<Value>>("", "evaluated")?
        .ok_or_else(|| Error::Row {
            table: "job",
            message: format!("job {job_id} was queued without the values its assignment carries"),
        })?;
    Ok(ClaimedJob {
        run_id,
        attempt_id: run.state().attempt_id,
        job_id,
        execution_id,
        key,
        name: candidate.try_get("", "name")?,
        organisation: found.try_get("", "organisation")?,
        project: found.try_get("", "project")?,
        env: workflow.env,
        job,
        evaluated: row::json("job", evaluated)?,
        continue_on_error: found.try_get("", "continue_on_error")?,
    })
}

pub async fn expired_leases<C: TransactionTrait>(
    db: &C,
) -> Result<Option<Vec<ExecutionId>>, Error> {
    let transaction = db.begin().await?;
    if !periodic::try_lock(&transaction, Task::LeaseExpiry).await? {
        return Ok(None);
    }
    let found = transaction
        .query_all(statement(
            "SELECT id FROM execution WHERE status = $1 AND lease_until < now() ORDER BY id",
            [row::text("execution", ExecutionStatus::Assigned)?.into()],
        ))
        .await?;
    transaction.commit().await?;
    found
        .into_iter()
        .map(|found| row::id("execution", found.try_get::<Uuid>("", "id")?))
        .collect::<Result<Vec<ExecutionId>, Error>>()
        .map(Some)
}

pub async fn overdue<C: TransactionTrait>(
    db: &C,
    max_queue_time: Duration,
) -> Result<Option<Vec<JobId>>, Error> {
    let transaction = db.begin().await?;
    if !periodic::try_lock(&transaction, Task::QueueTimeout).await? {
        return Ok(None);
    }
    let found = transaction
        .query_all(statement(
            "SELECT id FROM job WHERE status = $1 \
             AND queued_at < now() - make_interval(secs => $2) ORDER BY id",
            [
                row::text("job", JobStatus::Queued)?.into(),
                max_queue_time.as_secs_f64().into(),
            ],
        ))
        .await?;
    transaction.commit().await?;
    found
        .into_iter()
        .map(|found| row::id("job", found.try_get::<Uuid>("", "id")?))
        .collect::<Result<Vec<JobId>, Error>>()
        .map(Some)
}

pub(crate) async fn announce(transaction: &DatabaseTransaction, job: JobId) -> Result<(), Error> {
    transaction
        .execute(statement(
            "SELECT pg_notify($1, $2)",
            [CHANNEL.into(), job.to_string().into()],
        ))
        .await?;
    Ok(())
}
