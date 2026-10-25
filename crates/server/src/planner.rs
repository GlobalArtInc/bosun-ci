mod concurrency;
mod matrix;

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};

use bosun_db::Error;
use bosun_db::control::{self, Kind};
use bosun_db::pools::Placement;
use bosun_db::queue::ClaimedJob;
use bosun_db::runs::{Evaluated, Grouped, JobState, Locked, NewRun, RunFacts, Strategy};
use bosun_expr::{Context, Scope, Status, Value};
use bosun_proto::assignment;
use bosun_types::actor::Actor;
use bosun_types::api::Run;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::ir::{self, Evaluable, JobTemplate, Segment, Shell, Span, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{
    Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason, WaitingReason,
};
use bosun_workflow::github::{self, Account, Facts, WorkflowFile};
use sea_orm::DatabaseTransaction;
use serde_json::{Map, json};

use crate::actions;
use crate::keys::Keyring;
use crate::secrets;
use crate::site::{ForgeSite, Site};
use concurrency::{Entered, Later, Owner, Reconciled};
use matrix::Expansion;

const LOG_RETENTION_DAYS: u32 = 90;

const INVALID_TYPE: &str = "invalid_type";

const INVALID_VALUE: &str = "invalid_value";

const START: Span = Span {
    line: 1,
    column: 1,
    end_line: 1,
    end_column: 1,
};

const SECRET_UNREADABLE: &str = "secret_unreadable";

const SINGLE: Strategy = Strategy {
    fail_fast: true,
    job_index: 0,
    job_total: 1,
    max_parallel: 1,
};

pub(crate) async fn dispatch(
    transaction: &DatabaseTransaction,
    new: NewRun<'_>,
    warnings: &[Diagnostic],
    site: &Site,
) -> Result<Run, Error> {
    let created = bosun_db::runs::create(transaction, &new).await?;
    let mut run = bosun_db::runs::lock(transaction, created.id)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {}", created.id)))?;
    let mut world = None;
    let mut grouped = None;
    if let Some(concurrency) = &new.ir.concurrency {
        let loaded = World::load(&run, site).await?;
        match concurrency::evaluate(concurrency, &Owner::Workflow, &loaded.run_contexts()?) {
            Ok(evaluated) => grouped = Some((concurrency, evaluated)),
            Err(fault) => {
                for warning in warnings {
                    run.notice(None, warning).await?;
                }
                run.notice(None, &loaded.diagnostic(fault)).await?;
                run.set_status(RunStatus::Completed, Some(RunConclusion::StartupFailure))
                    .await?;
                return view(transaction, created.id).await;
            }
        }
        world = Some(loaded);
    }
    run.create_jobs(new.ir, new.actor).await?;
    for warning in warnings {
        run.notice(None, warning).await?;
    }
    if let Some(template) = &new.ir.run_name {
        let loaded = match world.take() {
            Some(loaded) => loaded,
            None => World::load(&run, site).await?,
        };
        name(&run, &loaded, template).await?;
        world = Some(loaded);
    }
    let mut later = Later::default();
    if let Some((concurrency, grouped)) = &grouped
        && !run.state().awaiting_admission
    {
        enter_group(&mut run, concurrency, grouped, &mut later).await?;
    }
    planned(&mut run, site, world, &mut later).await?;
    follow(&mut run, site, later).await?;
    view(transaction, created.id).await
}

async fn enter_group(
    run: &mut Locked<'_>,
    concurrency: &ir::Concurrency,
    grouped: &Grouped,
    later: &mut Later,
) -> Result<(), Error> {
    let entered = concurrency::enter(run, None, grouped, later).await?;
    if let Some(fault) = concurrency::refusal(entered, concurrency, &Owner::Workflow, grouped) {
        let file = run.facts().await?.workflow;
        run.notice(None, &diagnostic(&file, fault)).await?;
        let jobs = run.jobs().await?;
        cancel_jobs(run, &jobs, StatusReason::Concurrency, &Actor::System, true).await?;
    }
    Ok(())
}

pub(crate) async fn admitted(
    run: &mut Locked<'_>,
    site: &Site,
    actor: &Actor,
) -> Result<(), Error> {
    let mut later = Later::default();
    let workflow = run.ir().await?;
    if let Some(concurrency) = &workflow.concurrency {
        let world = World::load(run, site).await?;
        match concurrency::evaluate(concurrency, &Owner::Workflow, &world.run_contexts()?) {
            Ok(grouped) => enter_group(run, concurrency, &grouped, &mut later).await?,
            Err(fault) => {
                run.notice(None, &world.diagnostic(fault)).await?;
                let jobs = run.jobs().await?;
                cancel_jobs(
                    run,
                    &jobs,
                    StatusReason::EvaluationFailed,
                    &Actor::System,
                    true,
                )
                .await?;
            }
        }
    }
    for job in run.jobs().await? {
        if job.status == JobStatus::Waiting && job.waiting_reason == Some(WaitingReason::Untrusted)
        {
            admit(
                run,
                &job,
                None,
                job.concurrency.as_ref(),
                false,
                actor,
                &mut later,
            )
            .await?;
        }
    }
    planned(run, site, None, &mut later).await?;
    follow(run, site, later).await
}

async fn view(transaction: &DatabaseTransaction, run: RunId) -> Result<Run, Error> {
    bosun_db::views::run(transaction, run)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {run}")))
}

pub(crate) async fn fail_startup(
    transaction: &DatabaseTransaction,
    new: NewRun<'_>,
    notices: &[Diagnostic],
) -> Result<Run, Error> {
    let created = bosun_db::runs::dispatch(transaction, new).await?;
    let mut run = bosun_db::runs::lock(transaction, created.id)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {}", created.id)))?;
    for notice in notices {
        run.notice(None, notice).await?;
    }
    run.set_status(RunStatus::Completed, Some(RunConclusion::StartupFailure))
        .await?;
    view(transaction, created.id).await
}

pub(crate) async fn plan(run: &mut Locked<'_>, site: &Site) -> Result<(), Error> {
    let mut later = Later::default();
    planned(run, site, None, &mut later).await?;
    follow(run, site, later).await
}

async fn follow(run: &mut Locked<'_>, site: &Site, mut later: Later) -> Result<(), Error> {
    while let Some(next) = later.next() {
        if next == run.state().id {
            run.refresh().await?;
            planned(run, site, None, &mut later).await?;
        } else {
            let mut other = bosun_db::runs::lock(run.transaction(), next)
                .await?
                .ok_or_else(|| Error::Vanished(format!("run {next}")))?;
            planned(&mut other, site, None, &mut later).await?;
        }
    }
    Ok(())
}

async fn planned(
    run: &mut Locked<'_>,
    site: &Site,
    mut world: Option<World>,
    later: &mut Later,
) -> Result<(), Error> {
    let mut jobs = run.jobs().await?;
    loop {
        if run.state().concurrency {
            match concurrency::reconcile(run, &jobs, later).await? {
                Reconciled::Unchanged => {}
                Reconciled::Changed => {
                    jobs = run.jobs().await?;
                    continue;
                }
                Reconciled::Admit(job) => {
                    if let Some(state) = jobs.iter().find(|state| state.id == job) {
                        let grouped = state.concurrency.clone();
                        admit(
                            run,
                            state,
                            None,
                            grouped.as_ref(),
                            false,
                            &Actor::System,
                            later,
                        )
                        .await?;
                    }
                    jobs = run.jobs().await?;
                    continue;
                }
            }
        }
        if advance(run, &jobs, later).await? {
            jobs = run.jobs().await?;
            continue;
        }
        let Some(index) = ready(&jobs) else {
            break;
        };
        let world = match &world {
            Some(world) => world,
            None => world.insert(World::load(run, site).await?),
        };
        match decide(run, world, &jobs, index, later).await? {
            Some(decided) if !run.state().concurrency => {
                let job = &mut jobs[index];
                job.status = decided.status;
                job.waiting_reason = decided.waiting_reason;
                job.conclusion = decided.conclusion;
                job.continue_on_error = decided.continue_on_error;
            }
            _ => jobs = run.jobs().await?,
        }
    }
    settle(run, &jobs).await?;
    if run.state().concurrency && run.state().status == RunStatus::Completed {
        concurrency::depart(run, later).await?;
    }
    Ok(())
}

pub(crate) async fn cancel_jobs(
    run: &Locked<'_>,
    jobs: &[JobState],
    reason: StatusReason,
    actor: &Actor,
    everything: bool,
) -> Result<(), Error> {
    let workflow = run.ir().await?;
    let expanded = expanded(jobs);
    for job in jobs {
        if job.status == JobStatus::Pending
            && (expanded.contains(&job.id)
                || (!everything && admits_cancellation(&workflow, &job.key)))
        {
            continue;
        }
        stop(run, job, reason, actor).await?;
    }
    Ok(())
}

fn expanded(jobs: &[JobState]) -> HashSet<JobId> {
    jobs.iter().filter_map(|job| job.group).collect()
}

pub(crate) async fn stop(
    run: &Locked<'_>,
    job: &JobState,
    reason: StatusReason,
    actor: &Actor,
) -> Result<bool, Error> {
    match job.status {
        JobStatus::Completed => Ok(false),
        JobStatus::Pending | JobStatus::Waiting | JobStatus::Queued => {
            run.complete_job(job.id, Conclusion::Cancelled, Some(reason), actor)
                .await?;
            Ok(true)
        }
        JobStatus::Assigned | JobStatus::InProgress | JobStatus::Paused | JobStatus::Held => {
            let execution = job.execution.as_ref().ok_or_else(|| Error::Row {
                table: "job",
                message: format!("job {} is on an agent without an execution", job.id),
            })?;
            let mut stopped = false;
            if job.status == JobStatus::Assigned {
                run.finish_execution(execution.id, Conclusion::Cancelled, Some(reason), actor)
                    .await?;
                stopped = true;
            }
            if !control::sent(run.transaction(), execution.id, Kind::Cancel).await? {
                control::publish(run.transaction(), execution.id, Kind::Cancel).await?;
                stopped = true;
            }
            Ok(stopped)
        }
    }
}

async fn advance(
    run: &mut Locked<'_>,
    jobs: &[JobState],
    later: &mut Later,
) -> Result<bool, Error> {
    let mut groups: BTreeMap<JobId, Vec<&JobState>> = BTreeMap::new();
    for job in jobs {
        if let Some(group) = job.group {
            groups.entry(group).or_default().push(job);
        }
    }
    for (group, mut members) in groups {
        if jobs
            .iter()
            .any(|job| job.id == group && job.status == JobStatus::Completed)
        {
            continue;
        }
        members.sort_by_key(|member| member.strategy.map(|strategy| strategy.job_index));
        if members
            .iter()
            .all(|member| member.status == JobStatus::Completed)
        {
            let (conclusion, outputs) = concluded(&members);
            run.complete_group(group, conclusion, &outputs).await?;
            return Ok(true);
        }
        let Some(strategy) = members.iter().find_map(|member| member.strategy) else {
            continue;
        };
        if strategy.fail_fast && members.iter().any(|member| failed(member)) {
            let mut stopped = false;
            for member in &members {
                stopped |= stop(run, member, StatusReason::FailFast, &Actor::System).await?;
            }
            if stopped {
                return Ok(true);
            }
            continue;
        }
        if run.state().awaiting_admission {
            continue;
        }
        let room = usize::try_from(strategy.max_parallel)
            .unwrap_or(usize::MAX)
            .saturating_sub(occupied(&members));
        let held: Vec<&JobState> = members
            .iter()
            .copied()
            .filter(|member| {
                member.status == JobStatus::Waiting
                    && member.waiting_reason == Some(WaitingReason::MaxParallel)
            })
            .take(room)
            .collect();
        for member in &held {
            admit(
                run,
                member,
                None,
                member.concurrency.as_ref(),
                false,
                &Actor::System,
                later,
            )
            .await?;
        }
        if !held.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn active(members: &[&JobState]) -> usize {
    members
        .iter()
        .filter(|member| {
            matches!(
                member.status,
                JobStatus::Queued
                    | JobStatus::Assigned
                    | JobStatus::InProgress
                    | JobStatus::Paused
                    | JobStatus::Held
            )
        })
        .count()
}

fn occupied(members: &[&JobState]) -> usize {
    active(members)
        + members
            .iter()
            .filter(|member| {
                member.status == JobStatus::Waiting
                    && matches!(
                        member.waiting_reason,
                        Some(WaitingReason::Untrusted | WaitingReason::Concurrency)
                    )
            })
            .count()
}

fn failed(job: &JobState) -> bool {
    matches!(
        job.conclusion,
        Some(Conclusion::Failure | Conclusion::TimedOut)
    ) && !job.continue_on_error
}

fn concluded(members: &[&JobState]) -> (Conclusion, BTreeMap<String, String>) {
    let conclusion = if members.iter().any(|member| failed(member)) {
        Conclusion::Failure
    } else if members
        .iter()
        .any(|member| member.conclusion == Some(Conclusion::Cancelled))
    {
        Conclusion::Cancelled
    } else {
        Conclusion::Success
    };
    let mut finished = members.to_vec();
    finished.sort_by_key(|member| {
        (
            member.completed_at,
            member.strategy.map(|strategy| strategy.job_index),
        )
    });
    let mut outputs = BTreeMap::new();
    for member in finished {
        for (name, value) in &member.outputs {
            if !value.is_empty() {
                outputs.insert(name.clone(), value.clone());
            }
        }
    }
    (conclusion, outputs)
}

fn admits_cancellation(workflow: &ir::Workflow, key: &JobKey) -> bool {
    workflow
        .jobs
        .iter()
        .find(|template| &template.key == key)
        .and_then(|template| template.r#if.as_ref())
        .is_some_and(bosun_expr::calls_status)
}

pub(crate) struct Assigned {
    pub(crate) contexts: assignment::Contexts,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) shell: Option<Shell>,
    pub(crate) working_directory: Option<String>,
}

pub(crate) enum Outcome {
    Assigned(Box<Assigned>),
    Failed(StatusReason),
}

pub(crate) async fn assign(
    run: &mut Locked<'_>,
    site: &Site,
    keys: &Keyring,
    claimed: &ClaimedJob,
    prepared: &actions::Prepared,
) -> Result<Outcome, Error> {
    let execution = claimed.execution_id;
    let job_id = claimed.job_id;
    let workflow_env = &claimed.env;
    let stored = &claimed.evaluated;
    let world = World::load(run, site).await?;
    let jobs = run.jobs().await?;
    let job = jobs
        .iter()
        .find(|job| job.id == job_id)
        .ok_or_else(|| Error::Row {
            table: "run",
            message: format!("the current attempt of the run has no job `{job_id}`"),
        })?;
    if let actions::Prepared::Failed(faults) = prepared {
        for fault in faults {
            run.notice(Some(job.id), &fault.diagnostic(&world.facts.workflow))
                .await?;
        }
        return fail_assigned(run, site, world, execution, StatusReason::ActionUnavailable).await;
    }
    let template = world.template(&job.key)?;
    let read = if run.state().trusted {
        secrets::read(
            run.transaction(),
            keys,
            world.facts.organisation_id,
            world.facts.project_id,
            &template.secrets,
        )
        .await?
    } else {
        Ok(BTreeMap::new())
    };
    let secrets = match read {
        Ok(secrets) => secrets,
        Err(unopened) => {
            let owner = if unopened.of_project {
                format!(
                    "project `{}/{}`",
                    world.facts.organisation, world.facts.project
                )
            } else {
                format!("organisation `{}`", world.facts.organisation)
            };
            let fault = Fault {
                code: SECRET_UNREADABLE,
                message: format!(
                    "job `{}` reads the secret `{}` of {owner}, which cannot be decrypted: {}",
                    job.key, unopened.name, unopened.reason
                ),
                span: template.span,
                path: at(&job.key, &[]),
            };
            return refuse(run, site, world, job.id, execution, fault).await;
        }
    };
    let secrets_context: Map<String, serde_json::Value> = secrets
        .into_iter()
        .map(|(name, value)| (name, serde_json::Value::String(value)))
        .collect();
    let secrets = value(serde_json::Value::Object(secrets_context.clone()))?;
    let github = world.github_context(job.key.as_str());
    let contexts = Contexts {
        github: value(serde_json::Value::Object(github.clone()))?,
        needs: Value::Null,
        inputs: world.inputs.clone(),
        bosun: world.bosun.clone(),
        env: Value::Null,
        strategy: Value::Null,
        matrix: Value::Null,
        vars: world.vars.clone(),
        secrets: secrets.clone(),
        secrets_read: Cell::new(false),
        statuses: None,
    };
    let mut env = BTreeMap::new();
    for (variable, template) in workflow_env {
        match bosun_expr::render(template, &contexts) {
            Ok(text) => {
                env.insert(variable.clone(), text);
            }
            Err(error) => {
                let what = format!("`env.{variable}` of the workflow");
                let fault = Fault::expression(error, &what, vec![key("env"), key(variable)]);
                return refuse(run, site, world, job.id, execution, fault).await;
            }
        }
    }
    let strategy = serde_json::to_value(job.strategy.unwrap_or(SINGLE))?;
    let matrix = match &job.matrix {
        Some(matrix) => serde_json::to_value(matrix)?,
        None => serde_json::Value::Null,
    };
    let needs = needs_context(&jobs, job);
    let job_values = if stored.deferred {
        let mut contexts = Contexts {
            needs: value(serde_json::Value::Object(needs.clone()))?,
            strategy: value(strategy.clone())?,
            matrix: match &job.matrix {
                Some(matrix) => matrix::context(matrix)?,
                None => Value::Null,
            },
            ..contexts
        };
        match environment(template, &world.ir.defaults, &mut contexts) {
            Ok(values) => values,
            Err(fault) => return refuse(run, site, world, job.id, execution, fault).await,
        }
    } else {
        Environment {
            env: stored.env.clone(),
            shell: stored.shell.clone(),
            working_directory: stored.working_directory.clone(),
        }
    };
    env.extend(job_values.env);
    if let actions::Prepared::Ready {
        actions,
        pins,
        notices,
    } = prepared
    {
        if let Some(pins) = pins {
            bosun_db::actions::pin(run.transaction(), job.id, pins).await?;
            for notice in notices {
                run.notice(Some(job.id), &notice.diagnostic(&world.facts.workflow))
                    .await?;
            }
        }
        let ir::JobKind::Steps(steps) = &claimed.job.kind;
        let stages = ir::stages(steps, |reference| {
            actions
                .iter()
                .find(|action| &action.uses == reference)
                .map(|action| match &action.metadata.runs {
                    ir::Runs::Node(runs) => runs,
                })
        });
        if stages.iter().any(|stage| stage.phase != ir::Phase::Main) {
            run.record_stages(execution, &stages).await?;
        }
    }
    Ok(Outcome::Assigned(Box::new(Assigned {
        contexts: assignment::Contexts {
            github,
            needs,
            strategy,
            matrix,
            inputs: serde_json::from_value(world.facts.inputs.clone())?,
            vars: world.vars_context.clone(),
            secrets: secrets_context,
            bosun: world.bosun_context.clone(),
        },
        env,
        shell: job_values.shell,
        working_directory: job_values.working_directory,
    })))
}

async fn fail_assigned(
    run: &mut Locked<'_>,
    site: &Site,
    world: World,
    execution: ExecutionId,
    reason: StatusReason,
) -> Result<Outcome, Error> {
    run.finish_execution(execution, Conclusion::Failure, Some(reason), &Actor::System)
        .await?;
    let mut later = Later::default();
    planned(run, site, Some(world), &mut later).await?;
    follow(run, site, later).await?;
    Ok(Outcome::Failed(reason))
}

async fn refuse(
    run: &mut Locked<'_>,
    site: &Site,
    world: World,
    job: JobId,
    execution: ExecutionId,
    fault: Fault,
) -> Result<Outcome, Error> {
    run.notice(Some(job), &world.diagnostic(fault)).await?;
    fail_assigned(run, site, world, execution, StatusReason::EvaluationFailed).await
}

struct World {
    ir: ir::Workflow,
    facts: RunFacts,
    forge: Option<ForgeSite>,
    attempt: u32,
    inputs: Value,
    bosun: Value,
    bosun_context: Map<String, serde_json::Value>,
    vars: Value,
    vars_context: Map<String, serde_json::Value>,
}

impl World {
    async fn load(run: &Locked<'_>, site: &Site) -> Result<World, Error> {
        let ir = run.ir().await?;
        let facts = run.facts().await?;
        let state = run.state();
        let bosun_context: Map<String, serde_json::Value> = [
            ("url", json!(site.url())),
            ("run_url", json!(site.run_url(state.id))),
            ("run_id", json!(state.id.to_string())),
            ("attempt", json!(state.attempt)),
            ("draft", json!(false)),
            (
                "project",
                json!(format!("{}/{}", facts.organisation, facts.project)),
            ),
            (
                "trigger",
                json!({ "kind": kind(&facts.actor), "login": login(&facts.actor) }),
            ),
        ]
        .into_iter()
        .map(|(property, value)| (property.to_owned(), value))
        .collect();
        let vars_context: Map<String, serde_json::Value> = bosun_db::variables::in_scope(
            run.transaction(),
            facts.organisation_id,
            facts.project_id,
        )
        .await?
        .into_iter()
        .map(|(name, value)| (name, serde_json::Value::String(value)))
        .collect();
        Ok(World {
            inputs: value(facts.inputs.clone())?,
            bosun: value(serde_json::Value::Object(bosun_context.clone()))?,
            bosun_context,
            vars: value(serde_json::Value::Object(vars_context.clone()))?,
            vars_context,
            attempt: state.attempt,
            forge: facts
                .repository
                .as_ref()
                .and_then(|_| site.forge().cloned()),
            ir,
            facts,
        })
    }

    fn github(&self, job: &str) -> Result<Value, Error> {
        value(serde_json::Value::Object(self.github_context(job)))
    }

    fn github_context(&self, job: &str) -> Map<String, serde_json::Value> {
        let facts = Facts {
            repository: self
                .facts
                .repository
                .as_ref()
                .map(|repository| github::Repository {
                    owner: repository.owner.clone(),
                    name: repository.name.clone(),
                    id: repository.id,
                    owner_id: repository.owner_id,
                }),
            forge: self.forge.as_ref().map(|forge| github::Forge {
                server_url: forge.server_url.clone(),
                api_url: forge.api_url.clone(),
                graphql_url: forge.graphql_url.clone(),
            }),
            git_ref: self.facts.git_ref.clone(),
            sha: self.facts.sha.clone(),
            ref_protected: false,
            workflow: WorkflowFile {
                name: self.ir.name.clone(),
                file: self.facts.workflow.clone(),
            },
            run: github::Run {
                id: self.facts.global_number,
                number: self.facts.number,
                attempt: self.attempt,
                retention_days: LOG_RETENTION_DAYS,
            },
            actor: Account {
                login: login(&self.facts.actor).unwrap_or_default().to_owned(),
                id: match &self.facts.actor {
                    Actor::Forge { id, .. } => Some(*id),
                    _ => None,
                },
            },
            triggering_actor: login(&self.facts.triggering_actor)
                .unwrap_or_default()
                .to_owned(),
            job: job.to_owned(),
            workspace: String::new(),
        };
        github::context(&self.facts.event, &self.facts.payload, &facts)
    }

    fn template(&self, key: &JobKey) -> Result<&JobTemplate, Error> {
        self.ir
            .jobs
            .iter()
            .find(|template| &template.key == key)
            .ok_or_else(|| Error::Row {
                table: "run",
                message: format!("the IR of the run has no job `{key}`"),
            })
    }

    fn diagnostic(&self, fault: Fault) -> Diagnostic {
        diagnostic(&self.facts.workflow, fault)
    }

    fn run_contexts(&self) -> Result<Contexts, Error> {
        Ok(Contexts {
            github: self.github("")?,
            needs: Value::Null,
            inputs: self.inputs.clone(),
            bosun: self.bosun.clone(),
            env: Value::Null,
            strategy: Value::Null,
            matrix: Value::Null,
            vars: self.vars.clone(),
            secrets: Value::Object(Vec::new().into()),
            secrets_read: Cell::new(false),
            statuses: None,
        })
    }
}

fn diagnostic(file: &str, fault: Fault) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: fault.code.to_owned(),
        message: fault.message,
        file: file.to_owned(),
        span: fault.span,
        path: fault.path,
    }
}

fn value(json: serde_json::Value) -> Result<Value, Error> {
    Ok(serde_json::from_value(json)?)
}

fn kind(actor: &Actor) -> &'static str {
    match actor {
        Actor::User { .. } => "user",
        Actor::Token { .. } => "token",
        Actor::Agent { .. } => "agent",
        Actor::Forge { .. } => "forge",
        Actor::System => "system",
    }
}

