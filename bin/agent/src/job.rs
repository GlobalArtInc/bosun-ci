use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bosun_proto::agent::v1::agent_message::Message;
use bosun_proto::agent::v1::{
    Assignment, Conclusion as Concluded, JobFinished, StepFinished, StepStarted,
};
use bosun_proto::assignment::Documents;
use bosun_runner::{BosunFacts, Completed, Event, Job, Runner, Tracking};
use bosun_types::id::ExecutionId;
use bosun_types::ir::{self, Action, JobKind, Shell};
use bosun_types::slug::{SLUG_RULE, is_slug};
use bosun_types::status::Conclusion;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use tokio::task;
use tokio::time::{Instant, sleep_until};
use tracing::{info, warn};

use crate::config::Config;
use crate::host;
use crate::spool::Spool;

const CHUNK: usize = 64 * 1024;

const FLUSH_AFTER: Duration = Duration::from_millis(200);

const MINUTE: Duration = Duration::from_secs(60);

pub(crate) enum Out {
    Log { execution: ExecutionId, seq: u64 },
    Report(Message),
    Finished(ExecutionId),
}

pub(crate) struct Running {
    pub(crate) execution: ExecutionId,
    cancel: watch::Sender<bool>,
}

impl Running {
    pub(crate) fn cancel(&self) {
        self.cancel.send_replace(true);
    }
}

pub(crate) struct Prepared {
    execution: ExecutionId,
    job: Job,
}

impl Prepared {
    pub(crate) fn execution(&self) -> ExecutionId {
        self.execution
    }
}

pub(crate) fn prepare(
    assignment: Assignment,
    config: &Config,
    name: &str,
) -> Result<Prepared, String> {
    let execution: ExecutionId = assignment.execution_id.parse().map_err(|_| {
        format!(
            "`{}` is not the id of an execution",
            assignment.execution_id
        )
    })?;
    for (what, slug) in [
        ("an organisation", &assignment.organisation),
        ("a project", &assignment.project),
    ] {
        if !is_slug(slug) {
            return Err(format!(
                "`{slug}` is not the slug of {what}: a slug is {SLUG_RULE}"
            ));
        }
    }
    let Documents {
        job,
        contexts,
        evaluated,
    } = Documents::read(&assignment)?;
    let JobKind::Steps(steps) = &job.kind;
    for (index, step) in (0..).zip(steps) {
        runnable(index, step)?;
    }
    let bosun = BosunFacts {
        url: property(&contexts.bosun, "url"),
        run_id: property(&contexts.bosun, "run_id"),
        run_url: property(&contexts.bosun, "run_url"),
        job_id: assignment.job_id.clone(),
        draft: contexts.bosun.get("draft") == Some(&Value::Bool(true)),
    };
    let repository = repository(&contexts.github)?
        .unwrap_or(&assignment.project)
        .to_owned();
    let work = config.dir.join("work");
    Ok(Prepared {
        execution,
        job: Job {
            id: execution.to_string(),
            workspace: work
                .join(format!(
                    "{}.{}",
                    assignment.organisation, assignment.project
                ))
                .join(repository),
            temp: work.join("_temp"),
            tool_cache: config.dir.join("tools"),
            runner: Runner {
                name: name.to_owned(),
                os: host::os().to_owned(),
                arch: host::arch().to_owned(),
            },
            bosun,
            proxies: config.proxies().into_iter().collect(),
            template: job,
            contexts,
            evaluated,
            minute: MINUTE,
        },
    })
}

fn repository(github: &Map<String, Value>) -> Result<Option<&str>, String> {
    let Some(full) = github
        .get("repository")
        .and_then(Value::as_str)
        .filter(|full| !full.is_empty())
    else {
        return Ok(None);
    };
    match full.split_once('/') {
        Some((owner, name))
            if !owner.is_empty()
                && !matches!(name, "" | "." | "..")
                && !name.contains(['/', '\\', '\0']) =>
        {
            Ok(Some(name))
        }
        _ => Err(format!(
            "`{full}` is not an owner and a repository whose name can name a workspace"
        )),
    }
}

