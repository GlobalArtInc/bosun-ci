use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, SystemTime};

use bosun_expr::Scope;
use bosun_proto::assignment::{Contexts, Evaluated};
use bosun_types::ir::{
    self, Action, ActionMetadata, ActionReference, Condition, JobKind, JobTemplate, NodeRuns,
    Phase, Runs, Shell, Step, Template,
};
use bosun_types::status::Conclusion;
use bosun_workflow::github::{self, BosunFacts, RunnerFacts, StepFacts};
use rustix::process::{Pid, Signal};
use serde_json::{Map, Value};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until, timeout};
use uuid::Uuid;

use crate::annotation::{Annotation, AnnotationKind};
use crate::commands::Commands;
use crate::mask::{Masker, Sink};
use crate::matchers::{Place, Registry};
use crate::output::Output;
use crate::scope::{self, Fixed, Record, Standing, StepScope};
use crate::shell::Invocation;
use crate::tracking::{self, Processes, Tracking};
use crate::workflow_commands::{Processor, Reports, Settings};

const INTERRUPT_GRACE: Duration = Duration::from_millis(7500);

const TERMINATE_GRACE: Duration = Duration::from_millis(2500);

const OUTPUT_AFTER_TEARDOWN: Duration = Duration::from_secs(2);

const SCRIPT: &str = "__run";

const STEP_DEBUG: &str = "ACTIONS_STEP_DEBUG";

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub workspace: PathBuf,
    pub temp: PathBuf,
    pub tool_cache: PathBuf,
    pub runner: Runner,
    pub bosun: BosunFacts,
    pub proxies: BTreeMap<String, String>,
    pub template: JobTemplate,
    pub contexts: Contexts,
    pub evaluated: Evaluated,
    pub actions: Vec<NodeAction>,
    pub fetched: Result<(), String>,
    pub minute: Duration,
}

#[derive(Debug, Clone)]
pub struct NodeAction {
    pub uses: ActionReference,
    pub directory: PathBuf,
    pub node: PathBuf,
    pub metadata: ActionMetadata,
}

impl NodeAction {
    fn runs(&self) -> &NodeRuns {
        let Runs::Node(runs) = &self.metadata.runs;
        runs
    }
}

#[derive(Debug)]
pub struct Runner {
    pub name: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    pub conclusion: Conclusion,
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    StepStarted {
        index: u32,
        at: SystemTime,
    },
    Line {
        index: u32,
        at: SystemTime,
        text: Vec<u8>,
    },
    StepFinished {
        index: u32,
        at: SystemTime,
        outcome: Conclusion,
        conclusion: Conclusion,
        outputs: BTreeMap<String, String>,
        annotations: Vec<Annotation>,
        summary: Option<String>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct Results {
    pub(crate) outputs: BTreeMap<String, String>,
    pub(crate) annotations: Vec<Annotation>,
    pub(crate) summary: Option<String>,
}

pub async fn run(
    job: Job,
    tracking: &Tracking,
    events: UnboundedSender<Event>,
    cancel: watch::Receiver<bool>,
) -> Completed {
    let started = Instant::now();
    let Job {
        id,
        workspace,
        temp,
        tool_cache,
        runner,
        bosun,
        proxies,
        template,
        contexts,
        evaluated,
        actions,
        fetched,
        minute,
    } = job;
    let events = Sink::new(
        events,
        Masker::new(contexts.secrets.values().filter_map(Value::as_str)),
    );
    let JobKind::Steps(steps) = template.kind;
    let prepared = fetched
        .map_err(io::Error::other)
        .and_then(|()| prepare(&workspace, &temp, &tool_cache, &contexts.github))
        .and_then(|directories| {
            tracking
                .start(&id)
                .map(|processes| (directories, processes))
        });
    let (directories, processes) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            if !steps.is_empty() {
                events.started(0);
                events.line(
                    0,
                    &format!("the job could not be prepared on this agent: {error}"),
                );
                events.finished(
                    0,
                    Conclusion::Failure,
                    Conclusion::Failure,
                    Results::default(),
                );
            }
            return Completed {
                conclusion: Conclusion::Failure,
                outputs: BTreeMap::new(),
            };
        }
    };
    let Directories {
        workspace,
        temp,
        tool_cache,
    } = directories;
    let mut github = contexts.github.clone();
    github.insert("workspace".to_owned(), Value::String(text(&workspace)));
    let runner = RunnerFacts {
        name: runner.name,
        os: runner.os,
        arch: runner.arch,
        temp: text(&temp),
        tool_cache: text(&tool_cache),
        workspace: workspace.parent().map(text).unwrap_or_default(),
    };
    let fixed = Fixed::new(&contexts, &runner);
    let step_debug = [&contexts.vars, &contexts.secrets]
        .into_iter()
        .any(|context| {
            context
                .get(STEP_DEBUG)
                .and_then(Value::as_str)
                .is_some_and(enabled)
        });
    let Evaluated {
        env,
        shell,
        working_directory,
        timeout_minutes,
        ..
    } = evaluated;
    let mut execution = Execution {
        workspace,
        temp,
        base: std::env::vars_os().collect(),
        github,
        runner,
        bosun,
        proxies,
        env,
        shell,
        working_directory: working_directory.filter(|directory| !directory.is_empty()),
        fixed,
        actions,
        minute,
        deadline: limit(minute, timeout_minutes).and_then(|limit| started.checked_add(limit)),
        standing: Standing::default(),
        timed_out: false,
        files: BTreeMap::new(),
        paths: Vec::new(),
        records: Vec::new(),
        last: None,
        matchers: Registry::default(),
        step_debug,
        processes,
        outputs: Vec::new(),
        events,
        cancel,
    };
    execution.steps(&steps).await;
    let outputs = execution.job_outputs(&template.outputs);
    let conclusion = execution.conclusion();
    execution.teardown().await;
    Completed {
        conclusion,
        outputs,
    }
}

