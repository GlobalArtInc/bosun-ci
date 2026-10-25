use std::collections::HashMap;

use bosun_types::actor::Actor;
use bosun_types::api::{Annotation, Execution, Job, Notice, QueuedJob, Run, Step};
use bosun_types::id::{ExecutionId, JobId, ProjectId, RunId, WorkflowId};
use bosun_types::status::{JobStatus, RunConclusion, RunStatus};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QueryResult, QuerySelect,
};
use serde_json::Value;

use crate::entity::{attempt, execution, job, run, run_event, step};
use crate::projects::Reach;
use crate::results;
use crate::row::statement;
use crate::{Error, row};

const BRANCHES: &str = "refs/heads/";

pub async fn run<C: ConnectionTrait>(db: &C, id: RunId) -> Result<Option<Run>, Error> {
    let Some(found) = run::Entity::find_by_id(Uuid::from(id)).one(db).await? else {
        return Ok(None);
    };
    Ok(decode_runs(db, vec![found]).await?.pop())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusOrConclusion {
    Status(RunStatus),
    Conclusion(RunConclusion),
}

#[derive(Debug, Clone, Default)]
pub struct RunFilter<'a> {
    pub project: Option<ProjectId>,
    pub workflow: Option<WorkflowId>,
    pub branch: Option<&'a str>,
    pub event: Option<&'a str>,
    pub status: Option<StatusOrConclusion>,
    pub actor: Option<&'a str>,
}

