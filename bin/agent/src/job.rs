use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bosun_proto::agent::v1::agent_message::Message;
use bosun_proto::agent::v1::segment::Segment;
use bosun_proto::agent::v1::step::Action;
use bosun_proto::agent::v1::{
    Assignment, Conclusion as Concluded, JobFinished, StepFinished, StepStarted, Template,
};
use bosun_runner::{Event, Job, Step, Tracking};
use bosun_types::id::ExecutionId;
use bosun_types::ir::Shell;
use bosun_types::slug::{SLUG_RULE, is_slug};
use bosun_types::status::Conclusion;
use chrono::{DateTime, SecondsFormat, Utc};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use tokio::task;
use tokio::time::{Instant, sleep_until};
use tracing::{info, warn};

use crate::config::Config;
use crate::spool::Spool;

const CHUNK: usize = 64 * 1024;

const FLUSH_AFTER: Duration = Duration::from_millis(200);

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

pub(crate) fn prepare(assignment: Assignment, config: &Config) -> Result<Prepared, String> {
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
    let work = config.dir.join("work");
    let mut env: BTreeMap<String, String> = config.proxies().into_iter().collect();
    env.extend(assignment.env);
    let mut steps = Vec::with_capacity(assignment.steps.len());
    for step in assignment.steps {
        let index = step.index;
        let script = match step.action {
            Some(Action::Run(script)) => text(script, index)?,
            None => return Err(format!("step {index} has no action this agent knows")),
        };
        let working_directory = step
            .working_directory
            .map(|directory| text(directory, index))
            .transpose()?
            .filter(|directory| !directory.is_empty());
        let mut variables = BTreeMap::new();
        for (name, value) in step.env {
            variables.insert(name, text(value, index)?);
        }
        steps.push(Step {
            index,
            script,
            shell: step
                .shell
                .map(|shell| {
                    shell
                        .parse::<Shell>()
                        .map_err(|error| format!("the shell of step {index} is {error}"))
                })
                .transpose()?,
            working_directory,
            env: variables,
        });
    }
    Ok(Prepared {
        execution,
        job: Job {
            id: execution.to_string(),
            workspace: work
                .join(format!(
                    "{}.{}",
                    assignment.organisation, assignment.project
                ))
                .join(&assignment.project),
            temp: work.join("_temp"),
            env,
            steps,
        },
    })
}

fn text(template: Template, index: u32) -> Result<String, String> {
    let mut text = String::new();
    for segment in template.segments {
        match segment.segment {
            Some(Segment::Text(part)) => text.push_str(&part),
            None => {
                return Err(format!(
                    "step {index} holds a template segment this agent does not know"
                ));
            }
        }
    }
    Ok(text)
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
        runner: impl Future<Output = Conclusion>,
    ) {
        let mut runner = std::pin::pin!(runner);
        let mut conclusion = None;
        loop {
            let deadline = self.deadline;
            tokio::select! {
                event = events.recv() => match event {
                    Some(event) => self.handle(event).await,
                    None => break,
                },
                finished = &mut runner, if conclusion.is_none() => conclusion = Some(finished),
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.flush().await;
                }
            }
        }
        self.flush().await;
        let conclusion = match conclusion {
            Some(conclusion) => conclusion,
            None => runner.await,
        };
        info!(execution_id = %self.execution, ?conclusion, "finished a job");
        self.report(Message::JobFinished(JobFinished {
            execution_id: self.execution.to_string(),
            conclusion: concluded(conclusion).into(),
            outputs: Default::default(),
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
            } => {
                self.flush().await;
                self.report(Message::StepFinished(StepFinished {
                    execution_id: self.execution.to_string(),
                    step_index: index,
                    completed_at: Some(at.into()),
                    outcome: concluded(outcome).into(),
                    conclusion: concluded(conclusion).into(),
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::SystemTime;

    use bosun_proto::agent::v1::segment::Segment as Text;
    use bosun_proto::agent::v1::step::Action;
    use bosun_proto::agent::v1::{Assignment, Segment, Step as Offered, Template};
    use bosun_runner::Event;
    use bosun_types::id::ExecutionId;
    use bosun_types::ir::Shell;
    use tokio::sync::mpsc;

    use super::{Out, Pump, prepare};
    use crate::config::Config;
    use crate::spool::Spool;

    const EXECUTION: &str = "0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3a";

    fn template(text: &str) -> Template {
        Template {
            segments: vec![Segment {
                segment: Some(Text::Text(text.to_owned())),
            }],
        }
    }

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

    #[test]
    fn an_assignment_becomes_a_job_in_the_workspace_of_its_project() {
        let mut assignment = Assignment {
            execution_id: EXECUTION.to_owned(),
            organisation: "globalart".to_owned(),
            project: "keel".to_owned(),
            ..Assignment::default()
        };
        assignment
            .env
            .insert("http_proxy".to_owned(), "http://job:8080".to_owned());
        assignment.steps.push(Offered {
            index: 4,
            action: Some(Action::Run(template("make"))),
            shell: Some("bash -c \"{0}\"".to_owned()),
            working_directory: Some(Template::default()),
            env: [("A".to_owned(), template("b"))].into(),
            ..Offered::default()
        });
        let prepared = prepare(assignment, &config()).expect("a job");
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
        assert_eq!(job.env["HTTP_PROXY"], "http://proxy:3128");
        assert_eq!(job.env["http_proxy"], "http://job:8080");
        let step = &job.steps[0];
        assert_eq!(step.index, 4);
        assert_eq!(step.script, "make");
        assert_eq!(
            step.shell,
            Some(Shell::Custom("bash -c \"{0}\"".to_owned()))
        );
        assert_eq!(step.working_directory, None);
        assert_eq!(step.env["A"], "b");
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
