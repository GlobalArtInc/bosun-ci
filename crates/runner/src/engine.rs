use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, SystemTime};

use bosun_types::ir::Shell;
use bosun_types::status::Conclusion;
use rustix::process::{Pid, Signal};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tokio::time::timeout;

use crate::output::Output;
use crate::shell::Invocation;
use crate::tracking::{self, Processes, Tracking};

const INTERRUPT_GRACE: Duration = Duration::from_millis(7500);

const TERMINATE_GRACE: Duration = Duration::from_millis(2500);

const OUTPUT_AFTER_TEARDOWN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub workspace: PathBuf,
    pub temp: PathBuf,
    pub env: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub index: u32,
    pub script: String,
    pub shell: Option<Shell>,
    pub working_directory: Option<String>,
    pub env: BTreeMap<String, String>,
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
    },
}

pub async fn run(
    job: Job,
    tracking: &Tracking,
    events: UnboundedSender<Event>,
    cancel: watch::Receiver<bool>,
) -> Conclusion {
    let prepared = prepare(&job).and_then(|directories| {
        tracking
            .start(&job.id)
            .map(|processes| (directories, processes))
    });
    let ((workspace, temp), processes) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let Some(first) = job.steps.first() else {
                return Conclusion::Failure;
            };
            let _ = events.send(Event::StepStarted {
                index: first.index,
                at: SystemTime::now(),
            });
            line(
                &events,
                first.index,
                format!("the job could not be prepared on this agent: {error}"),
            );
            finished(&events, first.index, Conclusion::Failure);
            return Conclusion::Failure;
        }
    };
    let mut base: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    base.insert("CI".into(), "true".into());
    base.insert("GITHUB_WORKSPACE".into(), workspace.clone().into());
    if let Some(parent) = workspace.parent() {
        base.insert("RUNNER_WORKSPACE".into(), parent.into());
    }
    base.insert("RUNNER_TEMP".into(), temp.clone().into());
    for (name, value) in &job.env {
        base.insert(name.into(), value.into());
    }
    let mut execution = Execution {
        workspace,
        temp,
        base,
        processes,
        outputs: Vec::new(),
        events,
        cancel,
    };
    let conclusion = execution.steps(&job.steps).await;
    execution.teardown().await;
    conclusion
}

fn prepare(job: &Job) -> io::Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(&job.workspace)?;
    fs::create_dir_all(&job.temp)?;
    Ok((
        fs::canonicalize(&job.workspace)?,
        fs::canonicalize(&job.temp)?,
    ))
}

struct Execution {
    workspace: PathBuf,
    temp: PathBuf,
    base: BTreeMap<OsString, OsString>,
    processes: Processes,
    outputs: Vec<Output>,
    events: UnboundedSender<Event>,
    cancel: watch::Receiver<bool>,
}

impl Execution {
    async fn steps(&mut self, steps: &[Step]) -> Conclusion {
        for step in steps {
            if *self.cancel.borrow() {
                return Conclusion::Cancelled;
            }
            let _ = self.events.send(Event::StepStarted {
                index: step.index,
                at: SystemTime::now(),
            });
            let outcome = self.step(step).await;
            finished(&self.events, step.index, outcome);
            if outcome != Conclusion::Success {
                return outcome;
            }
        }
        Conclusion::Success
    }

    async fn step(&mut self, step: &Step) -> Conclusion {
        let mut env = self.base.clone();
        for (name, value) in &step.env {
            env.insert(name.into(), value.into());
        }
        let directory = match &step.working_directory {
            Some(directory) => self.workspace.join(directory),
            None => self.workspace.clone(),
        };
        if !directory.is_dir() {
            line(
                &self.events,
                step.index,
                format!(
                    "the working directory {} does not exist",
                    directory.display()
                ),
            );
            return Conclusion::Failure;
        }
        let invocation = match Invocation::resolve(
            step.shell.as_ref(),
            env.get(OsStr::new("PATH")).map(OsString::as_os_str),
        ) {
            Ok(invocation) => invocation,
            Err(reason) => {
                line(&self.events, step.index, reason);
                return Conclusion::Failure;
            }
        };
        let script = self
            .temp
            .join(format!("step-{}{}", step.index, invocation.extension()));
        if let Err(error) = fs::write(&script, &step.script) {
            line(
                &self.events,
                step.index,
                format!(
                    "the script could not be written to {}: {error}",
                    script.display()
                ),
            );
            return Conclusion::Failure;
        }
        let (child, mut output) = match self.spawn(step, &invocation, &script, &directory, env) {
            Ok(started) => started,
            Err(error) => {
                line(
                    &self.events,
                    step.index,
                    format!(
                        "the step could not start `{}`: {error}",
                        invocation.program(&script)
                    ),
                );
                return Conclusion::Failure;
            }
        };
        let (status, cancelled) = self.wait(child).await;
        output.flush().await;
        self.outputs.push(output);
        match status {
            _ if cancelled => Conclusion::Cancelled,
            Ok(status) if status.success() => Conclusion::Success,
            _ => Conclusion::Failure,
        }
    }

    fn spawn(
        &mut self,
        step: &Step,
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
        let output = Output::start(reader, step.index, self.events.clone())?;
        Ok(((child, group), output))
    }

    async fn wait(&mut self, (mut child, group): (Child, Pid)) -> (io::Result<ExitStatus>, bool) {
        tokio::select! {
            status = child.wait() => return (status, false),
            () = cancelled(&mut self.cancel) => {}
        }
        tracking::signal(group, Signal::INT);
        if let Ok(status) = timeout(INTERRUPT_GRACE, child.wait()).await {
            return (status, true);
        }
        tracking::signal(group, Signal::TERM);
        if let Ok(status) = timeout(TERMINATE_GRACE, child.wait()).await {
            return (status, true);
        }
        tracking::signal(group, Signal::KILL);
        (child.wait().await, true)
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

fn finished(events: &UnboundedSender<Event>, index: u32, outcome: Conclusion) {
    let _ = events.send(Event::StepFinished {
        index,
        at: SystemTime::now(),
        outcome,
        conclusion: outcome,
    });
}