fn login(actor: &Actor) -> Option<&str> {
    match actor {
        Actor::User { login, .. } | Actor::Token { login, .. } | Actor::Forge { login, .. } => {
            Some(login)
        }
        Actor::Agent { name, .. } => Some(name),
        Actor::System => None,
    }
}

#[derive(Clone)]
struct Contexts {
    github: Value,
    needs: Value,
    inputs: Value,
    bosun: Value,
    env: Value,
    strategy: Value,
    matrix: Value,
    vars: Value,
    secrets: Value,
    secrets_read: Cell<bool>,
    statuses: Option<Statuses>,
}

#[derive(Clone, Copy)]
struct Statuses {
    succeeded: bool,
    failed: bool,
    cancelling: bool,
}

impl Scope for Contexts {
    fn context(&self, context: Context) -> Value {
        match context {
            Context::Github => self.github.clone(),
            Context::Needs => self.needs.clone(),
            Context::Inputs => self.inputs.clone(),
            Context::Bosun => self.bosun.clone(),
            Context::Env => self.env.clone(),
            Context::Strategy => self.strategy.clone(),
            Context::Matrix => self.matrix.clone(),
            Context::Vars => self.vars.clone(),
            Context::Secrets => {
                self.secrets_read.set(true);
                self.secrets.clone()
            }
            Context::Job | Context::Jobs | Context::Steps | Context::Runner => Value::Null,
        }
    }

