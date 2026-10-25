use std::collections::{BTreeMap, HashMap, HashSet};

use bosun_db::Error;
use bosun_db::runs::{Evaluated, JobState, Locked, NewRun, RunFacts};
use bosun_expr::{Context, Scope, Status, Value};
use bosun_types::actor::Actor;
use bosun_types::api::Run;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::ir::{self, Evaluable, JobTemplate, Segment, Shell, Span, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason};
use bosun_workflow::github::{self, Account, Facts, WorkflowFile};
use sea_orm::DatabaseTransaction;
use serde_json::json;

use crate::site::Site;

const LOG_RETENTION_DAYS: u32 = 90;

const INVALID_TYPE: &str = "invalid_type";

const INVALID_VALUE: &str = "invalid_value";

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
    while let Some(index) = ready(&jobs) {
        let world = match &world {
            Some(world) => world,
            None => world.insert(World::load(run, site).await?),
        };
        let decided = decide(run, world, &jobs, index).await?;
        let job = &mut jobs[index];
        job.status = decided.status;
        job.conclusion = decided.conclusion;
        job.continue_on_error = decided.continue_on_error;
    }
    settle(run, &jobs).await
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
                (job.strategy.is_some(), "`strategy`", 16),
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

struct World {
    ir: ir::Workflow,
    facts: RunFacts,
    attempt: u32,
    inputs: Value,
    bosun: Value,
}

impl World {
    async fn load(run: &Locked<'_>, site: &Site) -> Result<World, Error> {
        let ir = run.ir().await?;
        let facts = run.facts().await?;
        let state = run.state();
        let bosun = json!({
            "url": site.url(),
            "run_url": site.run_url(state.id),
            "run_id": state.id.to_string(),
            "attempt": state.attempt,
            "draft": false,
            "project": format!("{}/{}", facts.organisation, facts.project),
            "trigger": { "kind": kind(&facts.actor), "login": login(&facts.actor) },
        });
        Ok(World {
            inputs: serde_json::from_value(facts.inputs.clone())?,
            bosun: serde_json::from_value(bosun)?,
            attempt: state.attempt,
            ir,
            facts,
        })
    }

    fn github(&self, job: &str) -> Result<Value, Error> {
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
        let context = github::context(&self.facts.event, &self.facts.payload, &facts);
        Ok(serde_json::from_value(serde_json::Value::Object(context))?)
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
            Context::Vars | Context::Secrets => Value::Object(Vec::new().into()),
            Context::Job
            | Context::Jobs
            | Context::Steps
            | Context::Runner
            | Context::Strategy
            | Context::Matrix => Value::Null,
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
    conclusion: Option<Conclusion>,
    continue_on_error: bool,
}

async fn decide(
    run: &Locked<'_>,
    world: &World,
    jobs: &[JobState],
    index: usize,
) -> Result<Decided, Error> {
    let job = &jobs[index];
    let template = world.template(&job.key)?;
    let (succeeded, failed) = ancestry(jobs, index);
    let cancelling = run.state().cancel_requested;
    let mut contexts = Contexts {
        github: world.github(job.key.as_str())?,
        needs: needs(jobs, job)?,
        inputs: world.inputs.clone(),
        bosun: world.bosun.clone(),
        env: Value::Null,
        statuses: Some(Statuses {
            succeeded,
            failed,
            cancelling,
        }),
    };
    let admitted = match &template.r#if {
        None => contexts.status(Status::Success).unwrap_or(false),
        Some(condition) => match bosun_expr::holds(condition, &contexts) {
            Ok(holds) => holds,
            Err(error) => {
                let what = format!("`if` of job `{}`", job.key);
                let fault = Fault::expression(error, &what, at(&job.key, &["if"]));
                return fail(run, world, job, fault).await;
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
        return Ok(Decided {
            status: JobStatus::Completed,
            conclusion: Some(conclusion),
            continue_on_error: false,
        });
    }
    contexts.statuses = None;
    let prepared = match prepare(template, &mut contexts) {
        Ok(prepared) => prepared,
        Err(fault) => return fail(run, world, job, fault).await,
    };
    let pools = bosun_db::pools::eligible(run.transaction(), prepared.group.as_deref()).await?;
    run.prepare_job(
        job.id,
        &prepared.name,
        prepared.continue_on_error,
        &prepared.evaluated,
    )
    .await?;
    run.queue_job(job.id, &prepared.labels, &pools).await?;
    Ok(Decided {
        status: JobStatus::Queued,
        conclusion: None,
        continue_on_error: prepared.continue_on_error,
    })
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
    Ok(Decided {
        status: JobStatus::Completed,
        conclusion: Some(Conclusion::Failure),
        continue_on_error: false,
    })
}

fn ready(jobs: &[JobState]) -> Option<usize> {
    let completed: HashSet<&JobKey> = jobs
        .iter()
        .filter(|job| job.status == JobStatus::Completed)
        .map(|job| &job.key)
        .collect();
    jobs.iter().position(|job| {
        job.status == JobStatus::Pending && job.needs.iter().all(|need| completed.contains(need))
    })
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
    let by_key: HashMap<&JobKey, &JobState> = jobs.iter().map(|job| (&job.key, job)).collect();
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

fn needs(jobs: &[JobState], job: &JobState) -> Result<Value, Error> {
    let by_key: HashMap<&JobKey, &JobState> = jobs.iter().map(|job| (&job.key, job)).collect();
    let mut needs = serde_json::Map::new();
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
    Ok(serde_json::from_value(serde_json::Value::Object(needs))?)
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

fn prepare(job: &JobTemplate, contexts: &mut Contexts) -> Result<Prepared, Fault> {
    let name = match &job.name {
        Some(name) => Place::of(job, "name", &["name"], name).text(name, contexts)?,
        None => String::new(),
    };
    let name = if name.trim().is_empty() {
        job.key.to_string()
    } else {
        name
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
        None => None,
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
        None => None,
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
