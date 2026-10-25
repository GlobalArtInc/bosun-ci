mod matrix;

use std::collections::{BTreeMap, HashMap, HashSet};

use bosun_db::Error;
use bosun_db::control::{self, Kind};
use bosun_db::runs::{Evaluated, JobState, Locked, NewRun, RunFacts, Strategy};
use bosun_expr::{Context, Scope, Status, Value};
use bosun_proto::assignment;
use bosun_types::actor::Actor;
use bosun_types::api::Run;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::id::{ExecutionId, JobId};
use bosun_types::ir::{self, Evaluable, JobTemplate, Segment, Shell, Span, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{
    Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason, WaitingReason,
};
use bosun_workflow::github::{self, Account, Facts, WorkflowFile};
use sea_orm::DatabaseTransaction;
use serde_json::{Map, json};

use crate::site::Site;
use matrix::Expansion;

const LOG_RETENTION_DAYS: u32 = 90;

const INVALID_TYPE: &str = "invalid_type";

const INVALID_VALUE: &str = "invalid_value";

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
    let run_name = new.ir.run_name.clone();
    let created = bosun_db::runs::dispatch(transaction, new).await?;
    let mut run = bosun_db::runs::lock(transaction, created.id)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {}", created.id)))?;
    for warning in warnings {
        run.notice(None, warning).await?;
    }
    let world = match &run_name {
        Some(template) => {
            let world = World::load(&run, site).await?;
            name(&run, &world, template).await?;
            Some(world)
        }
        None => None,
    };
    planned(&mut run, site, world).await?;
    bosun_db::views::run(transaction, created.id)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {}", created.id)))
}

pub(crate) async fn plan(run: &mut Locked<'_>, site: &Site) -> Result<(), Error> {
    planned(run, site, None).await
}

