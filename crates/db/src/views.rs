use std::collections::HashMap;

use bosun_types::actor::Actor;
use bosun_types::api::{Execution, Job, Notice, Run, Step};
use bosun_types::id::{ExecutionId, JobId, RunId};
use sea_orm::prelude::Uuid;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde_json::Value;

use crate::entity::{attempt, execution, job, run, run_event, step};
use crate::row::statement;
use crate::{Error, row};

pub async fn run<C: ConnectionTrait>(db: &C, id: RunId) -> Result<Option<Run>, Error> {
    let Some(found) = run::Entity::find_by_id(Uuid::from(id)).one(db).await? else {
        return Ok(None);
    };
    let latest = attempt::Entity::find()
        .filter(attempt::Column::RunId.eq(found.id))
        .order_by_desc(attempt::Column::Number)
        .one(db)
        .await?
        .ok_or_else(|| Error::Vanished(format!("the attempts of run {id}")))?;
    let actor: Actor = row::json(
        "run",
        found.trigger.get("actor").cloned().unwrap_or(Value::Null),
    )?;
    let notices = run_event::Entity::find()
        .filter(run_event::Column::RunId.eq(found.id))
        .filter(run_event::Column::Kind.eq("run.notice"))
        .order_by_asc(run_event::Column::Seq)
        .all(db)
        .await?
        .into_iter()
        .map(|event| {
            Ok(Notice {
                job_id: row::optional_id("run_event", event.job_id)?,
                diagnostic: row::json("run_event", event.data)?,
            })
        })
        .collect::<Result<Vec<Notice>, Error>>()?;
    Ok(Some(Run {
        id,
        project_id: row::id("run", found.project_id)?,
        workflow_id: row::id("run", found.workflow_id)?,
        revision_id: row::id("run", found.revision_id)?,
        number: row::unsigned("run", found.number)?,
        global_number: row::unsigned("run", found.global_number)?,
        display_name: found.display_name.unwrap_or_default(),
        event: found.event,
        status: row::name("run", found.status)?,
        conclusion: found
            .conclusion
            .map(|conclusion| row::name("run", conclusion))
            .transpose()?,
        attempt: whole("attempt", latest.number)?,
        actor,
        notices,
        created_at: found.created_at,
        started_at: found.started_at,
        completed_at: found.completed_at,
    }))
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
    let steps = step::Entity::find()
        .filter(step::Column::ExecutionId.eq(found.id))
        .order_by_asc(step::Column::Index)
        .all(db)
        .await?
        .into_iter()
        .map(|step| decode_step(step, id))
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

fn decode_step(step: step::Model, execution: ExecutionId) -> Result<Step, Error> {
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
    })
}

fn whole(table: &'static str, number: i32) -> Result<u32, Error> {
    u32::try_from(number).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}