    fn status(&self, status: Status) -> Option<bool> {
        let statuses = self.statuses?;
        Some(match status {
            Status::Success => !statuses.cancelling && statuses.succeeded,
            Status::Failure => !statuses.cancelling && statuses.failed,
            Status::Cancelled => statuses.cancelling,
        })
    }
}

async fn name(run: &Locked<'_>, world: &World, template: &Template) -> Result<(), Error> {
    let contexts = world.run_contexts()?;
    match bosun_expr::render(template, &contexts) {
        Ok(name) if !name.trim().is_empty() => run.set_display_name(&name).await,
        Ok(_) => Ok(()),
        Err(error) => {
            let fault = Fault::expression(error, "`run-name`", vec![key("run-name")]);
            run.notice(None, &world.diagnostic(fault)).await?;
            Ok(())
        }
    }
}

struct Decided {
    status: JobStatus,
    waiting_reason: Option<WaitingReason>,
    conclusion: Option<Conclusion>,
    continue_on_error: bool,
}

impl Decided {
    fn completed(conclusion: Conclusion) -> Decided {
        Decided {
            status: JobStatus::Completed,
            waiting_reason: None,
            conclusion: Some(conclusion),
            continue_on_error: false,
        }
    }
}

async fn decide(
    run: &mut Locked<'_>,
    world: &World,
    jobs: &[JobState],
    index: usize,
    later: &mut Later,
) -> Result<Option<Decided>, Error> {
    let job = &jobs[index];
    let template = world.template(&job.key)?;
    let mut contexts = Contexts {
        github: world.github(job.key.as_str())?,
        needs: value(serde_json::Value::Object(needs_context(jobs, job)))?,
        inputs: world.inputs.clone(),
        bosun: world.bosun.clone(),
        env: Value::Null,
        strategy: Value::Null,
        matrix: match &job.matrix {
            Some(matrix) => matrix::context(matrix)?,
            None => Value::Null,
        },
        vars: world.vars.clone(),
        secrets: Value::Object(Vec::new().into()),
        secrets_read: Cell::new(false),
        statuses: None,
    };
    let mut single = None;
    if job.group.is_none() {
        let (succeeded, failed) = ancestry(jobs, index);
        let cancelling = run.state().cancel_requested;
        contexts.statuses = Some(Statuses {
            succeeded,
            failed,
            cancelling,
        });
        let admitted = match &template.r#if {
            None => contexts.status(Status::Success).unwrap_or(false),
            Some(condition) => match bosun_expr::holds(condition, &contexts) {
                Ok(holds) => holds,
                Err(error) => {
                    let what = format!("`if` of job `{}`", job.key);
                    let fault = Fault::expression(error, &what, at(&job.key, &["if"]));
                    return fail(run, world, job, fault).await.map(Some);
                }
            },
        };
        if !admitted {
            let (conclusion, reason) = if cancelling {
                let reason = if run.cancelled_by_group().await? {
                    StatusReason::Concurrency
                } else {
                    StatusReason::Command
                };
                (Conclusion::Cancelled, reason)
            } else {
                (Conclusion::Skipped, StatusReason::IfFalse)
            };
            run.complete_job(job.id, conclusion, Some(reason), &Actor::System)
                .await?;
            return Ok(Some(Decided::completed(conclusion)));
        }
        contexts.statuses = None;
        if let Some(strategy) = &template.strategy {
            match matrix::expand(template, strategy, &contexts) {
                Ok(Expansion::Members(members)) => {
                    run.expand(job, &members).await?;
                    return Ok(None);
                }
                Ok(Expansion::Single(strategy)) => single = Some(strategy),
                Err(fault) => return fail(run, world, job, fault).await.map(Some),
            }
        }
    }
    let strategy = job.strategy.or(single).unwrap_or(SINGLE);
    contexts.strategy = serde_json::from_value(serde_json::to_value(strategy)?)?;
    let prepared = match prepare(template, &world.ir.defaults, &mut contexts, job) {
        Ok(prepared) => prepared,
        Err(fault) => return fail(run, world, job, fault).await.map(Some),
    };
    let placement = bosun_db::pools::place(
        run.transaction(),
        world.facts.organisation_id,
        world.facts.project_id,
        prepared.group.as_deref(),
        &prepared.labels,
    )
    .await?;
    run.prepare_job(
        job.id,
        &prepared.name,
        prepared.continue_on_error,
        &prepared.evaluated,
        single.as_ref(),
    )
    .await?;
    let full = job.group.is_some_and(|group| {
        let members: Vec<&JobState> = jobs
            .iter()
            .filter(|member| member.group == Some(group))
            .collect();
        occupied(&members) >= usize::try_from(strategy.max_parallel).unwrap_or(usize::MAX)
    });
    let grouped = prepared.evaluated.concurrency.as_ref();
    let decided = admit(
        run,
        job,
        Some(&placement),
        grouped,
        full,
        &Actor::System,
        later,
    )
    .await?;
    Ok(Some(Decided {
        continue_on_error: prepared.continue_on_error,
        ..decided
    }))
}