async fn planned(run: &mut Locked<'_>, site: &Site, mut world: Option<World>) -> Result<(), Error> {
    let mut jobs = run.jobs().await?;
    loop {
        if advance(run, &jobs).await? {
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
        match decide(run, world, &jobs, index).await? {
            Some(decided) => {
                let job = &mut jobs[index];
                job.status = decided.status;
                job.waiting_reason = decided.waiting_reason;
                job.conclusion = decided.conclusion;
                job.continue_on_error = decided.continue_on_error;
            }
            None => jobs = run.jobs().await?,
        }
    }
    settle(run, &jobs).await
}

pub(crate) fn expanded(jobs: &[JobState]) -> HashSet<JobId> {
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

async fn advance(run: &Locked<'_>, jobs: &[JobState]) -> Result<bool, Error> {
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
        let room = usize::try_from(strategy.max_parallel)
            .unwrap_or(usize::MAX)
            .saturating_sub(active(&members));
        let held: Vec<JobId> = members
            .iter()
            .filter(|member| {
                member.status == JobStatus::Waiting
                    && member.waiting_reason == Some(WaitingReason::MaxParallel)
            })
            .take(room)
            .map(|member| member.id)
            .collect();
        for member in &held {
            run.release_job(*member).await?;
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

pub(crate) fn admits_cancellation(workflow: &ir::Workflow, key: &JobKey) -> bool {
    workflow
        .jobs
        .iter()
        .find(|template| &template.key == key)
        .and_then(|template| template.r#if.as_ref())
        .is_some_and(bosun_expr::calls_status)
}

pub(crate) fn unapplied(workflow: &ir::Workflow) -> Option<String> {
    let (what, step) = match &workflow.concurrency {
        Some(_) => ("`concurrency` of the workflow".to_owned(), 17),
        None => workflow.jobs.iter().find_map(|job| {
            [
                (job.concurrency.is_some(), "`concurrency`", 17),
                (uses_an_action(job), "a `uses` step", 33),
            ]
            .into_iter()
            .find(|(used, ..)| *used)
            .map(|(_, key, step)| (format!("{key} of job `{}`", job.key), step))
        })?,
    };
    Some(format!(
        "{what}, which the planner applies from M1 step {step}"
    ))
}

fn uses_an_action(job: &ir::JobTemplate) -> bool {
    let ir::JobKind::Steps(steps) = &job.kind;
    steps
        .iter()
        .any(|step| matches!(step.action, ir::Action::Uses(_)))
}

pub(crate) struct Assigned {
    pub(crate) contexts: assignment::Contexts,
    pub(crate) env: BTreeMap<String, String>,
}

pub(crate) async fn assign(
    run: &mut Locked<'_>,
    site: &Site,
    execution: ExecutionId,
    job_id: JobId,
    workflow_env: &BTreeMap<String, Template>,
) -> Result<Option<Assigned>, Error> {
    let world = World::load(run, site).await?;
    let jobs = run.jobs().await?;
    let job = jobs
        .iter()
        .find(|job| job.id == job_id)
        .ok_or_else(|| Error::Row {
            table: "run",
            message: format!("the current attempt of the run has no job `{job_id}`"),
        })?;
    let github = world.github_context(job.key.as_str());
    let contexts = Contexts {
        github: value(serde_json::Value::Object(github.clone()))?,
        needs: Value::Null,
        inputs: world.inputs.clone(),
        bosun: world.bosun.clone(),
        env: Value::Null,
        strategy: Value::Null,
        matrix: Value::Null,
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
                run.notice(Some(job.id), &world.diagnostic(fault)).await?;
                run.finish_execution(
                    execution,
                    Conclusion::Failure,
                    Some(StatusReason::EvaluationFailed),
                    &Actor::System,
                )
                .await?;
                planned(run, site, Some(world)).await?;
                return Ok(None);
            }
        }
    }
    Ok(Some(Assigned {
        contexts: assignment::Contexts {
            github,
            needs: needs_context(&jobs, job),
            strategy: serde_json::to_value(job.strategy.unwrap_or(SINGLE))?,
            matrix: match &job.matrix {
                Some(matrix) => serde_json::to_value(matrix)?,
                None => serde_json::Value::Null,
            },
            inputs: serde_json::from_value(world.facts.inputs.clone())?,
            vars: Map::new(),
            secrets: Map::new(),
            bosun: world.bosun_context.clone(),
        },
        env,
    }))
}

struct World {
    ir: ir::Workflow,
    facts: RunFacts,
    attempt: u32,
    inputs: Value,
    bosun: Value,
    bosun_context: Map<String, serde_json::Value>,
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
        Ok(World {
            inputs: value(facts.inputs.clone())?,
            bosun: value(serde_json::Value::Object(bosun_context.clone()))?,
            bosun_context,
            attempt: state.attempt,
            ir,
            facts,
        })
    }

    fn github(&self, job: &str) -> Result<Value, Error> {
        value(serde_json::Value::Object(self.github_context(job)))
    }

    fn github_context(&self, job: &str) -> Map<String, serde_json::Value> {
        let facts = Facts {
            repository: None,
            forge: None,
            git_ref: None,
            sha: None,
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
                id: None,
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
        Diagnostic {
            severity: Severity::Error,
            code: fault.code.to_owned(),
            message: fault.message,
            file: self.facts.workflow.clone(),
            span: fault.span,
            path: fault.path,
        }
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
        Actor::System => "system",
    }
}

fn login(actor: &Actor) -> Option<&str> {
    match actor {
        Actor::User { login, .. } | Actor::Token { login, .. } => Some(login),
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
            Context::Vars | Context::Secrets => Value::Object(Vec::new().into()),
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
    let contexts = Contexts {
        github: world.github("")?,
        needs: Value::Null,
        inputs: world.inputs.clone(),
        bosun: world.bosun.clone(),
        env: Value::Null,
        strategy: Value::Null,
        matrix: Value::Null,
        statuses: None,
    };
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
    run: &Locked<'_>,
    world: &World,
    jobs: &[JobState],
    index: usize,
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
                (Conclusion::Cancelled, StatusReason::Command)
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
    let pools = bosun_db::pools::eligible(run.transaction(), prepared.group.as_deref()).await?;
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
        active(&members) >= usize::try_from(strategy.max_parallel).unwrap_or(usize::MAX)
    });
    let waiting_reason = if full {
        run.hold_job(job.id, &prepared.labels, &pools, WaitingReason::MaxParallel)
            .await?;
        Some(WaitingReason::MaxParallel)
    } else {
        run.queue_job(job.id, &prepared.labels, &pools).await?;
        None
    };
    Ok(Some(Decided {
        status: if full {
            JobStatus::Waiting
        } else {
            JobStatus::Queued
        },
        waiting_reason,
        conclusion: None,
        continue_on_error: prepared.continue_on_error,
    }))
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
    Ok(Prepared {
        name,
        labels,
        group,
        continue_on_error,
        evaluated: Evaluated {
            timeout_minutes,
            env,
            shell,
            working_directory,
        },
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