pub async fn runs<C: ConnectionTrait>(
    db: &C,
    reach: &Reach,
    filter: &RunFilter<'_>,
    before: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Run>, Error> {
    let mut query = run::Entity::find()
        .order_by_desc(run::Column::Id)
        .limit(limit);
    if let Some(condition) = reach.condition(run::Column::ProjectId) {
        query = query.filter(condition);
    }
    if let Some(project) = filter.project {
        query = query.filter(run::Column::ProjectId.eq(Uuid::from(project)));
    }
    if let Some(workflow) = filter.workflow {
        query = query.filter(run::Column::WorkflowId.eq(Uuid::from(workflow)));
    }
    if let Some(branch) = filter.branch {
        query = query.filter(run::Column::GitRef.eq(format!("{BRANCHES}{branch}")));
    }
    if let Some(event) = filter.event {
        query = query.filter(run::Column::Event.eq(event));
    }
    match filter.status {
        Some(StatusOrConclusion::Status(status)) => {
            query = query.filter(run::Column::Status.eq(row::text("run", status)?));
        }
        Some(StatusOrConclusion::Conclusion(conclusion)) => {
            query = query.filter(run::Column::Conclusion.eq(row::text("run", conclusion)?));
        }
        None => {}
    }
    if let Some(actor) = filter.actor {
        query = query.filter(Expr::cust_with_values(
            "\"run\".\"trigger\" -> 'actor' ->> 'login' = $1",
            [actor],
        ));
    }
    if let Some(before) = before {
        query = query.filter(run::Column::Id.lt(before));
    }
    let found = query.all(db).await?;
    decode_runs(db, found).await
}

pub(crate) async fn latest_runs<C: ConnectionTrait>(
    db: &C,
    workflows: Vec<Uuid>,
) -> Result<HashMap<WorkflowId, Run>, Error> {
    if workflows.is_empty() {
        return Ok(HashMap::new());
    }
    let found = run::Entity::find()
        .from_raw_sql(statement(
            &format!(
                "SELECT l.* FROM workflow w JOIN project p ON p.id = w.project_id \
                 LEFT JOIN forge_repository f ON f.id = p.repository_id \
                 CROSS JOIN LATERAL (SELECT r.* FROM run r WHERE r.workflow_id = w.id \
                 AND (f.id IS NULL OR r.ref = '{BRANCHES}' || f.default_branch) \
                 ORDER BY r.number DESC LIMIT 1) l WHERE w.id = ANY($1)"
            ),
            [workflows.into()],
        ))
        .all(db)
        .await?;
    Ok(decode_runs(db, found)
        .await?
        .into_iter()
        .map(|run| (run.workflow_id, run))
        .collect())
}

async fn decode_runs<C: ConnectionTrait>(
    db: &C,
    found: Vec<run::Model>,
) -> Result<Vec<Run>, Error> {
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = found.iter().map(|run| run.id).collect();
    let mut latest: HashMap<Uuid, i32> = HashMap::new();
    for attempt in db
        .query_all(statement(
            "SELECT run_id, max(number) AS number FROM attempt WHERE run_id = ANY($1) \
             GROUP BY run_id",
            [ids.clone().into()],
        ))
        .await?
    {
        latest.insert(
            attempt.try_get::<Uuid>("", "run_id")?,
            attempt.try_get::<i32>("", "number")?,
        );
    }
    let mut notices: HashMap<Uuid, Vec<Notice>> = HashMap::new();
    for event in run_event::Entity::find()
        .filter(run_event::Column::RunId.is_in(ids))
        .filter(run_event::Column::Kind.eq("run.notice"))
        .order_by_asc(run_event::Column::Seq)
        .all(db)
        .await?
    {
        notices.entry(event.run_id).or_default().push(Notice {
            job_id: row::optional_id("run_event", event.job_id)?,
            diagnostic: row::json("run_event", event.data)?,
        });
    }
    found
        .into_iter()
        .map(|found| {
            let id: RunId = row::id("run", found.id)?;
            let attempt = latest
                .get(&found.id)
                .copied()
                .ok_or_else(|| Error::Vanished(format!("the attempts of run {id}")))?;
            let actor: Actor = row::json(
                "run",
                found.trigger.get("actor").cloned().unwrap_or(Value::Null),
            )?;
            Ok(Run {
                id,
                project_id: row::id("run", found.project_id)?,
                workflow_id: row::id("run", found.workflow_id)?,
                revision_id: row::id("run", found.revision_id)?,
                number: row::unsigned("run", found.number)?,
                global_number: row::unsigned("run", found.global_number)?,
                display_name: found.display_name.unwrap_or_default(),
                event: found.event,
                r#ref: found.git_ref,
                sha: found.sha,
                status: row::name("run", found.status)?,
                conclusion: found
                    .conclusion
                    .map(|conclusion| row::name("run", conclusion))
                    .transpose()?,
                attempt: whole("attempt", attempt)?,
                actor,
                notices: notices.remove(&found.id).unwrap_or_default(),
                created_at: found.created_at,
                started_at: found.started_at,
                completed_at: found.completed_at,
            })
        })
        .collect()
}

pub async fn jobs<C: ConnectionTrait>(
    db: &C,
    run: RunId,
    attempt: u32,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Option<Vec<Job>>, Error> {
    let Some(found) = attempt::Entity::find()
        .filter(attempt::Column::RunId.eq(Uuid::from(run)))
        .filter(attempt::Column::Number.eq(i64::from(attempt)))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let mut query = job::Entity::find()
        .filter(job::Column::AttemptId.eq(found.id))
        .order_by_asc(job::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(job::Column::Id.gt(after));
    }
    let jobs = query.all(db).await?;
    let ids: Vec<Uuid> = jobs.iter().map(|job| job.id).collect();
    let executions = latest_executions(db, &ids).await?;
    jobs.into_iter()
        .map(|job| {
            let execution = executions.get(&job.id).copied();
            decode_job(job, run, execution)
        })
        .collect::<Result<Vec<Job>, Error>>()
        .map(Some)
}

pub async fn job<C: ConnectionTrait>(db: &C, id: JobId) -> Result<Option<Job>, Error> {
    let Some(found) = job::Entity::find_by_id(Uuid::from(id)).one(db).await? else {
        return Ok(None);
    };
    let owner = attempt::Entity::find_by_id(found.attempt_id)
        .one(db)
        .await?
        .ok_or_else(|| Error::Vanished(format!("the attempt of job {id}")))?;
    let executions = latest_executions(db, &[found.id]).await?;
    let execution = executions.get(&found.id).copied();
    decode_job(found, row::id("attempt", owner.run_id)?, execution).map(Some)
}

pub async fn execution<C: ConnectionTrait>(
    db: &C,
    id: ExecutionId,
) -> Result<Option<Execution>, Error> {
    let Some(found) = execution::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let stored = step::Entity::find()
        .filter(step::Column::ExecutionId.eq(found.id))
        .order_by_asc(step::Column::Index)
        .all(db)
        .await?;
    let ids: Vec<Uuid> = stored.iter().map(|step| step.id).collect();
    let mut reported = results::of_steps(db, &ids).await?;
    let steps = stored
        .into_iter()
        .map(|step| {
            let (annotations, summary_size) = reported.remove(&step.id).unwrap_or_default();
            decode_step(step, id, annotations, summary_size)
        })
        .collect::<Result<Vec<Step>, Error>>()?;
    Ok(Some(Execution {
        id,
        job_id: row::id("execution", found.job_id)?,
        number: whole("execution", found.number)?,
        agent_id: row::id("execution", found.agent_id)?,
        status: row::name("execution", found.status)?,
        conclusion: found
            .conclusion
            .map(|conclusion| row::name("execution", conclusion))
            .transpose()?,
        reason: found
            .reason
            .map(|reason| row::name("execution", reason))
            .transpose()?,
        started_at: found.started_at,
        completed_at: found.completed_at,
        steps,
    }))
}

pub async fn queue<C: ConnectionTrait>(
    db: &C,
    reach: &Reach,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<QueuedJob>, Error> {
    let mut values: Vec<sea_orm::Value> = vec![
        after.unwrap_or(Uuid::nil()).into(),
        i64::try_from(limit).unwrap_or(i64::MAX).into(),
    ];
    let reach = if reach.everywhere {
        "true".to_owned()
    } else {
        let projects: Vec<Uuid> = reach.projects.iter().copied().map(Uuid::from).collect();
        let organisations: Vec<Uuid> = reach
            .organisations
            .iter()
            .copied()
            .map(Uuid::from)
            .collect();
        values.push(projects.into());
        values.push(organisations.into());
        "(r.project_id = ANY($3) OR r.project_id IN \
         (SELECT id FROM project WHERE organisation_id = ANY($4)))"
            .to_owned()
    };
    let found = db
        .query_all(statement(
            &format!(
                "{} WHERE j.status IN ({}, {}) AND j.id > $1 AND {reach} ORDER BY j.id LIMIT $2",
                entry()?,
                quoted(JobStatus::Queued)?,
                quoted(JobStatus::Waiting)?,
            ),
            values,
        ))
        .await?;
    entries(db, found).await
}

pub(crate) async fn queued_job<C: ConnectionTrait>(
    db: &C,
    job: JobId,
) -> Result<Option<QueuedJob>, Error> {
    let found = db
        .query_all(statement(
            &format!("{} WHERE j.id = $1", entry()?),
            [Uuid::from(job).into()],
        ))
        .await?;
    Ok(entries(db, found).await?.pop())
}

fn entry() -> Result<String, Error> {
    let queued = quoted(JobStatus::Queued)?;
    let waiting = quoted(JobStatus::Waiting)?;
    Ok(format!(
        "SELECT j.id, a.run_id, r.project_id, CASE j.status WHEN {queued} THEN j.queued_at \
         WHEN {waiting} THEN (SELECT e.at FROM run_event e WHERE e.run_id = a.run_id \
         AND e.job_id = j.id AND e.kind = 'job.status' ORDER BY e.seq DESC LIMIT 1) END AS since, \
         CASE WHEN j.status IN ({queued}, {waiting}) THEN ARRAY(SELECT l.label \
         FROM unnest(j.labels) WITH ORDINALITY AS l (label, place) WHERE NOT EXISTS \
         (SELECT 1 FROM agent g WHERE g.online AND l.label = ANY (g.labels)) ORDER BY l.place) \
         ELSE '{{}}' END AS missing_labels \
         FROM job j JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id"
    ))
}

fn quoted(status: JobStatus) -> Result<String, Error> {
    Ok(format!("'{}'", row::text("job", status)?))
}

async fn entries<C: ConnectionTrait>(
    db: &C,
    found: Vec<QueryResult>,
) -> Result<Vec<QueuedJob>, Error> {
    let ids: Vec<Uuid> = found
        .iter()
        .map(|found| found.try_get::<Uuid>("", "id"))
        .collect::<Result<_, _>>()?;
    let mut jobs: HashMap<Uuid, job::Model> = job::Entity::find()
        .filter(job::Column::Id.is_in(ids.clone()))
        .all(db)
        .await?
        .into_iter()
        .map(|stored| (stored.id, stored))
        .collect();
    let executions = latest_executions(db, &ids).await?;
    let mut entries = Vec::with_capacity(found.len());
    for found in found {
        let id: Uuid = found.try_get("", "id")?;
        let stored = jobs
            .remove(&id)
            .ok_or_else(|| Error::Vanished(format!("job {id}")))?;
        let run: RunId = row::id("attempt", found.try_get::<Uuid>("", "run_id")?)?;
        entries.push(QueuedJob {
            job: decode_job(stored, run, executions.get(&id).copied())?,
            project_id: row::id("run", found.try_get::<Uuid>("", "project_id")?)?,
            since: found.try_get("", "since")?,
            missing_labels: found.try_get("", "missing_labels")?,
        });
    }
    Ok(entries)
}

async fn latest_executions<C: ConnectionTrait>(
    db: &C,
    jobs: &[Uuid],
) -> Result<HashMap<Uuid, ExecutionId>, Error> {
    if jobs.is_empty() {
        return Ok(HashMap::new());
    }
    let mut latest = HashMap::new();
    for found in db
        .query_all(statement(
            "SELECT DISTINCT ON (job_id) job_id, id FROM execution WHERE job_id = ANY($1) \
             ORDER BY job_id, number DESC",
            [jobs.to_vec().into()],
        ))
        .await?
    {
        latest.insert(
            found.try_get::<Uuid>("", "job_id")?,
            row::id("execution", found.try_get::<Uuid>("", "id")?)?,
        );
    }
    Ok(latest)
}

fn decode_job(job: job::Model, run: RunId, execution: Option<ExecutionId>) -> Result<Job, Error> {
    Ok(Job {
        id: row::id("job", job.id)?,
        run_id: run,
        attempt_id: row::id("job", job.attempt_id)?,
        key: row::name("job", job.key)?,
        name: job.name,
        group_id: row::optional_id("job", job.group_id)?,
        matrix: job
            .matrix
            .map(|matrix| row::json("job", matrix))
            .transpose()?,
        needs: job
            .needs
            .into_iter()
            .map(|need| row::name("job", need))
            .collect::<Result<_, Error>>()?,
        labels: job.labels,
        status: row::name("job", job.status)?,
        waiting_reason: job
            .waiting_reason
            .map(|reason| row::name("job", reason))
            .transpose()?,
        conclusion: job
            .conclusion
            .map(|conclusion| row::name("job", conclusion))
            .transpose()?,
        conclusion_source: job
            .conclusion_source
            .map(|source| row::name("job", source))
            .transpose()?,
        outputs: row::json("job", job.outputs)?,
        execution_id: execution,
        queued_at: job.queued_at,
        started_at: job.started_at,
        completed_at: job.completed_at,
    })
}

fn decode_step(
    step: step::Model,
    execution: ExecutionId,
    annotations: Vec<Annotation>,
    summary_size: Option<u64>,
) -> Result<Step, Error> {
    Ok(Step {
        id: row::id("step", step.id)?,
        execution_id: execution,
        index: whole("step", step.index)?,
        name: step.name,
        status: row::name("step", step.status)?,
        outcome: step
            .outcome
            .map(|outcome| row::name("step", outcome))
            .transpose()?,
        conclusion: step
            .conclusion
            .map(|conclusion| row::name("step", conclusion))
            .transpose()?,
        conclusion_source: step
            .conclusion_source
            .map(|source| row::name("step", source))
            .transpose()?,
        started_at: step.started_at,
        completed_at: step.completed_at,
        annotations,
        summary_size,
    })
}

fn whole(table: &'static str, number: i32) -> Result<u32, Error> {
    u32::try_from(number).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}