enum Admission {
    Queue,
    Hold(WaitingReason),
    Refuse(Entered, Grouped),
}

async fn admit(
    run: &mut Locked<'_>,
    job: &JobState,
    placement: Option<&Placement>,
    grouped: Option<&Grouped>,
    full: bool,
    actor: &Actor,
    later: &mut Later,
) -> Result<Decided, Error> {
    let admission = if full {
        Admission::Hold(WaitingReason::MaxParallel)
    } else if run.state().awaiting_admission {
        Admission::Hold(WaitingReason::Untrusted)
    } else if concurrency::held(run).await? {
        Admission::Hold(WaitingReason::Concurrency)
    } else if let Some(grouped) = grouped {
        match concurrency::enter(run, Some(job.id), grouped, later).await? {
            Entered::Running => Admission::Queue,
            Entered::Waiting => Admission::Hold(WaitingReason::Concurrency),
            refused => Admission::Refuse(refused, grouped.clone()),
        }
    } else {
        Admission::Queue
    };
    let placed = || {
        placement.ok_or_else(|| Error::Row {
            table: "job",
            message: format!("job {} leaves `pending` without its placement", job.id),
        })
    };
    match admission {
        Admission::Queue => {
            if job.status == JobStatus::Pending {
                run.queue_job(job.id, placed()?).await?;
            } else {
                run.release_job(job.id, actor).await?;
            }
            Ok(Decided {
                status: JobStatus::Queued,
                waiting_reason: None,
                conclusion: None,
                continue_on_error: job.continue_on_error,
            })
        }
        Admission::Hold(reason) => {
            if job.status == JobStatus::Pending {
                run.hold_job(job.id, placed()?, reason).await?;
            } else if job.waiting_reason != Some(reason) {
                run.rehold_job(job.id, reason, actor).await?;
            }
            Ok(Decided {
                status: JobStatus::Waiting,
                waiting_reason: Some(reason),
                conclusion: None,
                continue_on_error: job.continue_on_error,
            })
        }
        Admission::Refuse(entered, grouped) => {
            let workflow = run.ir().await?;
            let fault = workflow
                .jobs
                .iter()
                .find(|template| template.key == job.key)
                .and_then(|template| {
                    let concurrency = template.concurrency.as_ref()?;
                    concurrency::refusal(entered, concurrency, &Owner::Job(template), &grouped)
                });
            if let Some(fault) = fault {
                let file = run.facts().await?.workflow;
                run.notice(Some(job.id), &diagnostic(&file, fault)).await?;
            }
            run.complete_job(
                job.id,
                Conclusion::Cancelled,
                Some(StatusReason::Concurrency),
                &Actor::System,
            )
            .await?;
            Ok(Decided::completed(Conclusion::Cancelled))
        }
    }
}