fn property(context: &Map<String, Value>, name: &str) -> String {
    context
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn runnable(index: u32, step: &ir::Step) -> Result<(), String> {
    if let Action::Uses(reference) = &step.action {
        return Err(format!(
            "step {index} uses the action `{reference}`, which agents run from M1 step 33"
        ));
    }
    if let Some(Shell::Custom(template)) = &step.shell {
        template
            .parse::<Shell>()
            .map_err(|error| format!("the shell of step {index} is {error}"))?;
    }
    Ok(())
}

pub(crate) fn start(
    prepared: Prepared,
    tracking: Arc<Tracking>,
    spool: Arc<Spool>,
    out: UnboundedSender<Out>,
) -> Running {
    let Prepared { execution, job } = prepared;
    let (cancel, cancelled) = watch::channel(false);
    let (events, received) = mpsc::unbounded_channel();
    let runner = async move { bosun_runner::run(job, &tracking, events, cancelled).await };
    let pump = Pump {
        execution,
        spool,
        out,
        chunk: Vec::new(),
        step: 0,
        deadline: None,
        dropped: 0,
    };
    tokio::spawn(pump.run(received, runner));
    Running { execution, cancel }
}

struct Pump {
    execution: ExecutionId,
    spool: Arc<Spool>,
    out: UnboundedSender<Out>,
    chunk: Vec<u8>,
    step: u32,
    deadline: Option<Instant>,
    dropped: u64,
}

impl Pump {
    async fn run(
        mut self,
        mut events: UnboundedReceiver<Event>,
        runner: impl Future<Output = Completed>,
    ) {
        let mut runner = std::pin::pin!(runner);
        let mut completed = None;
        loop {
            let deadline = self.deadline;
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => self.handle(event).await,
                    None => break,
                },
                finished = &mut runner, if completed.is_none() => completed = Some(finished),
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.flush().await;
                }
            }
        }
        self.flush().await;
        let Completed {
            conclusion,
            outputs,
        } = match completed {
            Some(completed) => completed,
            None => runner.await,
        };
        info!(execution_id = %self.execution, ?conclusion, "finished a job");
        self.report(Message::JobFinished(JobFinished {
            execution_id: self.execution.to_string(),
            conclusion: concluded(conclusion).into(),
            outputs: outputs.into_iter().collect(),
        }));
        if let Err(error) = self.spool.finish(self.execution) {
            warn!(execution_id = %self.execution, %error, "the log spool could not record the end of a job");
        }
        let _ = self.out.send(Out::Finished(self.execution));
    }

    async fn handle(&mut self, event: Event) {
        match event {
            Event::StepStarted { index, at } => {
                self.flush().await;
                self.report(Message::StepStarted(StepStarted {
                    execution_id: self.execution.to_string(),
                    step_index: index,
                    started_at: Some(at.into()),
                }));
            }
            Event::Line { index, at, text } => {
                if index != self.step {
                    self.flush().await;
                    self.step = index;
                }
                self.line(at, &text);
                if self.chunk.len() >= CHUNK {
                    self.flush().await;
                }
            }
            Event::StepFinished {
                index,
                at,
                outcome,
                conclusion,
                outputs,
            } => {
                self.flush().await;
                self.report(Message::StepFinished(StepFinished {
                    execution_id: self.execution.to_string(),
                    step_index: index,
                    completed_at: Some(at.into()),
                    outcome: concluded(outcome).into(),
                    conclusion: concluded(conclusion).into(),
                    outputs: outputs.into_iter().collect(),
                }));
            }
        }
    }

    fn line(&mut self, at: SystemTime, text: &[u8]) {
        if self.chunk.is_empty() {
            self.deadline = Some(Instant::now() + FLUSH_AFTER);
        }
        self.chunk.extend_from_slice(
            DateTime::<Utc>::from(at)
                .to_rfc3339_opts(SecondsFormat::Nanos, true)
                .as_bytes(),
        );
        self.chunk.push(b' ');
        self.chunk.extend_from_slice(text);
        self.chunk.push(b'\n');
    }

    async fn flush(&mut self) {
        self.deadline = None;
        if self.chunk.is_empty() {
            return;
        }
        let mut lines = std::mem::take(&mut self.chunk);
        let produced = lines.len() as u64;
        if self.dropped > 0 {
            let notice = format!(
                "{} {} bytes of output were dropped because the log spool of the agent was full\n",
                DateTime::<Utc>::from(SystemTime::now())
                    .to_rfc3339_opts(SecondsFormat::Nanos, true),
                self.dropped
            );
            lines.splice(0..0, notice.into_bytes());
        }
        let spool = Arc::clone(&self.spool);
        let (execution, step) = (self.execution, self.step);
        let stored = task::spawn_blocking(move || spool.store(execution, step, &lines))
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)));
        match stored {
            Ok(Some(seq)) => {
                self.dropped = 0;
                let _ = self.out.send(Out::Log {
                    execution: self.execution,
                    seq,
                });
            }
            Ok(None) => self.dropped += produced,
            Err(error) => {
                warn!(execution_id = %self.execution, %error, "a log chunk could not be spooled");
                self.dropped += produced;
            }
        }
    }

    fn report(&self, message: Message) {
        let _ = self.out.send(Out::Report(message));
    }
}

