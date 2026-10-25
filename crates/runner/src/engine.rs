use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, SystemTime};

use bosun_proto::assignment::{Contexts, Evaluated};
use bosun_types::ir::{Action, JobKind, JobTemplate, Shell, Step, Template};
use bosun_types::status::Conclusion;
use bosun_workflow::github::{self, BosunFacts, RunnerFacts, StepFacts};
use rustix::process::{Pid, Signal};
use serde_json::{Map, Value};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until, timeout};
use uuid::Uuid;

use crate::commands::Commands;
use crate::output::Output;
use crate::scope::{self, Fixed, Record, Standing, StepScope};
use crate::shell::Invocation;
use crate::tracking::{self, Processes, Tracking};

const INTERRUPT_GRACE: Duration = Duration::from_millis(7500);

const TERMINATE_GRACE: Duration = Duration::from_millis(2500);

const OUTPUT_AFTER_TEARDOWN: Duration = Duration::from_secs(2);

const SCRIPT: &str = "__run";

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
    pub minute: Duration,
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
    },
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
        minute,
    } = job;
    let JobKind::Steps(steps) = template.kind;
    let prepared =
        prepare(&workspace, &temp, &tool_cache, &contexts.github).and_then(|directories| {
            tracking
                .start(&id)
                .map(|processes| (directories, processes))
        });
    let (directories, processes) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            if !steps.is_empty() {
                let _ = events.send(Event::StepStarted {
                    index: 0,
                    at: SystemTime::now(),
                });
                line(
                    &events,
                    0,
                    format!("the job could not be prepared on this agent: {error}"),
                );
                finished(
                    &events,
                    0,
                    Conclusion::Failure,
                    Conclusion::Failure,
                    BTreeMap::new(),
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
        minute,
        deadline: limit(minute, timeout_minutes).and_then(|limit| started.checked_add(limit)),
        standing: Standing::default(),
        timed_out: false,
        files: BTreeMap::new(),
        paths: Vec::new(),
        records: Vec::new(),
        last: None,
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

fn actions(steps: &[Step]) -> Vec<String> {
    let mut taken = HashSet::new();
    steps
        .iter()
        .map(|step| {
            let base = step.key.as_ref().map_or(SCRIPT, |key| key.as_str());
            let mut name = base.to_owned();
            let mut number = 2;
            while !taken.insert(name.clone()) {
                name = format!("{base}_{number}");
                number += 1;
            }
            name
        })
        .collect()
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
    minute: Duration,
    deadline: Option<Instant>,
    standing: Standing,
    timed_out: bool,
    files: BTreeMap<String, String>,
    paths: Vec<String>,
    records: Vec<Record>,
    last: Option<u32>,
    processes: Processes,
    outputs: Vec<Output>,
    events: UnboundedSender<Event>,
    cancel: watch::Receiver<bool>,
}

struct Ran {
    outcome: Conclusion,
    timed_out: Option<Limit>,
    outputs: BTreeMap<String, String>,
}

impl Ran {
    fn failed() -> Ran {
        Ran {
            outcome: Conclusion::Failure,
            timed_out: None,
            outputs: BTreeMap::new(),
        }
    }
}

impl Execution {
    async fn steps(&mut self, steps: &[Step]) {
        for ((index, step), action) in (0..).zip(steps).zip(actions(steps)) {
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.timed_out = true;
            }
            if self.timed_out {
                return;
            }
            self.step(index, step, action).await;
        }
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

    async fn step(&mut self, index: u32, step: &Step, action: String) {
        if *self.cancel.borrow() {
            self.standing.cancelled = true;
        }
        let facts = StepFacts {
            action,
            id: Uuid::now_v7().to_string(),
        };
        let github = github::step_context(&self.github, &self.runner, &facts);
        let env = self.env_context();
        let mut scope = self.scope(&github, &env);
        let holds = match &step.r#if {
            None => Ok(self.standing.succeeding()),
            Some(condition) => bosun_expr::holds(condition, &scope)
                .map_err(|error| scope::fault("`if` of the step", &error)),
        };
        match holds {
            Ok(false) => {
                self.record(
                    step,
                    Conclusion::Skipped,
                    Conclusion::Skipped,
                    &BTreeMap::new(),
                );
                finished(
                    &self.events,
                    index,
                    Conclusion::Skipped,
                    Conclusion::Skipped,
                    BTreeMap::new(),
                );
                return;
            }
            Ok(true) => self.start(index),
            Err(fault) => {
                self.start(index);
                line(&self.events, index, fault);
                self.standing.failed = true;
                self.record(
                    step,
                    Conclusion::Failure,
                    Conclusion::Failure,
                    &BTreeMap::new(),
                );
                finished(
                    &self.events,
                    index,
                    Conclusion::Failure,
                    Conclusion::Failure,
                    BTreeMap::new(),
                );
                return;
            }
        }
        let cancellable = !self.standing.cancelled;
        let ran = self
            .execute(index, step, &github, env, &mut scope, cancellable)
            .await;
        self.conclude(index, step, ran, &scope);
    }

    fn start(&mut self, index: u32) {
        self.last = Some(index);
        let _ = self.events.send(Event::StepStarted {
            index,
            at: SystemTime::now(),
        });
    }

    fn conclude(&mut self, index: u32, step: &Step, ran: Ran, scope: &StepScope) {
        let Ran {
            outcome,
            timed_out,
            outputs,
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
                Err(fault) => line(&self.events, index, fault),
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
        self.record(step, outcome, conclusion, &outputs);
        finished(&self.events, index, outcome, conclusion, outputs);
    }

    fn record(
        &mut self,
        step: &Step,
        outcome: Conclusion,
        conclusion: Conclusion,
        outputs: &BTreeMap<String, String>,
    ) {
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
        index: u32,
        step: &Step,
        github: &Map<String, Value>,
        mut env: BTreeMap<String, String>,
        scope: &mut StepScope,
        cancellable: bool,
    ) -> Ran {
        let failed = |fault: String| {
            line(&self.events, index, fault);
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
        let directory = match &step.working_directory {
            Some(template) => {
                match scope::text("`working-directory` of the step", template, &*scope) {
                    Ok(directory) => Some(directory).filter(|directory| !directory.is_empty()),
                    Err(fault) => return failed(fault),
                }
            }
            None => None,
        };
        let script = match &step.action {
            Action::Run { script } => script,
            Action::Uses(reference) => {
                return failed(format!(
                    "the step uses the action `{reference}`, which this agent cannot run"
                ));
            }
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
        let (outcome, timed_out) = self
            .process(
                index,
                Process {
                    script: &script,
                    shell: shell.as_ref(),
                    directory: &directory,
                    env: self.process_env(github, &variables),
                    minutes,
                    cancellable,
                },
            )
            .await;
        let written = commands.read();
        for fault in &written.faults {
            line(&self.events, index, fault.clone());
        }
        self.files.extend(written.env);
        for path in written.paths {
            self.paths.retain(|known| *known != path);
            self.paths.push(path);
        }
        let outcome = if outcome == Conclusion::Success && !written.faults.is_empty() {
            Conclusion::Failure
        } else {
            outcome
        };
        Ran {
            outcome,
            timed_out,
            outputs: written.outputs.into_iter().collect(),
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
            script,
            shell,
            directory,
            env,
            minutes,
            cancellable,
        } = process;
        if !directory.is_dir() {
            line(
                &self.events,
                index,
                format!(
                    "the working directory {} does not exist",
                    directory.display()
                ),
            );
            return (Conclusion::Failure, None);
        }
        let invocation = match Invocation::resolve(
            shell,
            env.get(OsStr::new("PATH")).map(OsString::as_os_str),
        ) {
            Ok(invocation) => invocation,
            Err(reason) => {
                line(&self.events, index, reason);
                return (Conclusion::Failure, None);
            }
        };
        let file = self
            .temp
            .join(format!("step-{index}{}", invocation.extension()));
        if let Err(error) = fs::write(&file, script) {
            line(
                &self.events,
                index,
                format!(
                    "the script could not be written to {}: {error}",
                    file.display()
                ),
            );
            return (Conclusion::Failure, None);
        }
        let deadline = minutes
            .and_then(|minutes| Instant::now().checked_add(minutes))
            .map(|step| match self.deadline {
                Some(job) if job <= step => (job, Limit::Job),
                _ => (step, Limit::Step),
            })
            .or(self.deadline.map(|job| (job, Limit::Job)));
        let (child, mut output) = match self.spawn(index, &invocation, &file, directory, env) {
            Ok(started) => started,
            Err(error) => {
                line(
                    &self.events,
                    index,
                    format!(
                        "the step could not start `{}`: {error}",
                        invocation.program(&file)
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
                line(
                    &self.events,
                    index,
                    match limit {
                        Limit::Step => "the step ran beyond its `timeout-minutes`".to_owned(),
                        Limit::Job => "the job ran beyond its `timeout-minutes`".to_owned(),
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

    fn spawn(
        &mut self,
        index: u32,
        invocation: &Invocation,
        script: &Path,
        directory: &Path,
        env: BTreeMap<OsString, OsString>,
    ) -> io::Result<((Child, Pid), Output)> {
        let (reader, writer) = io::pipe()?;
        let errors = writer.try_clone()?;
        let mut command = Command::new(invocation.program(script));
        command
            .args(invocation.arguments(script))
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
        let output = Output::start(reader, index, self.events.clone())?;
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
                Ok(value) => {
                    outputs.insert(name.clone(), value);
                }
                Err(fault) => {
                    line(&self.events, self.last.unwrap_or(0), fault);
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

struct Process<'a> {
    script: &'a str,
    shell: Option<&'a Shell>,
    directory: &'a Path,
    env: BTreeMap<OsString, OsString>,
    minutes: Option<Duration>,
    cancellable: bool,
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    if cancel.wait_for(|cancelled| *cancelled).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn line(events: &UnboundedSender<Event>, index: u32, text: String) {
    let _ = events.send(Event::Line {
        index,
        at: SystemTime::now(),
        text: text.into_bytes(),
    });
}

fn finished(
    events: &UnboundedSender<Event>,
    index: u32,
    outcome: Conclusion,
    conclusion: Conclusion,
    outputs: BTreeMap<String, String>,
) {
    let _ = events.send(Event::StepFinished {
        index,
        at: SystemTime::now(),
        outcome,
        conclusion,
        outputs,
    });
}