async fn fail(
    run: &Locked<'_>,
    world: &World,
    job: &JobState,
    fault: Fault,
) -> Result<Decided, Error> {
    run.notice(Some(job.id), &world.diagnostic(fault)).await?;
    run.complete_job(
        job.id,
        Conclusion::Failure,
        Some(StatusReason::EvaluationFailed),
        &Actor::System,
    )
    .await?;
    Ok(Decided::completed(Conclusion::Failure))
}

fn ready(jobs: &[JobState]) -> Option<usize> {
    let completed: HashSet<&JobKey> = jobs
        .iter()
        .filter(|job| job.group.is_none() && job.status == JobStatus::Completed)
        .map(|job| &job.key)
        .collect();
    let expanded = expanded(jobs);
    jobs.iter().position(|job| {
        job.status == JobStatus::Pending
            && !expanded.contains(&job.id)
            && job.needs.iter().all(|need| completed.contains(need))
    })
}

fn templates(jobs: &[JobState]) -> HashMap<&JobKey, &JobState> {
    jobs.iter()
        .filter(|job| job.group.is_none())
        .map(|job| (&job.key, job))
        .collect()
}

fn effective(job: &JobState) -> Option<Conclusion> {
    match job.conclusion {
        Some(Conclusion::Failure | Conclusion::TimedOut) if job.continue_on_error => {
            Some(Conclusion::Success)
        }
        conclusion => conclusion,
    }
}