fn concluded(conclusion: Conclusion) -> Concluded {
    match conclusion {
        Conclusion::Success => Concluded::Success,
        Conclusion::Failure => Concluded::Failure,
        Conclusion::Cancelled => Concluded::Cancelled,
        Conclusion::Skipped => Concluded::Skipped,
        Conclusion::TimedOut => Concluded::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use bosun_proto::agent::v1::Assignment;
    use bosun_proto::assignment::{Contexts, Documents, Evaluated};
    use bosun_runner::Event;
    use bosun_types::id::ExecutionId;
    use bosun_types::ir::{
        Action, Defaults, Evaluable, JobKind, JobTemplate, RunsOn, SecretReferences, Shell, Span,
        Step, Template,
    };
    use serde_json::{Map, Value, json};
    use tokio::sync::mpsc;

    use super::{Out, Pump, prepare};
    use crate::config::Config;
    use crate::spool::Spool;

    const EXECUTION: &str = "0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3a";

    const SPAN: Span = Span {
        line: 1,
        column: 1,
        end_line: 1,
        end_column: 1,
    };

    fn config() -> Config {
        Config {
            url: "https://ci.example.org".to_owned(),
            dir: PathBuf::from("/var/lib/bosun/agents/1"),
            agent_id: EXECUTION.parse().expect("an agent id"),
            credential: "bat_x".to_owned(),
            labels: Vec::new(),
            http_proxy: Some("http://proxy:3128".to_owned()),
            https_proxy: None,
            no_proxy: None,
            spool_max_bytes: None,
        }
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    fn documents(step: Step) -> Documents {
        Documents {
            job: JobTemplate {
                key: "build".parse().expect("a job key"),
                name: None,
                needs: Vec::new(),
                r#if: None,
                runs_on: Evaluable::Literal(RunsOn {
                    group: None,
                    labels: vec![Template::text("self-hosted")],
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
                kind: JobKind::Steps(vec![step]),
                span: SPAN,
            },
            contexts: Contexts {
                github: object(json!({ "job": "build", "workspace": "", "repository": "" })),
                needs: Map::new(),
                strategy: Value::Null,
                matrix: Value::Null,
                inputs: Map::new(),
                vars: Map::new(),
                secrets: Map::new(),
                bosun: object(json!({
                    "url": "https://ci.example.org",
                    "run_url": "https://ci.example.org/runs/r",
                    "run_id": "r",
                    "draft": true,
                })),
            },
            evaluated: Evaluated {
                env: [("http_proxy".to_owned(), "http://job:8080".to_owned())].into(),
                shell: None,
                working_directory: None,
                timeout_minutes: 360,
                continue_on_error: false,
            },
        }
    }

    fn step() -> Step {
        Step {
            key: Some("make".parse().expect("a step key")),
            name: None,
            r#if: None,
            action: Action::Run {
                script: Template::text("make"),
            },
            shell: Some(Shell::Custom("bash -c \"{0}\"".to_owned())),
            working_directory: Some(Template::default()),
            with: BTreeMap::new(),
            env: [("A".to_owned(), Template::text("b"))].into(),
            continue_on_error: Evaluable::Literal(false),
            timeout_minutes: None,
            span: SPAN,
        }
    }

    fn assignment(documents: &Documents) -> Assignment {
        let mut assignment = Assignment {
            execution_id: EXECUTION.to_owned(),
            job_id: "j".to_owned(),
            organisation: "globalart".to_owned(),
            project: "keel".to_owned(),
            ..Assignment::default()
        };
        documents.write(&mut assignment).expect("JSON documents");
        assignment
    }

    #[test]
    fn an_assignment_becomes_a_job_in_the_workspace_of_its_project() {
        let documents = documents(step());
        let assignment = assignment(&documents);
        let prepared = prepare(assignment, &config(), "runner1-1").expect("a job");
        assert_eq!(
            prepared.execution(),
            EXECUTION.parse::<ExecutionId>().expect("an id")
        );
        let job = prepared.job;
        assert_eq!(job.id, EXECUTION);
        assert_eq!(
            job.workspace,
            PathBuf::from("/var/lib/bosun/agents/1/work/globalart.keel/keel")
        );
        assert_eq!(
            job.temp,
            PathBuf::from("/var/lib/bosun/agents/1/work/_temp")
        );
        assert_eq!(
            job.tool_cache,
            PathBuf::from("/var/lib/bosun/agents/1/tools")
        );
        assert_eq!(job.runner.name, "runner1-1");
        assert_eq!(
            (
                job.bosun.url.as_str(),
                job.bosun.run_url.as_str(),
                job.bosun.run_id.as_str(),
                job.bosun.job_id.as_str(),
                job.bosun.draft
            ),
            (
                "https://ci.example.org",
                "https://ci.example.org/runs/r",
                "r",
                "j",
                true
            )
        );
        assert_eq!(
            job.proxies,
            [
                ("HTTP_PROXY", "http://proxy:3128"),
                ("http_proxy", "http://proxy:3128")
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .into()
        );
        assert_eq!(job.evaluated, documents.evaluated);
        assert_eq!(job.contexts, documents.contexts);
        assert_eq!(job.template, documents.job);
        assert_eq!(job.minute, Duration::from_secs(60));
    }

    #[test]
    fn the_workspace_of_a_repository_is_named_after_it_and_a_name_that_escapes_is_refused() {
        let mut documents = documents(step());
        documents
            .contexts
            .github
            .insert("repository".to_owned(), json!("GlobalArtInc/keel-ci"));
        let prepared = prepare(assignment(&documents), &config(), "runner1-1").expect("a job");
        assert_eq!(
            prepared.job.workspace,
            PathBuf::from("/var/lib/bosun/agents/1/work/globalart.keel/keel-ci")
        );
        for repository in ["GlobalArtInc/..", "GlobalArtInc/", "/keel", "keel", "a/b/c"] {
            documents
                .contexts
                .github
                .insert("repository".to_owned(), json!(repository));
            let refused = prepare(assignment(&documents), &config(), "runner1-1")
                .err()
                .unwrap_or_else(|| panic!("`{repository}` named a workspace"));
            assert_eq!(
                refused,
                format!(
                    "`{repository}` is not an owner and a repository whose name can name a \
                     workspace"
                )
            );
        }
    }

    #[tokio::test]
    async fn output_beyond_the_spool_limit_is_dropped_and_the_log_says_how_much() {
        let directory = tempfile::tempdir().expect("a directory");
        let spool = Arc::new(Spool::open(&directory.path().join("spool"), 200).expect("a spool"));
        let execution: ExecutionId = EXECUTION.parse().expect("an id");
        let (out, mut results) = mpsc::unbounded_channel();
        let mut pump = Pump {
            execution,
            spool: Arc::clone(&spool),
            out,
            chunk: Vec::new(),
            step: 0,
            deadline: None,
            dropped: 0,
        };
        let line = |text: &str| Event::Line {
            index: 0,
            at: SystemTime::UNIX_EPOCH,
            text: text.as_bytes().to_vec(),
        };
        pump.handle(line("kept")).await;
        pump.flush().await;
        pump.handle(line(&"x".repeat(200))).await;
        pump.flush().await;
        let seqs: Vec<u64> = std::iter::from_fn(|| results.try_recv().ok())
            .map(|out| match out {
                Out::Log { seq, .. } => seq,
                Out::Report(message) => panic!("an unexpected report {message:?}"),
                Out::Finished(_) => panic!("an unexpected end"),
            })
            .collect();
        assert_eq!(seqs, vec![1]);
        let first = spool.read(execution, 1).expect("read").expect("a chunk");
        assert_eq!(
            String::from_utf8(first.lines).expect("UTF-8"),
            "1970-01-01T00:00:00.000000000Z kept\n"
        );
        spool.acknowledge(execution, 1).expect("acknowledged");
        pump.handle(line("after")).await;
        pump.flush().await;
        let Some(Out::Log { seq: 2, .. }) = results.try_recv().ok() else {
            panic!("the chunk after the drop was not spooled");
        };
        let second = spool.read(execution, 2).expect("read").expect("a chunk");
        let text = String::from_utf8(second.lines).expect("UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with(
                " 232 bytes of output were dropped because the log spool of the agent was full"
            ),
            "{text}"
        );
        assert_eq!(lines[1], "1970-01-01T00:00:00.000000000Z after");
    }
}
