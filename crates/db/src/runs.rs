use std::collections::BTreeMap;
use std::fmt::{Debug, Display};

use bosun_types::actor::Actor;
use bosun_types::api::{Run, Workflow};
use bosun_types::diagnostic::Diagnostic;
use bosun_types::event::{
    AttemptCreated, Event, JobCreated, RunCreated, RunEvent, RunStatusChanged,
};
use bosun_types::id::{
    AgentId, AttemptId, ExecutionId, JobId, OrganisationId, ProjectId, RevisionId, RunId, StepId,
};
use bosun_types::ir::{self, Shell, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{
    Conclusion, ExecutionStatus, JobStatus, RunConclusion, RunStatus, WaitingReason,
};
use sea_orm::prelude::{DateTimeUtc, Uuid};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait,
    QueryFilter, QueryResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::entity::{attempt, job, run, workflow};
use crate::journal::{self, NewEvent};
use crate::row::statement;
use crate::{Error, row};

const RUNS: &str = "run";

const DISPATCH: &str = "workflow_dispatch";

const FIRST_ATTEMPT: u8 = 1;

pub struct Locked<'a> {
    transaction: &'a DatabaseTransaction,
    state: RunState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunState {
    pub id: RunId,
    pub attempt_id: AttemptId,
    pub attempt: u32,
    pub status: RunStatus,
    pub conclusion: Option<RunConclusion>,
    pub cancel_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobState {
    pub id: JobId,
    pub key: JobKey,
    pub name: String,
    pub needs: Vec<JobKey>,
    pub group: Option<JobId>,
    pub matrix: Option<BTreeMap<String, Value>>,
    pub strategy: Option<Strategy>,
    pub status: JobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub conclusion: Option<Conclusion>,
    pub continue_on_error: bool,
    pub outputs: BTreeMap<String, String>,
    pub started: bool,
    pub completed_at: Option<DateTimeUtc>,
    pub execution: Option<ExecutionState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Strategy {
    pub fail_fast: bool,
    pub job_index: u32,
    pub job_total: u32,
    pub max_parallel: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewMember {
    pub name: String,
    pub matrix: BTreeMap<String, Value>,
    pub strategy: Strategy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluated {
    pub timeout_minutes: u32,
    pub env: BTreeMap<String, String>,
    pub shell: Option<Shell>,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEnd {
    pub outcome: Conclusion,
    pub conclusion: Conclusion,
    pub at: Option<DateTimeUtc>,
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunFacts {
    pub number: u64,
    pub global_number: u64,
    pub event: String,
    pub payload: Value,
    pub inputs: Value,
    pub actor: Actor,
    pub triggering_actor: Actor,
    pub workflow: String,
    pub organisation: String,
    pub project: String,
    pub organisation_id: OrganisationId,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionState {
    pub id: ExecutionId,
    pub agent_id: AgentId,
    pub agent_online: bool,
    pub status: ExecutionStatus,
}

pub(crate) const RUN_STATE: &str = "r.status, r.conclusion, \
     r.cancel_requested_at IS NOT NULL AS cancel_requested, a.id AS attempt_id, \
     a.number AS attempt";

pub async fn lock(
    transaction: &DatabaseTransaction,
    run: RunId,
) -> Result<Option<Locked<'_>>, Error> {
    let Some(found) = transaction
        .query_one(statement(
            &format!(
                "SELECT {RUN_STATE} FROM run r JOIN attempt a ON a.run_id = r.id \
                 WHERE r.id = $1 ORDER BY a.number DESC LIMIT 1 FOR NO KEY UPDATE OF r"
            ),
            [Uuid::from(run).into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let state = decode_state(run, &found)?;
    Ok(Some(Locked { transaction, state }))
}

pub async fn lock_job(
    transaction: &DatabaseTransaction,
    job: JobId,
) -> Result<Option<Locked<'_>>, Error> {
    let Some(owner) = transaction
        .query_one(statement(
            "SELECT a.run_id FROM job j JOIN attempt a ON a.id = j.attempt_id WHERE j.id = $1",
            [Uuid::from(job).into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let run = row::id(RUNS, owner.try_get::<Uuid>("", "run_id")?)?;
    lock(transaction, run).await
}

pub(crate) fn decode_state(run: RunId, found: &QueryResult) -> Result<RunState, Error> {
    let attempt: i32 = found.try_get("", "attempt")?;
    Ok(RunState {
        id: run,
        attempt_id: row::id("attempt", found.try_get::<Uuid>("", "attempt_id")?)?,
        attempt: u32::try_from(attempt).map_err(|error| Error::Row {
            table: "attempt",
            message: error.to_string(),
        })?,
        status: row::name(RUNS, found.try_get("", "status")?)?,
        conclusion: found
            .try_get::<Option<String>>("", "conclusion")?
            .map(|conclusion| row::name(RUNS, conclusion))
            .transpose()?,
        cancel_requested: found.try_get("", "cancel_requested")?,
    })
}

pub async fn lock_execution(
    transaction: &DatabaseTransaction,
    execution: ExecutionId,
) -> Result<Option<(Locked<'_>, ExecutionState)>, Error> {
    let Some(owner) = transaction
        .query_one(statement(
            "SELECT a.run_id FROM execution e JOIN job j ON j.id = e.job_id \
             JOIN attempt a ON a.id = j.attempt_id WHERE e.id = $1",
            [Uuid::from(execution).into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let run = row::id(RUNS, owner.try_get::<Uuid>("", "run_id")?)?;
    let Some(locked) = lock(transaction, run).await? else {
        return Ok(None);
    };
    let found = transaction
        .query_one(statement(
            "SELECT e.id AS execution_id, e.agent_id, e.status AS execution_status, \
             g.online AS agent_online FROM execution e JOIN agent g ON g.id = e.agent_id \
             WHERE e.id = $1",
            [Uuid::from(execution).into()],
        ))
        .await?;
    match found {
        Some(found) => Ok(Some((locked, decode_execution(&found)?))),
        None => Ok(None),
    }
}

impl<'a> Locked<'a> {
    pub(crate) fn new(transaction: &'a DatabaseTransaction, state: RunState) -> Locked<'a> {
        Locked { transaction, state }
    }

    pub fn state(&self) -> &RunState {
        &self.state
    }

    pub fn transaction(&self) -> &'a DatabaseTransaction {
        self.transaction
    }

    pub async fn jobs(&self) -> Result<Vec<JobState>, Error> {
        self.transaction
            .query_all(statement(
                "SELECT j.id, j.key, j.name, j.needs, j.group_id, j.matrix, j.strategy, j.status, \
                 j.waiting_reason, j.conclusion, j.continue_on_error, j.outputs, \
                 j.started_at IS NOT NULL AS started, j.completed_at, e.id AS execution_id, e.agent_id, \
                 e.status AS execution_status, e.online AS agent_online FROM job j \
                 LEFT JOIN LATERAL (SELECT x.id, x.agent_id, x.status, g.online \
                 FROM execution x JOIN agent g ON g.id = x.agent_id WHERE x.job_id = j.id \
                 ORDER BY x.number DESC LIMIT 1) e ON true \
                 WHERE j.attempt_id = $1 ORDER BY j.id",
                [Uuid::from(self.state.attempt_id).into()],
            ))
            .await?
            .iter()
            .map(decode_job)
            .collect()
    }

    pub async fn ir(&self) -> Result<ir::Workflow, Error> {
        let found = self
            .transaction
            .query_one(statement(
                "SELECT ir FROM run WHERE id = $1",
                [Uuid::from(self.state.id).into()],
            ))
            .await?
            .ok_or_else(|| Error::Vanished(format!("run {}", self.state.id)))?;
        row::json(RUNS, found.try_get::<Value>("", "ir")?)
    }

    pub async fn facts(&self) -> Result<RunFacts, Error> {
        let found = self
            .transaction
            .query_one(statement(
                "SELECT r.number, r.global_number, r.event, r.trigger, r.inputs, \
                 w.name AS workflow, o.slug AS organisation, p.slug AS project, \
                 o.id AS organisation_id, p.id AS project_id, \
                 a.actor AS triggering_actor FROM run r \
                 JOIN workflow w ON w.id = r.workflow_id JOIN project p ON p.id = r.project_id \
                 JOIN organisation o ON o.id = p.organisation_id JOIN attempt a ON a.id = $2 \
                 WHERE r.id = $1",
                [
                    Uuid::from(self.state.id).into(),
                    Uuid::from(self.state.attempt_id).into(),
                ],
            ))
            .await?
            .ok_or_else(|| Error::Vanished(format!("run {}", self.state.id)))?;
        let mut trigger: Value = found.try_get("", "trigger")?;
        Ok(RunFacts {
            number: row::unsigned(RUNS, found.try_get("", "number")?)?,
            global_number: row::unsigned(RUNS, found.try_get("", "global_number")?)?,
            event: found.try_get("", "event")?,
            payload: trigger
                .get_mut("payload")
                .map(Value::take)
                .unwrap_or(Value::Null),
            inputs: found.try_get("", "inputs")?,
            actor: row::json(
                RUNS,
                trigger
                    .get_mut("actor")
                    .map(Value::take)
                    .unwrap_or(Value::Null),
            )?,
            triggering_actor: row::json("attempt", found.try_get("", "triggering_actor")?)?,
            workflow: found.try_get("", "workflow")?,
            organisation: found.try_get("", "organisation")?,
            project: found.try_get("", "project")?,
            organisation_id: row::id("organisation", found.try_get("", "organisation_id")?)?,
            project_id: row::id("project", found.try_get("", "project_id")?)?,
        })
    }

    pub async fn set_display_name(&self, name: &str) -> Result<(), Error> {
        self.transaction
            .execute(statement(
                "UPDATE run SET display_name = $2 WHERE id = $1",
                [Uuid::from(self.state.id).into(), name.into()],
            ))
            .await?;
        Ok(())
    }

    pub async fn notice(
        &self,
        job: Option<JobId>,
        diagnostic: &Diagnostic,
    ) -> Result<RunEvent, Error> {
        self.append(
            job,
            None,
            None,
            &Actor::System,
            Event::RunNotice(diagnostic.clone()),
        )
        .await
    }

    pub async fn set_status(
        &mut self,
        to: RunStatus,
        conclusion: Option<RunConclusion>,
    ) -> Result<(), Error> {
        let from = self.state.status;
        if !from.can_transition(to) {
            return Err(refuse("run", self.state.id, from, to));
        }
        self.transaction
            .execute(statement(
                "UPDATE run SET status = $2, conclusion = $3, \
                 started_at = CASE WHEN $4 THEN coalesce(started_at, now()) ELSE started_at END, \
                 completed_at = CASE WHEN $5 THEN now() ELSE completed_at END WHERE id = $1",
                [
                    Uuid::from(self.state.id).into(),
                    row::text(RUNS, to)?.into(),
                    conclusion
                        .map(|conclusion| row::text(RUNS, conclusion))
                        .transpose()?
                        .into(),
                    (to == RunStatus::InProgress).into(),
                    (to == RunStatus::Completed).into(),
                ],
            ))
            .await?;
        self.state.status = to;
        self.state.conclusion = conclusion;
        let attempt_id = self.state.attempt_id;
        journal::append(
            self.transaction,
            NewEvent {
                run_id: self.state.id,
                attempt_id: Some(attempt_id),
                job_id: None,
                execution_id: None,
                step_id: None,
                actor: Actor::System,
                event: Event::RunStatusChanged(RunStatusChanged {
                    from,
                    to,
                    conclusion,
                }),
            },
        )
        .await?;
        Ok(())
    }

    pub async fn request_cancel(&mut self) -> Result<(), Error> {
        self.transaction
            .execute(statement(
                "UPDATE run SET cancel_requested_at = coalesce(cancel_requested_at, now()) \
                 WHERE id = $1",
                [Uuid::from(self.state.id).into()],
            ))
            .await?;
        self.state.cancel_requested = true;
        Ok(())
    }

    pub async fn record(
        &self,
        job: Option<JobId>,
        actor: &Actor,
        event: Event,
    ) -> Result<RunEvent, Error> {
        self.append(job, None, None, actor, event).await
    }

    pub(crate) async fn append(
        &self,
        job: Option<JobId>,
        execution: Option<ExecutionId>,
        step: Option<StepId>,
        actor: &Actor,
        event: Event,
    ) -> Result<RunEvent, Error> {
        journal::append(
            self.transaction,
            NewEvent {
                run_id: self.state.id,
                attempt_id: job.map(|_| self.state.attempt_id),
                job_id: job,
                execution_id: execution,
                step_id: step,
                actor: actor.clone(),
                event,
            },
        )
        .await
    }
}

pub(crate) fn refuse<S: Serialize + Debug>(
    entity: &'static str,
    id: impl Display,
    from: S,
    to: S,
) -> Error {
    Error::Transition {
        entity,
        id: id.to_string(),
        from: label(from),
        to: label(to),
    }
}

fn label<S: Serialize + Debug>(status: S) -> String {
    let fallback = format!("{status:?}");
    row::text(RUNS, status).unwrap_or(fallback)
}

fn decode_job(found: &QueryResult) -> Result<JobState, Error> {
    let id: JobId = row::id("job", found.try_get::<Uuid>("", "id")?)?;
    let execution = match found.try_get::<Option<Uuid>>("", "execution_id")? {
        Some(_) => Some(decode_execution(found)?),
        None => None,
    };
    Ok(JobState {
        id,
        key: row::name("job", found.try_get("", "key")?)?,
        name: found.try_get("", "name")?,
        needs: found
            .try_get::<Vec<String>>("", "needs")?
            .into_iter()
            .map(|need| row::name("job", need))
            .collect::<Result<Vec<JobKey>, Error>>()?,
        group: row::optional_id("job", found.try_get("", "group_id")?)?,
        matrix: found
            .try_get::<Option<Value>>("", "matrix")?
            .map(|matrix| row::json("job", matrix))
            .transpose()?,
        strategy: found
            .try_get::<Option<Value>>("", "strategy")?
            .map(|strategy| row::json("job", strategy))
            .transpose()?,
        status: row::name("job", found.try_get("", "status")?)?,
        waiting_reason: found
            .try_get::<Option<String>>("", "waiting_reason")?
            .map(|reason| row::name("job", reason))
            .transpose()?,
        conclusion: found
            .try_get::<Option<String>>("", "conclusion")?
            .map(|conclusion| row::name("job", conclusion))
            .transpose()?,
        continue_on_error: found.try_get("", "continue_on_error")?,
        outputs: row::json("job", found.try_get::<Value>("", "outputs")?)?,
        started: found.try_get("", "started")?,
        completed_at: found.try_get("", "completed_at")?,
        execution,
    })
}

fn decode_execution(found: &QueryResult) -> Result<ExecutionState, Error> {
    Ok(ExecutionState {
        id: row::id("execution", found.try_get::<Uuid>("", "execution_id")?)?,
        agent_id: row::id("execution", found.try_get::<Uuid>("", "agent_id")?)?,
        agent_online: found.try_get("", "agent_online")?,
        status: row::name("execution", found.try_get("", "execution_status")?)?,
    })
}

pub struct NewRun<'a> {
    pub workflow: &'a Workflow,
    pub revision_id: RevisionId,
    pub ir: &'a ir::Workflow,
    pub payload: &'a Value,
    pub actor: &'a Actor,
}

pub async fn dispatch<C: ConnectionTrait>(db: &C, new: NewRun<'_>) -> Result<Run, Error> {
    let number = next_number(db, new.workflow).await?;
    let actor = serde_json::to_value(new.actor)?;
    let display_name = new
        .ir
        .name
        .clone()
        .unwrap_or_else(|| new.workflow.name.clone());
    let created = run::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        project_id: ActiveValue::Set(new.workflow.project_id.into()),
        workflow_id: ActiveValue::Set(new.workflow.id.into()),
        revision_id: ActiveValue::Set(new.revision_id.into()),
        ir: ActiveValue::Set(serde_json::to_value(new.ir)?),
        number: ActiveValue::Set(number),
        global_number: ActiveValue::NotSet,
        display_name: ActiveValue::Set(Some(display_name.clone())),
        event: ActiveValue::Set(DISPATCH.to_owned()),
        trigger: ActiveValue::Set(json!({ "actor": actor, "payload": new.payload })),
        git_ref: ActiveValue::Set(None),
        sha: ActiveValue::Set(None),
        inputs: ActiveValue::NotSet,
        status: ActiveValue::Set(row::text(RUNS, RunStatus::Queued)?),
        conclusion: ActiveValue::Set(None),
        created_at: ActiveValue::NotSet,
        started_at: ActiveValue::Set(None),
        completed_at: ActiveValue::Set(None),
        cancel_requested_at: ActiveValue::Set(None),
        trusted: ActiveValue::Set(true),
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
                group_id: None,
                matrix: None,
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
        display_name,
        event: created.event,
        status: RunStatus::Queued,
        conclusion: None,
        attempt: u32::from(FIRST_ATTEMPT),
        actor: new.actor.clone(),
        notices: Vec::new(),
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