fn ancestry(jobs: &[JobState], index: usize) -> (bool, bool) {
    let by_key = templates(jobs);
    let mut seen = HashSet::new();
    let mut open: Vec<&JobKey> = jobs[index].needs.iter().collect();
    let (mut succeeded, mut failed) = (true, false);
    while let Some(key) = open.pop() {
        if !seen.insert(key) {
            continue;
        }
        let Some(need) = by_key.get(key) else {
            succeeded = false;
            continue;
        };
        match effective(need) {
            Some(Conclusion::Success) => {}
            Some(Conclusion::Failure | Conclusion::TimedOut) => {
                succeeded = false;
                failed = true;
            }
            _ => succeeded = false,
        }
        open.extend(need.needs.iter());
    }
    (succeeded, failed)
}

fn needs_context(jobs: &[JobState], job: &JobState) -> Map<String, serde_json::Value> {
    let by_key = templates(jobs);
    let mut needs = Map::new();
    for key in &job.needs {
        let Some(need) = by_key.get(key) else {
            continue;
        };
        let result = match effective(need) {
            Some(Conclusion::Success) => "success",
            Some(Conclusion::Failure | Conclusion::TimedOut) => "failure",
            Some(Conclusion::Cancelled) => "cancelled",
            Some(Conclusion::Skipped) => "skipped",
            None => "",
        };
        needs.insert(
            key.to_string(),
            json!({ "result": result, "outputs": need.outputs }),
        );
    }
    needs
}