fn enabled(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn limit(minute: Duration, minutes: u32) -> Option<Duration> {
    (minutes > 0).then(|| minute.checked_mul(minutes)).flatten()
}

struct Directories {
    workspace: PathBuf,
    temp: PathBuf,
    tool_cache: PathBuf,
}

fn prepare(
    workspace: &Path,
    temp: &Path,
    tool_cache: &Path,
    github: &Map<String, Value>,
) -> io::Result<Directories> {
    for directory in [workspace, temp, tool_cache] {
        fs::create_dir_all(directory)?;
    }
    let directories = Directories {
        workspace: fs::canonicalize(workspace)?,
        temp: fs::canonicalize(temp)?,
        tool_cache: fs::canonicalize(tool_cache)?,
    };
    let events = directories.temp.join("_github_workflow");
    fs::create_dir_all(&events)?;
    let payload = github.get("event").unwrap_or(&Value::Null);
    fs::write(
        events.join("event.json"),
        serde_json::to_vec_pretty(payload)?,
    )?;
    Ok(directories)
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn names(steps: &[Step]) -> Vec<String> {
    let mut taken = HashSet::new();
    steps
        .iter()
        .map(|step| {
            let base = match (&step.key, &step.action) {
                (Some(key), _) => key.as_str().to_owned(),
                (None, Action::Uses(reference)) => format!(
                    "__{}_{}",
                    plain(&reference.owner),
                    plain(&reference.repository)
                ),
                (None, Action::Run { .. }) => SCRIPT.to_owned(),
            };
            let mut name = base.clone();
            let mut number = 2;
            while !taken.insert(name.clone()) {
                name = format!("{base}_{number}");
                number += 1;
            }
            name
        })
        .collect()
}

fn inputs(
    phase: Phase,
    action: &NodeAction,
    step: &Step,
    scope: &dyn Scope,
    reports: &Reports,
) -> Result<BTreeMap<String, String>, String> {
    let mut inputs = BTreeMap::new();
    for (name, template) in &step.with {
        let value = scope::text(&format!("`with.{name}` of the step"), template, scope)?;
        inputs.insert(variable(name), value);
    }
    let main = phase == Phase::Main;
    for input in &action.metadata.inputs {
        let supplied = step
            .with
            .keys()
            .any(|name| name.eq_ignore_ascii_case(&input.name));
        if supplied {
            if main && let Some(message) = &input.deprecation_message {
                reports.note(
                    AnnotationKind::Warning,
                    format!(
                        "the input `{}` of the action `{}` is deprecated: {message}",
                        input.name, action.uses
                    ),
                );
            }
            continue;
        }
        let value = match &input.default {
            Some(default) => scope::text(
                &format!(
                    "the default of the input `{}` of the action `{}`",
                    input.name, action.uses
                ),
                default,
                scope,
            )?,
            None => {
                if main && input.required {
                    reports.note(
                        AnnotationKind::Warning,
                        format!(
                            "the action `{}` requires the input `{}`, which the step does not \
                             supply",
                            action.uses, input.name
                        ),
                    );
                }
                String::new()
            }
        };
        inputs.insert(variable(&input.name), value);
    }
    Ok(inputs)
}

fn plain(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn variable(input: &str) -> String {
    format!("INPUT_{}", input.replace(' ', "_").to_uppercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    Step,
    Job,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Exited,
    Cancelled,
    TimedOut(Limit),
}

struct Execution {
    workspace: PathBuf,
    temp: PathBuf,
    base: BTreeMap<OsString, OsString>,
    github: Map<String, Value>,
    runner: RunnerFacts,
    bosun: BosunFacts,
    proxies: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    shell: Option<Shell>,
    working_directory: Option<String>,
    fixed: Fixed,
    actions: Vec<NodeAction>,
    minute: Duration,
    deadline: Option<Instant>,
    standing: Standing,
    timed_out: bool,
    files: BTreeMap<String, String>,
    paths: Vec<String>,
    records: Vec<Record>,
    last: Option<u32>,
    matchers: Registry,
    step_debug: bool,
    processes: Processes,
    outputs: Vec<Output>,
    events: Sink,
    cancel: watch::Receiver<bool>,
}

struct Ran {
    outcome: Conclusion,
    timed_out: Option<Limit>,
    results: Results,
}

impl Ran {
    fn failed() -> Ran {
        Ran {
            outcome: Conclusion::Failure,
            timed_out: None,
            results: Results::default(),
        }
    }
}

impl Execution {
    async fn steps(&mut self, steps: &[Step]) {
        let names = names(steps);
        let stages = ir::stages(steps, |reference| {
            self.actions
                .iter()
                .find(|action| &action.uses == reference)
                .map(NodeAction::runs)
        });
        let mut reached = vec![false; steps.len()];
        let mut states = vec![BTreeMap::new(); steps.len()];
        for (index, stage) in (0..).zip(stages) {
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.timed_out = true;
            }
            if self.timed_out {
                return;
            }
            let position = stage.step as usize;
            let (Some(step), Some(name), Some(state)) = (
                steps.get(position),
                names.get(position),
                states.get_mut(position),
            ) else {
                continue;
            };
            if stage.phase == Phase::Post && !reached[position] {
                self.events.finished(
                    index,
                    Conclusion::Skipped,
                    Conclusion::Skipped,
                    Results::default(),
                );
                continue;
            }
            let at = At {
                index,
                phase: stage.phase,
                action: name.clone(),
            };
            if self.step(at, step, state).await {
                reached[position] = true;
            }
        }
    }

    fn action(&self, step: &Step) -> Option<NodeAction> {
        let Action::Uses(reference) = &step.action else {
            return None;
        };
        self.actions
            .iter()
            .find(|action| &action.uses == reference)
            .cloned()
    }

    fn env_context(&self) -> BTreeMap<String, String> {
        let mut env = self.files.clone();
        env.extend(self.env.clone());
        env
    }

    fn scope(&self, github: &Map<String, Value>, env: &BTreeMap<String, String>) -> StepScope {
        StepScope::new(
            &self.fixed,
            scope::value(Value::Object(github.clone())),
            scope::strings(env),
            &self.records,
            self.standing,
            self.workspace.clone(),
        )
    }

    fn condition(&self, at: &At, step: &Step) -> (String, Option<Condition>) {
        let action = match (at.phase, self.action(step)) {
            (Phase::Pre | Phase::Post, Some(action)) => action,
            _ => return ("`if` of the step".to_owned(), step.r#if.clone()),
        };
        let runs = action.runs();
        let (key, condition) = match at.phase {
            Phase::Pre => ("pre-if", runs.pre_if.clone()),
            _ => ("post-if", runs.post_if.clone()),
        };
        (
            format!("`{key}` of the action `{}`", action.uses),
            condition,
        )
    }

    async fn step(&mut self, at: At, step: &Step, state: &mut BTreeMap<String, String>) -> bool {
        let index = at.index;
        if *self.cancel.borrow() {
            self.standing.cancelled = true;
        }
        let facts = StepFacts {
            action: at.action.clone(),
            id: Uuid::now_v7().to_string(),
        };
        let mut github = github::step_context(&self.github, &self.runner, &facts);
        if let (Action::Uses(reference), Some(action)) = (&step.action, self.action(step)) {
            for (property, value) in [
                ("action_path", text(&action.directory)),
                (
                    "action_repository",
                    format!("{}/{}", reference.owner, reference.repository),
                ),
                ("action_ref", reference.r#ref.clone()),
            ] {
                github.insert(property.to_owned(), Value::String(value));
            }
        }
        let env = self.env_context();
        let mut scope = self.scope(&github, &env);
        let (what, condition) = self.condition(&at, step);
        let holds = match &condition {
            None if at.phase == Phase::Main => Ok(self.standing.succeeding()),
            None => Ok(true),
            Some(condition) => {
                bosun_expr::holds(condition, &scope).map_err(|error| scope::fault(&what, &error))
            }
        };
        match holds {
            Ok(false) => {
                self.record(
                    &at,
                    step,
                    Conclusion::Skipped,
                    Conclusion::Skipped,
                    &BTreeMap::new(),
                );
                self.events.finished(
                    index,
                    Conclusion::Skipped,
                    Conclusion::Skipped,
                    Results::default(),
                );
                return false;
            }
            Ok(true) => self.start(index),
            Err(fault) => {
                self.start(index);
                self.events.line(index, &fault);
                self.standing.failed = true;
                self.record(
                    &at,
                    step,
                    Conclusion::Failure,
                    Conclusion::Failure,
                    &BTreeMap::new(),
                );
                self.events.finished(
                    index,
                    Conclusion::Failure,
                    Conclusion::Failure,
                    Results::default(),
                );
                return true;
            }
        }
        let ran = self
            .execute(&at, step, &github, env, &mut scope, state)
            .await;
        self.conclude(&at, step, ran, &scope);
        true
    }

    fn start(&mut self, index: u32) {
        self.last = Some(index);
        self.events.started(index);
    }

    fn conclude(&mut self, at: &At, step: &Step, ran: Ran, scope: &StepScope) {
        let Ran {
            outcome,
            timed_out,
            results,
        } = ran;
        let mut conclusion = outcome;
        if matches!(outcome, Conclusion::Failure | Conclusion::TimedOut)
            && timed_out != Some(Limit::Job)
        {
            match scope::flag(
                "`continue-on-error` of the step",
                &step.continue_on_error,
                scope,
            ) {
                Ok(true) => conclusion = Conclusion::Success,
                Ok(false) => {}
                Err(fault) => self.events.line(at.index, &fault),
            }
        }
        match conclusion {
            Conclusion::Failure | Conclusion::TimedOut => self.standing.failed = true,
            Conclusion::Cancelled => self.standing.cancelled = true,
            Conclusion::Success | Conclusion::Skipped => {}
        }
        if timed_out == Some(Limit::Job) {
            self.timed_out = true;
        }
        self.record(at, step, outcome, conclusion, &results.outputs);
        self.events.finished(at.index, outcome, conclusion, results);
    }

    fn record(
        &mut self,
        at: &At,
        step: &Step,
        outcome: Conclusion,
        conclusion: Conclusion,
        outputs: &BTreeMap<String, String>,
    ) {
        if at.phase != Phase::Main {
            return;
        }
        if let Some(key) = &step.key {
            self.records.push(Record {
                key: key.to_string(),
                outputs: outputs.clone(),
                outcome,
                conclusion,
            });
        }
    }

    async fn execute(
        &mut self,
        at: &At,
        step: &Step,
        github: &Map<String, Value>,
        mut env: BTreeMap<String, String>,
        scope: &mut StepScope,
        state: &mut BTreeMap<String, String>,
    ) -> Ran {
        let index = at.index;
        let cancellable = !self.standing.cancelled;
        let failed = |fault: String| {
            self.events.line(index, &fault);
            Ran::failed()
        };
        let commands = match Commands::create(github) {
            Ok(commands) => commands,
            Err(error) => {
                return failed(format!(
                    "the files of the step's file commands could not be created: {error}"
                ));
            }
        };
        let minutes = match &step.timeout_minutes {
            Some(minutes) => {
                match scope::minutes("`timeout-minutes` of the step", minutes, &*scope) {
                    Ok(minutes) => limit(self.minute, minutes),
                    Err(fault) => return failed(fault),
                }
            }
            None => None,
        };
        let mut variables = BTreeMap::new();
        for (name, template) in &step.env {
            match scope::text(&format!("`env.{name}` of the step"), template, &*scope) {
                Ok(value) => {
                    variables.insert(name.clone(), value);
                }
                Err(fault) => return failed(fault),
            }
        }
        env.extend(variables.clone());
        scope.env = scope::strings(&env);
        let reports = Reports::new(index, self.events.clone());
        let mut process_env = self.process_env(github, &variables);
        let (program, directory) = match &step.action {
            Action::Run { script } => {
                let directory = match &step.working_directory {
                    Some(template) => {
                        match scope::text("`working-directory` of the step", template, &*scope) {
                            Ok(directory) => {
                                Some(directory).filter(|directory| !directory.is_empty())
                            }
                            Err(fault) => return failed(fault),
                        }
                    }
                    None => None,
                };
                let script = match scope::text("`run` of the step", script, &*scope) {
                    Ok(script) => script,
                    Err(fault) => return failed(fault),
                };
                let directory = match directory.or_else(|| self.working_directory.clone()) {
                    Some(directory) => self.workspace.join(directory),
                    None => self.workspace.clone(),
                };
                let shell = step.shell.clone().or_else(|| self.shell.clone());
                (Program::Script { script, shell }, directory)
            }
            Action::Uses(reference) => {
                let Some(action) = self.action(step) else {
                    return failed(format!(
                        "the step uses the action `{reference}`, which the job did not fetch"
                    ));
                };
                let inputs = match inputs(at.phase, &action, step, &*scope, &reports) {
                    Ok(inputs) => inputs,
                    Err(fault) => return failed(fault),
                };
                for (name, value) in inputs.into_iter().chain(
                    state
                        .iter()
                        .map(|(name, value)| (format!("STATE_{name}"), value.clone())),
                ) {
                    process_env.insert(name.into(), value.into());
                }
                let runs = action.runs();
                let entry = match at.phase {
                    Phase::Pre => runs.pre.as_deref(),
                    Phase::Main => Some(runs.main.as_str()),
                    Phase::Post => runs.post.as_deref(),
                }
                .unwrap_or(runs.main.as_str());
                (
                    Program::Node {
                        node: action.node.clone(),
                        entry: action.directory.join(entry),
                    },
                    self.workspace.clone(),
                )
            }
        };
        let (outcome, timed_out) = self
            .process(
                index,
                Process {
                    program,
                    directory: &directory,
                    env: process_env,
                    minutes,
                    cancellable,
                    reports: reports.clone(),
                },
            )
            .await;
        let mut written = commands.read();
        let summary = match commands.summary(&self.events.masker()) {
            Ok(summary) => summary,
            Err(fault) => {
                written.faults.push(fault);
                None
            }
        };
        for fault in &written.faults {
            self.events.line(index, fault);
        }
        if summary.as_ref().is_some_and(|summary| summary.cut) {
            reports.note(
                AnnotationKind::Notice,
                "the step summary is longer than 1 MiB, so only its first 1 MiB is kept".to_owned(),
            );
        }
        let effects = reports.take();
        for path in effects.paths.into_iter().chain(written.paths) {
            self.paths.retain(|known| *known != path);
            self.paths.push(path);
        }
        self.files
            .extend(effects.env.into_iter().chain(written.env));
        if at.phase != Phase::Post {
            state.extend(effects.state.into_iter().chain(written.state));
        }
        let failed = !written.faults.is_empty() || effects.failed;
        let outcome = if outcome == Conclusion::Success && failed {
            Conclusion::Failure
        } else {
            outcome
        };
        Ran {
            outcome,
            timed_out,
            results: Results {
                outputs: effects.outputs.into_iter().chain(written.outputs).collect(),
                annotations: effects.annotations.into_vec(),
                summary: summary.map(|summary| summary.text),
            },
        }
    }

    fn settings(&self, env: &BTreeMap<OsString, OsString>) -> Settings {
        let variable = |name: &str| {
            env.get(OsStr::new(name))
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let property = |name: &str| {
            self.github
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Settings {
            debug: self.step_debug
                || enabled(&variable(STEP_DEBUG))
                || variable("RUNNER_DEBUG") == "1",
            unsecure_commands: enabled(&variable("ACTIONS_ALLOW_UNSECURE_COMMANDS")),
            unsecure_stop_tokens: enabled(&variable("ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS")),
            place: Place {
                workspace: self.workspace.clone(),
                repository: property("repository"),
                server_url: property("server_url"),
            },
        }
    }

    fn process_env(
        &self,
        github: &Map<String, Value>,
        variables: &BTreeMap<String, String>,
    ) -> BTreeMap<OsString, OsString> {
        let mut env = self.base.clone();
        for (name, value) in github::environment(github, &self.runner, &self.bosun)
            .iter()
            .chain(&self.proxies)
            .chain(&self.files)
            .chain(&self.env)
            .chain(variables)
        {
            env.insert(name.into(), value.into());
        }
        if !self.paths.is_empty() {
            let mut path = OsString::new();
            for (position, entry) in self.paths.iter().rev().enumerate() {
                if position > 0 {
                    path.push(":");
                }
                path.push(entry);
            }
            if let Some(current) = env
                .get(OsStr::new("PATH"))
                .filter(|current| !current.is_empty())
            {
                path.push(":");
                path.push(current);
            }
            env.insert("PATH".into(), path);
        }
        env
    }

    async fn process(&mut self, index: u32, process: Process<'_>) -> (Conclusion, Option<Limit>) {
        let Process {
            program,
            directory,
            env,
            minutes,
            cancellable,
            reports,
        } = process;
        if !directory.is_dir() {
            self.events.line(
                index,
                &format!(
                    "the working directory {} does not exist",
                    directory.display()
                ),
            );
            return (Conclusion::Failure, None);
        }
        let (command, arguments) = match program {
            Program::Script { script, shell } => {
                let invocation = match Invocation::resolve(
                    shell.as_ref(),
                    env.get(OsStr::new("PATH")).map(OsString::as_os_str),
                ) {
                    Ok(invocation) => invocation,
                    Err(reason) => {
                        self.events.line(index, &reason);
                        return (Conclusion::Failure, None);
                    }
                };
                let file = self
                    .temp
                    .join(format!("step-{index}{}", invocation.extension()));
                if let Err(error) = fs::write(&file, script) {
                    self.events.line(
                        index,
                        &format!(
                            "the script could not be written to {}: {error}",
                            file.display()
                        ),
                    );
                    return (Conclusion::Failure, None);
                }
                (
                    OsString::from(invocation.program(&file)),
                    invocation
                        .arguments(&file)
                        .into_iter()
                        .map(OsString::from)
                        .collect(),
                )
            }
            Program::Node { node, entry } => (node.into_os_string(), vec![entry.into_os_string()]),
        };
        let deadline = minutes
            .and_then(|minutes| Instant::now().checked_add(minutes))
            .map(|step| match self.deadline {
                Some(job) if job <= step => (job, Limit::Job),
                _ => (step, Limit::Step),
            })
            .or(self.deadline.map(|job| (job, Limit::Job)));
        let processor = Processor::new(reports, self.settings(&env), self.matchers.clone());
        let launch = Launch {
            program: &command,
            arguments,
            directory,
            env,
            processor,
        };
        let (child, mut output) = match self.spawn(index, launch) {
            Ok(started) => started,
            Err(error) => {
                self.events.line(
                    index,
                    &format!(
                        "the step could not start `{}`: {error}",
                        command.to_string_lossy()
                    ),
                );
                return (Conclusion::Failure, None);
            }
        };
        let (status, ended) = self.wait(child, cancellable, deadline).await;
        output.flush().await;
        self.outputs.push(output);
        match ended {
            Ended::TimedOut(limit) => {
                self.events.line(
                    index,
                    match limit {
                        Limit::Step => "the step ran beyond its `timeout-minutes`",
                        Limit::Job => "the job ran beyond its `timeout-minutes`",
                    },
                );
                (Conclusion::TimedOut, Some(limit))
            }
            Ended::Cancelled => (Conclusion::Cancelled, None),
            Ended::Exited => match status {
                Ok(status) if status.success() => (Conclusion::Success, None),
                _ => (Conclusion::Failure, None),
            },
        }
    }

    fn spawn(&mut self, index: u32, launch: Launch<'_>) -> io::Result<((Child, Pid), Output)> {
        let Launch {
            program,
            arguments,
            directory,
            env,
            processor,
        } = launch;
        let (reader, writer) = io::pipe()?;
        let errors = writer.try_clone()?;
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(directory)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(errors);
        self.processes.prepare(&mut command);
        let child = command.spawn();
        drop(command);
        let child = child?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("the step has no process id"))?;
        self.processes.started(group);
        let output = Output::start(reader, index, self.events.clone(), processor)?;
        Ok(((child, group), output))
    }

    async fn wait(
        &mut self,
        (mut child, group): (Child, Pid),
        cancellable: bool,
        deadline: Option<(Instant, Limit)>,
    ) -> (io::Result<ExitStatus>, Ended) {
        let ended = tokio::select! {
            status = child.wait() => return (status, Ended::Exited),
            () = cancelled(&mut self.cancel), if cancellable => Ended::Cancelled,
            () = sleep_until(deadline.map_or_else(Instant::now, |(at, _)| at)), if deadline.is_some() => {
                Ended::TimedOut(deadline.map_or(Limit::Job, |(_, limit)| limit))
            }
        };
        tracking::signal(group, Signal::INT);
        if let Ok(status) = timeout(INTERRUPT_GRACE, child.wait()).await {
            return (status, ended);
        }
        tracking::signal(group, Signal::TERM);
        if let Ok(status) = timeout(TERMINATE_GRACE, child.wait()).await {
            return (status, ended);
        }
        tracking::signal(group, Signal::KILL);
        (child.wait().await, ended)
    }

    fn job_outputs(&mut self, templates: &BTreeMap<String, Template>) -> BTreeMap<String, String> {
        let mut outputs = BTreeMap::new();
        if templates.is_empty() {
            return outputs;
        }
        let env = self.env_context();
        let scope = self.scope(&self.github, &env);
        for (name, template) in templates {
            match scope::text(&format!("`outputs.{name}` of the job"), template, &scope) {
                Ok(value) if value.is_empty() => {}
                Ok(value) if self.events.masker().masks(&value) => self.events.line(
                    self.last.unwrap_or(0),
                    &format!(
                        "the output `{name}` of the job is not reported, since it contains a \
                         masked value"
                    ),
                ),
                Ok(value) => {
                    outputs.insert(name.clone(), value);
                }
                Err(fault) => {
                    self.events.line(self.last.unwrap_or(0), &fault);
                    self.standing.failed = true;
                    return BTreeMap::new();
                }
            }
        }
        outputs
    }

    fn conclusion(&self) -> Conclusion {
        if self.timed_out {
            Conclusion::TimedOut
        } else if self.standing.cancelled {
            Conclusion::Cancelled
        } else if self.standing.failed {
            Conclusion::Failure
        } else {
            Conclusion::Success
        }
    }

    async fn teardown(mut self) {
        self.processes.kill().await;
        Output::finish(std::mem::take(&mut self.outputs), OUTPUT_AFTER_TEARDOWN).await;
        if let Ok(entries) = fs::read_dir(&self.temp) {
            for entry in entries.flatten() {
                let path = entry.path();
                let _ = if path.is_dir() && !path.is_symlink() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
    }
}

enum Program {
    Script {
        script: String,
        shell: Option<Shell>,
    },
    Node {
        node: PathBuf,
        entry: PathBuf,
    },
}

struct At {
    index: u32,
    phase: Phase,
    action: String,
}

struct Process<'a> {
    program: Program,
    directory: &'a Path,
    env: BTreeMap<OsString, OsString>,
    minutes: Option<Duration>,
    cancellable: bool,
    reports: Reports,
}

struct Launch<'a> {
    program: &'a OsStr,
    arguments: Vec<OsString>,
    directory: &'a Path,
    env: BTreeMap<OsString, OsString>,
    processor: Processor,
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    if cancel.wait_for(|cancelled| *cancelled).await.is_err() {
        std::future::pending::<()>().await;
    }
}
