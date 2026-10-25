mod cancel;
mod journal;
mod logs;
mod openapi;
mod planning;
mod queue;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use bosun_db::agents::{NewAgent, NewRegistrationToken, Registration};
use bosun_db::entity::{attempt, control_message, execution, job, run, step};
use bosun_db::runs::NewRun;
use bosun_db::workflows::NewWorkflow;
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::api::Workflow;
use bosun_types::command::{Command, CommandRequest, Target};
use bosun_types::event::{Event, RunEvent};
use bosun_types::id::{AgentId, ExecutionId, JobId, RunId, StepId, TokenId, UserId};
use bosun_types::ir::{
    self, Action, Defaults, Evaluable, JobKind, JobTemplate, RunsOn, SecretReferences, Span, Step,
    Template, Trigger, WorkflowDispatch,
};
use bosun_types::key::JobKey;
use bosun_types::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, RunConclusion, RunStatus,
    StatusReason, StepStatus, WaitingReason,
};
use chrono::{TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, TransactionTrait,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::agents::progress::Reporter;
use crate::commands::{self, Applied};
use crate::planner;

struct World {
    database: TestDatabase,
    workflow: Workflow,
    agent: Reporter,
}

impl World {
    async fn new() -> World {
        let database = TestDatabase::migrated().await;
        let db = database.connection();
        let organisation = bosun_db::organisations::create(db, "globalart", "GlobalArt")
            .await
            .expect("an organisation");
        let project = bosun_db::projects::create(db, organisation.id, "keel", "Keel")
            .await
            .expect("a project");
        let workflow = bosun_db::workflows::create(
            db,
            NewWorkflow {
                project_id: project.id,
                name: "ci",
                source: "name: ci\n",
                sha256: vec![1; 32],
            },
        )
        .await
        .expect("a workflow");
        let pool = bosun_db::pools::default_pool(db)
            .await
            .expect("the default pool");
        let now = Utc::now();
        bosun_db::agents::create_registration_token(
            db,
            NewRegistrationToken {
                pool_id: pool.id,
                sha256: vec![2; 32],
                reusable: false,
                expires_at: now + TimeDelta::hours(1),
            },
        )
        .await
        .expect("a registration token");
        let registered = bosun_db::agents::register(
            db,
            &[2; 32],
            now,
            NewAgent {
                name: "runner1-1".to_owned(),
                labels: Vec::new(),
                credential_sha256: vec![3; 32],
            },
        )
        .await
        .expect("a registration");
        let Registration::Registered(agent) = registered else {
            panic!("the agent was not registered");
        };
        World {
            database,
            workflow,
            agent: Reporter {
                id: agent.id,
                name: agent.name,
            },
        }
    }

    fn db(&self) -> &DatabaseConnection {
        self.database.connection()
    }

    fn agent(&self) -> &Reporter {
        &self.agent
    }

    async fn dispatch(&self, workflow: &ir::Workflow) -> RunId {
        let transaction = self.db().begin().await.expect("a transaction");
        let run = planner::dispatch(
            &transaction,
            NewRun {
                workflow: &self.workflow,
                revision_id: self.workflow.revision_id.expect("a current revision"),
                ir: workflow,
                actor: &operator(),
            },
        )
        .await
        .expect("a dispatch");
        transaction.commit().await.expect("a commit");
        run.id
    }

    async fn assign(&self, run: RunId, job: JobId) -> ExecutionId {
        let transaction = self.db().begin().await.expect("a transaction");
        let locked = bosun_db::runs::lock(&transaction, run)
            .await
            .expect("a lock")
            .expect("the run");
        let execution = locked
            .assign_job(job, self.agent.id, Duration::ZERO)
            .await
            .expect("an assignment");
        transaction.commit().await.expect("a commit");
        execution
    }

    async fn command(&self, run: RunId, request: &CommandRequest) -> Applied {
        let transaction = self.db().begin().await.expect("a transaction");
        let mut locked = bosun_db::runs::lock(&transaction, run)
            .await
            .expect("a lock")
            .expect("the run");
        let applied = commands::apply(&mut locked, &operator(), request)
            .await
            .unwrap_or_else(|error| panic!("the command failed: {}", error.0.message));
        transaction.commit().await.expect("a commit");
        applied
    }

    async fn set_online(&self, online: bool) {
        self.db()
            .execute_unprepared(&format!(
                "UPDATE bosun.agent SET online = {online} WHERE id = '{}'",
                self.agent.id
            ))
            .await
            .expect("the agent's state");
    }

    async fn controls(&self) -> BTreeSet<ExecutionId> {
        control_message::Entity::find()
            .all(self.db())
            .await
            .expect("the control messages")
            .into_iter()
            .map(|control| id(control.execution_id))
            .collect()
    }

    async fn journal(&self, run: RunId) -> Vec<RunEvent> {
        bosun_db::journal::read(self.db(), run, 0)
            .await
            .expect("the journal")
    }

    async fn project(&self, run: RunId) -> Projection {
        let db = self.db();
        let stored = run::Entity::find_by_id(run.as_uuid())
            .one(db)
            .await
            .expect("a run")
            .expect("the run");
        let attempts: Vec<Uuid> = attempt::Entity::find()
            .filter(attempt::Column::RunId.eq(run.as_uuid()))
            .all(db)
            .await
            .expect("the attempts")
            .into_iter()
            .map(|attempt| attempt.id)
            .collect();
        let jobs = job::Entity::find()
            .filter(job::Column::AttemptId.is_in(attempts))
            .all(db)
            .await
            .expect("the jobs");
        let executions = execution::Entity::find()
            .filter(execution::Column::JobId.is_in(jobs.iter().map(|job| job.id)))
            .all(db)
            .await
            .expect("the executions");
        let steps = step::Entity::find()
            .filter(
                step::Column::ExecutionId.is_in(executions.iter().map(|execution| execution.id)),
            )
            .all(db)
            .await
            .expect("the steps");
        Projection {
            run: Some(RunRow {
                status: name(&stored.status),
                conclusion: stored.conclusion.as_deref().map(name),
                cancel_requested: stored.cancel_requested_at.is_some(),
                started: stored.started_at.is_some(),
                completed: stored.completed_at.is_some(),
            }),
            jobs: jobs
                .into_iter()
                .map(|job| {
                    (
                        id(job.id),
                        JobRow {
                            key: job.key,
                            status: name(&job.status),
                            waiting_reason: job.waiting_reason.as_deref().map(name),
                            conclusion: job.conclusion.as_deref().map(name),
                            source: job.conclusion_source.as_deref().map(name),
                            queued: job.queued_at.is_some(),
                            started: job.started_at.is_some(),
                            completed: job.completed_at.is_some(),
                        },
                    )
                })
                .collect(),
            executions: executions
                .into_iter()
                .map(|execution| {
                    (
                        id(execution.id),
                        ExecutionRow {
                            job: id(execution.job_id),
                            number: u32::try_from(execution.number).expect("a number"),
                            agent: id(execution.agent_id),
                            status: name(&execution.status),
                            conclusion: execution.conclusion.as_deref().map(name),
                            reason: execution.reason.as_deref().map(name),
                            started: execution.started_at.is_some(),
                            completed: execution.completed_at.is_some(),
                        },
                    )
                })
                .collect(),
            steps: steps
                .into_iter()
                .map(|step| {
                    (
                        id(step.id),
                        StepRow {
                            execution: id(step.execution_id),
                            index: u32::try_from(step.index).expect("an index"),
                            name: step.name,
                            status: name(&step.status),
                            outcome: step.outcome.as_deref().map(name),
                            conclusion: step.conclusion.as_deref().map(name),
                            source: step.conclusion_source.as_deref().map(name),
                            started: step.started_at.is_some(),
                            completed: step.completed_at.is_some(),
                        },
                    )
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Projection {
    run: Option<RunRow>,
    jobs: BTreeMap<JobId, JobRow>,
    executions: BTreeMap<ExecutionId, ExecutionRow>,
    steps: BTreeMap<StepId, StepRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunRow {
    status: RunStatus,
    conclusion: Option<RunConclusion>,
    cancel_requested: bool,
    started: bool,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JobRow {
    key: String,
    status: JobStatus,
    waiting_reason: Option<WaitingReason>,
    conclusion: Option<Conclusion>,
    source: Option<ConclusionSource>,
    queued: bool,
    started: bool,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionRow {
    job: JobId,
    number: u32,
    agent: AgentId,
    status: ExecutionStatus,
    conclusion: Option<Conclusion>,
    reason: Option<StatusReason>,
    started: bool,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StepRow {
    execution: ExecutionId,
    index: u32,
    name: String,
    status: StepStatus,
    outcome: Option<Conclusion>,
    conclusion: Option<Conclusion>,
    source: Option<ConclusionSource>,
    started: bool,
    completed: bool,
}

impl Projection {
    fn job(&self, key: &str) -> (JobId, &JobRow) {
        self.jobs
            .iter()
            .find(|(_, job)| job.key == key)
            .map(|(id, job)| (*id, job))
            .unwrap_or_else(|| panic!("no job `{key}` in {self:#?}"))
    }

    fn executions_of(&self, job: JobId) -> Vec<(ExecutionId, &ExecutionRow)> {
        self.executions
            .iter()
            .filter(|(_, execution)| execution.job == job)
            .map(|(id, execution)| (*id, execution))
            .collect()
    }

    fn steps_of(&self, execution: ExecutionId) -> Vec<&StepRow> {
        let mut steps: Vec<&StepRow> = self
            .steps
            .values()
            .filter(|step| step.execution == execution)
            .collect();
        steps.sort_by_key(|step| step.index);
        steps
    }
}

fn replay(events: &[RunEvent]) -> Result<Projection, String> {
    let mut projection = Projection::default();
    let mut last = 0;
    for event in events {
        if event.seq <= last {
            return Err(format!("event {} follows {last}", event.seq));
        }
        last = event.seq;
        apply(&mut projection, event)
            .map_err(|problem| format!("event {}: {problem}", event.seq))?;
    }
    Ok(projection)
}

fn apply(projection: &mut Projection, event: &RunEvent) -> Result<(), String> {
    match &event.event {
        Event::RunCreated(_) => {
            if projection.run.is_some() {
                return Err("a second run.created".to_owned());
            }
            projection.run = Some(RunRow {
                status: RunStatus::Queued,
                conclusion: None,
                cancel_requested: false,
                started: false,
                completed: false,
            });
        }
        Event::AttemptCreated(_) | Event::CommandRefused(_) => {}
        Event::JobCreated(created) => {
            let job = event.job_id.ok_or("job.created without a job")?;
            let fresh = JobRow {
                key: created.key.to_string(),
                status: JobStatus::Pending,
                waiting_reason: None,
                conclusion: None,
                source: None,
                queued: false,
                started: false,
                completed: false,
            };
            if projection.jobs.insert(job, fresh).is_some() {
                return Err(format!("job {job} was created twice"));
            }
        }
        Event::JobStatusChanged(changed) => {
            let job = event.job_id.ok_or("job.status without a job")?;
            let row = projection
                .jobs
                .get_mut(&job)
                .ok_or(format!("job.status of the unknown job {job}"))?;
            moved(
                "job",
                row.status,
                changed.from,
                changed.to,
                JobStatus::can_transition,
            )?;
            row.status = changed.to;
            row.waiting_reason = changed.waiting_reason;
            row.conclusion = changed.conclusion;
            row.source = changed.conclusion_source;
            match changed.to {
                JobStatus::Queued => row.queued = true,
                JobStatus::InProgress => row.started = true,
                JobStatus::Completed => row.completed = true,
                _ => {}
            }
        }
        Event::ExecutionCreated(created) => {
            let execution = event
                .execution_id
                .ok_or("execution.created without an execution")?;
            let fresh = ExecutionRow {
                job: event.job_id.ok_or("execution.created without a job")?,
                number: created.number,
                agent: created.agent_id,
                status: ExecutionStatus::Assigned,
                conclusion: None,
                reason: None,
                started: false,
                completed: false,
            };
            if projection.executions.insert(execution, fresh).is_some() {
                return Err(format!("execution {execution} was created twice"));
            }
        }
        Event::ExecutionStatusChanged(changed) => {
            let execution = event
                .execution_id
                .ok_or("execution.status without an execution")?;
            let row = projection.executions.get_mut(&execution).ok_or(format!(
                "execution.status of the unknown execution {execution}"
            ))?;
            moved(
                "execution",
                row.status,
                changed.from,
                changed.to,
                ExecutionStatus::can_transition,
            )?;
            row.status = changed.to;
            row.conclusion = changed.conclusion;
            row.reason = changed.reason;
            match changed.to {
                ExecutionStatus::InProgress => row.started = true,
                ExecutionStatus::Completed => row.completed = true,
                _ => {}
            }
        }
        Event::StepCreated(created) => {
            let step = event.step_id.ok_or("step.created without a step")?;
            let fresh = StepRow {
                execution: event
                    .execution_id
                    .ok_or("step.created without an execution")?,
                index: created.index,
                name: created.name.clone(),
                status: StepStatus::Pending,
                outcome: None,
                conclusion: None,
                source: None,
                started: false,
                completed: false,
            };
            if projection.steps.insert(step, fresh).is_some() {
                return Err(format!("step {step} was created twice"));
            }
        }
        Event::StepStatusChanged(changed) => {
            let step = event.step_id.ok_or("step.status without a step")?;
            let row = projection
                .steps
                .get_mut(&step)
                .ok_or(format!("step.status of the unknown step {step}"))?;
            moved(
                "step",
                row.status,
                changed.from,
                changed.to,
                StepStatus::can_transition,
            )?;
            row.status = changed.to;
            row.outcome = changed.outcome;
            row.conclusion = changed.conclusion;
            row.source = changed.conclusion_source;
            match changed.to {
                StepStatus::InProgress => row.started = true,
                StepStatus::Completed => row.completed = true,
                _ => {}
            }
        }
        Event::RunStatusChanged(changed) => {
            let row = projection
                .run
                .as_mut()
                .ok_or("run.status before run.created")?;
            moved(
                "run",
                row.status,
                changed.from,
                changed.to,
                RunStatus::can_transition,
            )?;
            row.status = changed.to;
            row.conclusion = changed.conclusion;
            match changed.to {
                RunStatus::InProgress => row.started = true,
                RunStatus::Completed => row.completed = true,
                _ => {}
            }
        }
        Event::CommandAccepted(accepted) => {
            if accepted.command.command == Command::RunCancel {
                let row = projection
                    .run
                    .as_mut()
                    .ok_or("a command before run.created")?;
                row.cancel_requested = true;
            }
        }
    }
    Ok(())
}

fn moved<S: Copy + Eq + std::fmt::Debug>(
    entity: &str,
    current: S,
    from: S,
    to: S,
    allowed: fn(S, S) -> bool,
) -> Result<(), String> {
    if current != from {
        return Err(format!(
            "the {entity} is {current:?}, and the event starts from {from:?}"
        ));
    }
    if !allowed(from, to) {
        return Err(format!("the {entity} cannot go from {from:?} to {to:?}"));
    }
    Ok(())
}

fn operator() -> Actor {
    Actor::Token {
        id: TokenId::try_from(Uuid::now_v7()).expect("a version 7 id"),
        user_id: UserId::try_from(Uuid::now_v7()).expect("a version 7 id"),
        login: "roman".to_owned(),
    }
}

fn run_cancel() -> CommandRequest {
    CommandRequest {
        command: Command::RunCancel,
        target: Target::default(),
        reason: None,
    }
}

fn job_cancel(job: JobId) -> CommandRequest {
    CommandRequest {
        command: Command::JobCancel,
        target: Target {
            attempt: None,
            job: Some(job),
            step: None,
        },
        reason: None,
    }
}

fn key(text: &str) -> JobKey {
    text.parse().expect("a job key")
}

fn template(name: &str, needs: &[&str], steps: usize) -> JobTemplate {
    let span = Span {
        line: 1,
        column: 1,
        end_line: 1,
        end_column: 1,
    };
    JobTemplate {
        key: key(name),
        name: None,
        needs: needs.iter().map(|need| key(need)).collect(),
        r#if: None,
        runs_on: Evaluable::Literal(RunsOn {
            group: None,
            labels: vec![Template::text("Self-Hosted"), Template::text("Linux")],
        }),
        permissions: None,
        concurrency: None,
        strategy: None,
        env: BTreeMap::new(),
        defaults: Defaults::default(),
        timeout_minutes: Evaluable::Literal(360),
        continue_on_error: Evaluable::Literal(false),
        outputs: BTreeMap::new(),
        secrets: SecretReferences::Named(BTreeSet::new()),
        kind: JobKind::Steps(
            (0..steps)
                .map(|index| Step {
                    key: None,
                    name: None,
                    r#if: None,
                    action: Action::Run {
                        script: Template::text(format!("echo {name} {index}\n")),
                    },
                    shell: None,
                    working_directory: None,
                    with: BTreeMap::new(),
                    env: BTreeMap::new(),
                    continue_on_error: Evaluable::Literal(false),
                    timeout_minutes: None,
                    span,
                })
                .collect(),
        ),
        span,
    }
}

fn workflow(jobs: Vec<JobTemplate>) -> ir::Workflow {
    ir::Workflow {
        name: Some("ci".to_owned()),
        run_name: None,
        triggers: vec![Trigger::WorkflowDispatch(WorkflowDispatch::default())],
        permissions: None,
        env: BTreeMap::new(),
        defaults: Defaults::default(),
        concurrency: None,
        jobs,
    }
}

fn name<T: DeserializeOwned>(text: &str) -> T {
    serde_json::from_value(Value::String(text.to_owned()))
        .unwrap_or_else(|error| panic!("`{text}`: {error}"))
}

fn id<T: TryFrom<Uuid>>(value: Uuid) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(value).expect("a version 7 id")
}