struct Fault {
    code: &'static str,
    message: String,
    span: Span,
    path: Vec<PathSegment>,
}

impl Fault {
    fn expression(error: bosun_expr::Error, what: &str, path: Vec<PathSegment>) -> Fault {
        Fault {
            code: error.code.as_str(),
            message: format!("{what}: {}", error.message),
            span: error.span,
            path,
        }
    }
}

#[derive(Clone)]
struct Place {
    what: String,
    path: Vec<PathSegment>,
    span: Span,
}

impl Place {
    fn new(job: &JobTemplate, what: &str, path: &[&str], templates: &[&Template]) -> Place {
        Place {
            what: format!("`{what}` of job `{}`", job.key),
            path: at(&job.key, path),
            span: cover(templates).unwrap_or(job.span),
        }
    }

    fn of(job: &JobTemplate, what: &str, path: &[&str], template: &Template) -> Place {
        Place::new(job, what, path, &[template])
    }

    fn evaluation(&self, error: bosun_expr::Error) -> Fault {
        Fault::expression(error, &self.what, self.path.clone())
    }

    fn refuse(&self, code: &'static str, problem: String) -> Fault {
        Fault {
            code,
            message: format!("{} {problem}", self.what),
            span: self.span,
            path: self.path.clone(),
        }
    }

    fn typed(&self, template: &Template, scope: &dyn Scope) -> Result<Value, Fault> {
        match template.segments() {
            [Segment::Expression(expr)] => bosun_expr::evaluate(expr, scope),
            _ => bosun_expr::render(template, scope).map(Value::String),
        }
        .map_err(|error| self.evaluation(error))
    }

    fn text(&self, template: &Template, scope: &dyn Scope) -> Result<String, Fault> {
        bosun_expr::render(template, scope).map_err(|error| self.evaluation(error))
    }
}

struct Prepared {
    name: String,
    labels: Vec<String>,
    group: Option<String>,
    continue_on_error: bool,
    evaluated: Evaluated,
}

fn prepare(
    job: &JobTemplate,
    workflow: &ir::Defaults,
    contexts: &mut Contexts,
    state: &JobState,
) -> Result<Prepared, Fault> {
    let rendered = match &job.name {
        Some(name) if name.as_literal().is_none() => {
            Place::of(job, "name", &["name"], name).text(name, contexts)?
        }
        _ => String::new(),
    };
    let name = if !rendered.trim().is_empty() {
        rendered
    } else if !state.name.trim().is_empty() {
        state.name.clone()
    } else {
        job.key.to_string()
    };
    let (labels, group) = placement(job, contexts)?;
    let grouped = match &job.concurrency {
        Some(concurrency) => Some(concurrency::evaluate(
            concurrency,
            &Owner::Job(job),
            contexts,
        )?),
        None => None,
    };
    let timeout_minutes = match &job.timeout_minutes {
        Evaluable::Literal(minutes) => *minutes,
        Evaluable::Template(template) => {
            let place = Place::of(job, "timeout-minutes", &["timeout-minutes"], template);
            match place.typed(template, contexts)? {
                Value::Number(minutes)
                    if minutes.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&minutes) =>
                {
                    minutes as u32
                }
                Value::Number(minutes) => {
                    return Err(place.refuse(
                        INVALID_VALUE,
                        format!("must be a whole number of at least 0, and its expression gives {minutes}"),
                    ));
                }
                other => {
                    return Err(place.refuse(
                        INVALID_TYPE,
                        format!(
                            "must be a number, and its expression gives {}",
                            described(&other)
                        ),
                    ));
                }
            }
        }
    };
    let continue_on_error = match &job.continue_on_error {
        Evaluable::Literal(flag) => *flag,
        Evaluable::Template(template) => {
            let place = Place::of(job, "continue-on-error", &["continue-on-error"], template);
            match place.typed(template, contexts)? {
                Value::Boolean(flag) => flag,
                other => {
                    return Err(place.refuse(
                        INVALID_TYPE,
                        format!(
                            "must be `true` or `false`, and its expression gives {}",
                            described(&other)
                        ),
                    ));
                }
            }
        }
    };
    contexts.secrets_read.set(false);
    let environment = environment(job, workflow, contexts);
    let deferred = contexts.secrets_read.get();
    let environment = if deferred {
        Environment::default()
    } else {
        environment?
    };
    Ok(Prepared {
        name,
        labels,
        group,
        continue_on_error,
        evaluated: Evaluated {
            timeout_minutes,
            env: environment.env,
            shell: environment.shell,
            working_directory: environment.working_directory,
            deferred,
            concurrency: grouped,
        },
    })
}

#[derive(Default)]
struct Environment {
    env: BTreeMap<String, String>,
    shell: Option<Shell>,
    working_directory: Option<String>,
}

fn environment(
    job: &JobTemplate,
    workflow: &ir::Defaults,
    contexts: &mut Contexts,
) -> Result<Environment, Fault> {
    let mut env = BTreeMap::new();
    for (variable, value) in &job.env {
        let place = Place::of(job, &format!("env.{variable}"), &["env", variable], value);
        env.insert(variable.clone(), place.text(value, contexts)?);
    }
    contexts.env = Value::Object(
        env.iter()
            .map(|(variable, value)| (variable.clone(), Value::String(value.clone())))
            .collect(),
    );
    let shell = match &job.defaults.shell {
        None => match &workflow.shell {
            Some(Evaluable::Literal(shell)) => Some(shell.clone()),
            Some(Evaluable::Template(_)) | None => None,
        },
        Some(Evaluable::Literal(shell)) => Some(shell.clone()),
        Some(Evaluable::Template(template)) => {
            let place = Place::of(
                job,
                "defaults.run.shell",
                &["defaults", "run", "shell"],
                template,
            );
            let text = place.text(template, contexts)?;
            Some(
                text.parse::<Shell>()
                    .map_err(|error| place.refuse(INVALID_VALUE, format!("is {error}")))?,
            )
        }
    };
    let working_directory = match &job.defaults.working_directory {
        Some(template) => Some(
            Place::of(
                job,
                "defaults.run.working-directory",
                &["defaults", "run", "working-directory"],
                template,
            )
            .text(template, contexts)?,
        ),
        None => workflow
            .working_directory
            .as_ref()
            .and_then(Template::as_literal),
    };
    Ok(Environment {
        env,
        shell,
        working_directory,
    })
}

