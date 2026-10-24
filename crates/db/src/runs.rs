use bosun_types::actor::Actor;
use bosun_types::api::{Run, Workflow};
use bosun_types::event::{AttemptCreated, Event, JobCreated, RunCreated};
use bosun_types::id::{AttemptId, JobId, RevisionId, RunId};
use bosun_types::ir::{self, Template};
use bosun_types::status::{JobStatus, RunStatus};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};
use serde_json::json;

use crate::entity::{attempt, job, run, workflow};
use crate::journal::{self, NewEvent};
use crate::{Error, row};

const RUNS: &str = "run";

const DISPATCH: &str = "workflow_dispatch";

const FIRST_ATTEMPT: u8 = 1;

pub struct NewRun<'a> {
    pub workflow: &'a Workflow,
    pub revision_id: RevisionId,
    pub ir: &'a ir::Workflow,
    pub actor: &'a Actor,
}

pub async fn dispatch<C: ConnectionTrait>(db: &C, new: NewRun<'_>) -> Result<Run, Error> {
    let number = next_number(db, new.workflow).await?;
    let actor = serde_json::to_value(new.actor)?;
    let created = run::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        project_id: ActiveValue::Set(new.workflow.project_id.into()),
        workflow_id: ActiveValue::Set(new.workflow.id.into()),
        revision_id: ActiveValue::Set(new.revision_id.into()),
        ir: ActiveValue::Set(serde_json::to_value(new.ir)?),
        number: ActiveValue::Set(number),
        global_number: ActiveValue::NotSet,
        event: ActiveValue::Set(DISPATCH.to_owned()),
        trigger: ActiveValue::Set(json!({ "actor": actor, "payload": { "inputs": {} } })),
        git_ref: ActiveValue::Set(None),
        sha: ActiveValue::Set(None),
        inputs: ActiveValue::NotSet,
        status: ActiveValue::Set(row::text(RUNS, RunStatus::Queued)?),
        conclusion: ActiveValue::Set(None),
        created_at: ActiveValue::NotSet,
        started_at: ActiveValue::Set(None),
        completed_at: ActiveValue::Set(None),
    }
    .insert(db)
    .await?;
    let run_id: RunId = row::id(RUNS, created.id)?;
    let number = row::unsigned(RUNS, created.number)?;
    let global_number = row::unsigned(RUNS, created.global_number)?;
    record(
        db,
        new.actor,
        run_id,
        None,
        None,
        Event::RunCreated(RunCreated {
            workflow_id: new.workflow.id,
            revision_id: new.revision_id,
            number,
            global_number,
            event: DISPATCH.to_owned(),
        }),
    )
    .await?;

    let attempt = attempt::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        run_id: ActiveValue::Set(created.id),
        number: ActiveValue::Set(i32::from(FIRST_ATTEMPT)),
        actor: ActiveValue::Set(actor),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    let attempt_id: AttemptId = row::id("attempt", attempt.id)?;
    record(
        db,
        new.actor,
        run_id,
        Some(attempt_id),
        None,
        Event::AttemptCreated(AttemptCreated {
            number: u32::from(FIRST_ATTEMPT),
        }),
    )
    .await?;

    let pending = row::text("job", JobStatus::Pending)?;
    for template in &new.ir.jobs {
        let name = template
            .name
            .as_ref()
            .and_then(Template::as_literal)
            .unwrap_or_else(|| template.key.to_string());
        let created = job::ActiveModel {
            id: ActiveValue::Set(Uuid::now_v7()),
            attempt_id: ActiveValue::Set(attempt.id),
            key: ActiveValue::Set(template.key.to_string()),
            name: ActiveValue::Set(name.clone()),
            needs: ActiveValue::Set(template.needs.iter().map(ToString::to_string).collect()),
            status: ActiveValue::Set(pending.clone()),
            ..Default::default()
        }
        .insert(db)
        .await?;
        let job_id: JobId = row::id("job", created.id)?;
        record(
            db,
            new.actor,
            run_id,
            Some(attempt_id),
            Some(job_id),
            Event::JobCreated(JobCreated {
                key: template.key.clone(),
                name,
                needs: template.needs.clone(),
            }),
        )
        .await?;
    }

    Ok(Run {
        id: run_id,
        project_id: new.workflow.project_id,
        workflow_id: new.workflow.id,
        revision_id: new.revision_id,
        number,
        global_number,
        event: created.event,
        status: RunStatus::Queued,
        conclusion: None,
        attempt: u32::from(FIRST_ATTEMPT),
        actor: new.actor.clone(),
        created_at: created.created_at,
        started_at: None,
        completed_at: None,
    })
}

async fn next_number<C: ConnectionTrait>(db: &C, workflow: &Workflow) -> Result<i64, Error> {
    workflow::Entity::update_many()
        .col_expr(
            workflow::Column::LastRunNumber,
            Expr::col(workflow::Column::LastRunNumber).add(1),
        )
        .filter(workflow::Column::Id.eq(Uuid::from(workflow.id)))
        .exec_with_returning(db)
        .await?
        .into_iter()
        .next()
        .map(|counted| counted.last_run_number)
        .ok_or_else(|| Error::Vanished(format!("workflow {}", workflow.id)))
}

async fn record<C: ConnectionTrait>(
    db: &C,
    actor: &Actor,
    run_id: RunId,
    attempt_id: Option<AttemptId>,
    job_id: Option<JobId>,
    event: Event,
) -> Result<(), Error> {
    journal::append(
        db,
        NewEvent {
            run_id,
            attempt_id,
            job_id,
            execution_id: None,
            step_id: None,
            actor: actor.clone(),
            event,
        },
    )
    .await?;
    Ok(())
}