fn placement(
    job: &JobTemplate,
    contexts: &Contexts,
) -> Result<(Vec<String>, Option<String>), Fault> {
    let (labels, group, place) = match &job.runs_on {
        Evaluable::Literal(runs_on) => {
            let templates: Vec<&Template> = runs_on
                .labels
                .iter()
                .chain(runs_on.group.as_ref())
                .collect();
            let place = Place::new(job, "runs-on", &["runs-on"], &templates);
            let labels = runs_on
                .labels
                .iter()
                .map(|label| place.text(label, contexts))
                .collect::<Result<Vec<_>, _>>()?;
            let group = match &runs_on.group {
                Some(group) => Some(place.text(group, contexts)?),
                None => None,
            };
            (labels, group, place)
        }
        Evaluable::Template(template) => {
            let place = Place::of(job, "runs-on", &["runs-on"], template);
            let (labels, group) = match place.typed(template, contexts)? {
                Value::String(label) => (vec![label], None),
                Value::Array(items) => (strings(&place, &items)?, None),
                Value::Object(fields) => mapping(&place, &fields)?,
                other => {
                    return Err(place.refuse(
                        INVALID_TYPE,
                        format!(
                            "must be a label, a list of labels or a mapping of `group` and \
                             `labels`, and its expression gives {}",
                            described(&other)
                        ),
                    ));
                }
            };
            (labels, group, place)
        }
    };
    if labels.iter().any(|label| label.is_empty()) {
        return Err(place.refuse(INVALID_VALUE, "names an empty label".to_owned()));
    }
    if group.as_deref() == Some("") {
        return Err(place.refuse(INVALID_VALUE, "names an empty group".to_owned()));
    }
    if labels.is_empty() && group.is_none() {
        return Err(place.refuse(INVALID_VALUE, "names neither a group nor labels".to_owned()));
    }
    Ok((labels, group))
}

fn strings(place: &Place, items: &[Value]) -> Result<Vec<String>, Fault> {
    items
        .iter()
        .map(|item| match item {
            Value::String(label) => Ok(label.clone()),
            other => Err(place.refuse(
                INVALID_TYPE,
                format!(
                    "must list labels as strings, and its expression gives {}",
                    described(other)
                ),
            )),
        })
        .collect()
}

fn mapping(
    place: &Place,
    fields: &[(String, Value)],
) -> Result<(Vec<String>, Option<String>), Fault> {
    let mut labels = Vec::new();
    let mut group = None;
    for (field, value) in fields {
        match (field.as_str(), value) {
            ("labels", Value::String(label)) => labels = vec![label.clone()],
            ("labels", Value::Array(items)) => labels = strings(place, items)?,
            ("group", Value::String(name)) => group = Some(name.clone()),
            ("labels" | "group", other) => {
                return Err(place.refuse(
                    INVALID_TYPE,
                    format!(
                        "must give `{field}` as a string{}, and its expression gives {}",
                        if field == "labels" { " or a list" } else { "" },
                        described(other)
                    ),
                ));
            }
            _ => {
                return Err(place.refuse(
                    INVALID_VALUE,
                    format!("takes `group` and `labels`, and its expression gives `{field}`"),
                ));
            }
        }
    }
    Ok((labels, group))
}

fn described(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Boolean(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn cover(templates: &[&Template]) -> Option<Span> {
    let mut spans = templates
        .iter()
        .flat_map(|template| template.segments())
        .filter_map(|segment| match segment {
            Segment::Expression(expr) => Some(expr.span()),
            Segment::Text(_) => None,
        });
    let first = spans.next()?;
    let last = spans.next_back().unwrap_or(first);
    Some(Span {
        line: first.line,
        column: first.column,
        end_line: last.end_line,
        end_column: last.end_column,
    })
}

fn key(name: &str) -> PathSegment {
    PathSegment::Key(name.to_owned())
}

fn at(job: &JobKey, path: &[&str]) -> Vec<PathSegment> {
    [key("jobs"), key(job.as_str())]
        .into_iter()
        .chain(path.iter().map(|segment| key(segment)))
        .collect()
}

async fn settle(run: &mut Locked<'_>, jobs: &[JobState]) -> Result<(), Error> {
    let (to, conclusion) = if jobs.iter().all(|job| job.status == JobStatus::Completed) {
        (
            RunStatus::Completed,
            Some(conclude(run.state().cancel_requested, jobs)),
        )
    } else if waits(jobs) {
        (RunStatus::Waiting, None)
    } else if jobs.iter().any(|job| job.started) {
        (RunStatus::InProgress, None)
    } else {
        (RunStatus::Queued, None)
    };
    if (to, conclusion) != (run.state().status, run.state().conclusion) {
        run.set_status(to, conclusion).await?;
    }
    Ok(())
}

fn waits(jobs: &[JobState]) -> bool {
    jobs.iter().any(|job| job.status == JobStatus::Waiting)
        && !jobs.iter().any(|job| {
            matches!(
                job.status,
                JobStatus::Queued
                    | JobStatus::Assigned
                    | JobStatus::InProgress
                    | JobStatus::Paused
                    | JobStatus::Held
            )
        })
}

fn conclude(cancel_requested: bool, jobs: &[JobState]) -> RunConclusion {
    let any = |conclusion| jobs.iter().any(|job| job.conclusion == Some(conclusion));
    if jobs.iter().any(|job| {
        matches!(
            job.conclusion,
            Some(Conclusion::Failure | Conclusion::TimedOut)
        ) && !job.continue_on_error
    }) {
        RunConclusion::Failure
    } else if cancel_requested || any(Conclusion::Cancelled) {
        RunConclusion::Cancelled
    } else if jobs
        .iter()
        .all(|job| job.conclusion == Some(Conclusion::Skipped))
    {
        RunConclusion::Skipped
    } else {
        RunConclusion::Success
    }
}
