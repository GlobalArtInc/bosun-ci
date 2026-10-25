use std::io;
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bosun_proto::agent::v1::agent_message::Message;
use bosun_proto::agent::v1::{
    Annotation as Reported, AnnotationKind as ReportedKind, Assignment, Conclusion as Concluded,
    ForgeToken as Sent, JobFinished, StepFinished, StepStarted,
};
use bosun_proto::assignment::{self, Documents};
use bosun_runner::{
    Annotation, AnnotationKind, BosunFacts, Completed, Event, ForgeToken, Job, NodeAction, Renewal,
    Renewed, Runner, Tracking,
};
use bosun_types::id::ExecutionId;
use bosun_types::ir::{self, Action, JobKind, Shell};
use bosun_types::slug::{SLUG_RULE, is_slug};
use bosun_types::status::Conclusion;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{oneshot, watch};
use tokio::task;
use tokio::time::{Instant, sleep_until};
use tracing::{info, warn};

use crate::config::Config;
use crate::fetch::{Archive, Compression, Fetches};
use crate::host;
use crate::spool::Spool;

const CHUNK: usize = 64 * 1024;

const FLUSH_AFTER: Duration = Duration::from_millis(200);

const MINUTE: Duration = Duration::from_secs(60);

pub(crate) enum Out {
    Log {
        execution: ExecutionId,
        seq: u64,
    },
    Report(Message),
    Renew {
        execution: ExecutionId,
        reply: oneshot::Sender<Renewed>,
    },
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
    fetches: Fetches,
    forge_token: Option<ForgeToken>,
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
        actions,
    } = Documents::read(&assignment)?;
    let JobKind::Steps(steps) = &job.kind;
    for (index, step) in (0..).zip(steps) {
        runnable(index, step, &actions)?;
    }
    let (actions, archives) = local(&actions, &config.dir, &host::platform())?;
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
    let forge_token = assignment.forge_token.map(|token| received(*token));
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
            actions,
            forge_token: None,
            fetched: Ok(()),
            minute: MINUTE,
        },
        fetches: Fetches {
            url: config.url.clone(),
            credential: config.credential.clone(),
            archives,
        },
        forge_token,
    })
}

pub(crate) fn received(token: Sent) -> ForgeToken {
    let left = token
        .expires_in
        .and_then(|left| Duration::try_from(left).ok())
        .unwrap_or_default();
    ForgeToken {
        token: token.token,
        expires_at: Instant::now() + left,
    }
}

fn local(
    actions: &[assignment::Action],
    dir: &Path,
    platform: &str,
) -> Result<(Vec<NodeAction>, Vec<Archive>), String> {
    let mut local = Vec::new();
    let mut archives: Vec<Archive> = Vec::new();
    let mut runtimes: Vec<(String, String)> = Vec::new();
    for action in actions {
        let uses = &action.uses;
        let owner = uses.owner.to_lowercase();
        let repository = uses.repository.to_lowercase();
        let commit = action.commit.as_str();
        if [owner.as_str(), repository.as_str()]
            .iter()
            .any(|part| !segment(part))
            || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || commit.len() < 7
        {
            return Err(format!(
                "the action `{uses}` at `{commit}` names no directory this agent can fetch it into"
            ));
        }
        if !action.download.starts_with('/') {
            return Err(format!(
                "the action `{uses}` has the download address `{}`, which is not a path on the \
                 server",
                action.download
            ));
        }
        let root = dir
            .join("work/_actions")
            .join(&owner)
            .join(&repository)
            .join(commit);
        let directory = match &uses.path {
            Some(path) if inside(path) => root.join(path),
            Some(path) => {
                return Err(format!(
                    "the action `{uses}` has the path `{path}`, which leaves its repository"
                ));
            }
            None => root.clone(),
        };
        if !archives.iter().any(|archive| archive.into == root) {
            archives.push(Archive {
                path: action.download.clone(),
                compression: Compression::Gzip,
                into: root,
                marker: None,
            });
        }
        let major = node_major(&action.node).ok_or_else(|| {
            format!(
                "the action `{uses}` runs on Node `{}`, which is not a version",
                action.node
            )
        })?;
        let externals = dir.join("externals");
        let runtime = format!("node{major}");
        match runtimes.iter().find(|(known, _)| *known == runtime) {
            Some((_, version)) if *version != action.node => {
                return Err(format!(
                    "the assignment runs actions on Node {version} and {}, which would share \
                     {}",
                    action.node,
                    externals.join(&runtime).display()
                ));
            }
            Some(_) => {}
            None => {
                runtimes.push((runtime.clone(), action.node.clone()));
                archives.push(Archive {
                    path: format!("/_runtime/tools/node/{}/{platform}.tar.xz", action.node),
                    compression: Compression::Xz,
                    into: externals.join(&runtime),
                    marker: Some((
                        externals.join(format!("{runtime}.version")),
                        format!("{} {platform}\n", action.node),
                    )),
                });
            }
        }
        local.push(NodeAction {
            uses: uses.clone(),
            directory,
            node: externals.join(&runtime).join("bin/node"),
            metadata: action.metadata.clone(),
        });
    }
    Ok((local, archives))
}

fn segment(part: &str) -> bool {
    !matches!(part, "" | "." | "..") && !part.contains(['/', '\\', '\0'])
}

fn inside(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn node_major(version: &str) -> Option<&str> {
    let parts: Vec<&str> = version.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())))
    .then_some(parts[0])
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

fn runnable(index: u32, step: &ir::Step, actions: &[assignment::Action]) -> Result<(), String> {
    if let Action::Uses(reference) = &step.action
        && !actions.iter().any(|action| &action.uses == reference)
    {
        return Err(format!(
            "step {index} uses the action `{reference}`, which the assignment does not resolve"
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
    let Prepared {
        execution,
        mut job,
        fetches,
        forge_token,
    } = prepared;
    if let Some(token) = forge_token {
        let (renew, mut renewals) = mpsc::unbounded_channel();
        job.forge_token = Some(Renewal { token, renew });
        let out = out.clone();
        tokio::spawn(async move {
            while let Some(reply) = renewals.recv().await {
                if out.send(Out::Renew { execution, reply }).is_err() {
                    return;
                }
            }
        });
    }
    let (cancel, cancelled) = watch::channel(false);
    let (events, received) = mpsc::unbounded_channel();
    let runner = async move {
        job.fetched = fetches.run().await;
        bosun_runner::run(job, &tracking, events, cancelled).await
    };
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
                annotations,
                summary,
            } => {
                self.flush().await;
                self.report(Message::StepFinished(StepFinished {
                    execution_id: self.execution.to_string(),
                    step_index: index,
                    completed_at: Some(at.into()),
                    outcome: concluded(outcome).into(),
                    conclusion: concluded(conclusion).into(),
                    outputs: outputs.into_iter().collect(),
                    annotations: annotations.into_iter().map(reported).collect(),
                    summary: summary.unwrap_or_default(),
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

fn reported(annotation: Annotation) -> Reported {
    let kind = match annotation.kind {
        AnnotationKind::Notice => ReportedKind::Notice,
        AnnotationKind::Warning => ReportedKind::Warning,
        AnnotationKind::Error => ReportedKind::Error,
    };
    Reported {
        kind: kind.into(),
        message: annotation.message,
        title: annotation.title,
        file: annotation.file,
        line: annotation.line,
        end_line: annotation.end_line,
        column: annotation.column,
        end_column: annotation.end_column,
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
    use bosun_proto::assignment::{self, Contexts, Documents, Evaluated};
    use bosun_runner::Event;
    use bosun_types::id::ExecutionId;
    use bosun_types::ir::{
        Action, ActionMetadata, ActionReference, Defaults, Evaluable, JobKind, JobTemplate,
        NodeRuns, NodeRuntime, Runs, RunsOn, SecretReferences, Shell, Span, Step, Template,
    };
    use serde_json::{Map, Value, json};
    use tokio::sync::mpsc;

    use super::{Out, Pump, local, prepare};
    use crate::config::Config;
    use crate::fetch::{Archive, Compression};
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
            actions: Vec::new(),
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

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn resolved(owner: &str, path: Option<&str>, node: &str) -> assignment::Action {
        assignment::Action {
            uses: ActionReference {
                owner: owner.to_owned(),
                repository: "Actions".to_owned(),
                path: path.map(str::to_owned),
                r#ref: "v1".to_owned(),
            },
            commit: COMMIT.to_owned(),
            download: format!(
                "/_runtime/actions/{}/actions/{COMMIT}.tar.gz",
                owner.to_lowercase()
            ),
            metadata: ActionMetadata {
                name: None,
                description: None,
                author: None,
                branding: None,
                inputs: Vec::new(),
                outputs: Vec::new(),
                runs: Runs::Node(NodeRuns {
                    using: NodeRuntime::Node16,
                    main: "main.js".to_owned(),
                    pre: None,
                    pre_if: None,
                    post: None,
                    post_if: None,
                }),
            },
            node: node.to_owned(),
        }
    }

    #[test]
    fn actions_go_under_their_commit_and_node_under_its_major_version_each_fetched_once() {
        let dir = PathBuf::from("/var/lib/bosun/agents/1");
        let (actions, archives) = local(
            &[
                resolved("Bosun-Fixtures", Some("first"), "24.21.0"),
                resolved("Bosun-Fixtures", Some("second/nested"), "24.21.0"),
                resolved("acme", None, "20.19.5"),
            ],
            &dir,
            "linux-x64",
        )
        .expect("local actions");
        let root = dir.join(format!("work/_actions/bosun-fixtures/actions/{COMMIT}"));
        assert_eq!(
            actions
                .iter()
                .map(|action| (action.directory.clone(), action.node.clone()))
                .collect::<Vec<_>>(),
            [
                (root.join("first"), dir.join("externals/node24/bin/node")),
                (
                    root.join("second/nested"),
                    dir.join("externals/node24/bin/node")
                ),
                (
                    dir.join(format!("work/_actions/acme/actions/{COMMIT}")),
                    dir.join("externals/node20/bin/node")
                ),
            ]
        );
        assert_eq!(
            archives,
            [
                Archive {
                    path: format!("/_runtime/actions/bosun-fixtures/actions/{COMMIT}.tar.gz"),
                    compression: Compression::Gzip,
                    into: root,
                    marker: None,
                },
                Archive {
                    path: "/_runtime/tools/node/24.21.0/linux-x64.tar.xz".to_owned(),
                    compression: Compression::Xz,
                    into: dir.join("externals/node24"),
                    marker: Some((
                        dir.join("externals/node24.version"),
                        "24.21.0 linux-x64\n".to_owned()
                    )),
                },
                Archive {
                    path: format!("/_runtime/actions/acme/actions/{COMMIT}.tar.gz"),
                    compression: Compression::Gzip,
                    into: dir.join(format!("work/_actions/acme/actions/{COMMIT}")),
                    marker: None,
                },
                Archive {
                    path: "/_runtime/tools/node/20.19.5/linux-x64.tar.xz".to_owned(),
                    compression: Compression::Xz,
                    into: dir.join("externals/node20"),
                    marker: Some((
                        dir.join("externals/node20.version"),
                        "20.19.5 linux-x64\n".to_owned()
                    )),
                },
            ]
        );
    }

    #[test]
    fn actions_that_would_leave_their_directories_or_share_one_runtime_are_refused() {
        let dir = PathBuf::from("/var/lib/bosun/agents/1");
        let mut commit = resolved("acme", None, "24.21.0");
        commit.commit = "../../../etc".to_owned();
        let mut download = resolved("acme", None, "24.21.0");
        download.download = "https://elsewhere.example/a.tar.gz".to_owned();
        for (actions, expected) in [
            (
                vec![resolved("..", None, "24.21.0")],
                "the action `../Actions@v1` at `0123456789abcdef0123456789abcdef01234567` names \
                 no directory this agent can fetch it into",
            ),
            (
                vec![commit],
                "the action `acme/Actions@v1` at `../../../etc` names no directory this agent can \
                 fetch it into",
            ),
            (
                vec![resolved("acme", Some("../escape"), "24.21.0")],
                "the action `acme/Actions/../escape@v1` has the path `../escape`, which leaves its \
                 repository",
            ),
            (
                vec![download],
                "the action `acme/Actions@v1` has the download address \
                 `https://elsewhere.example/a.tar.gz`, which is not a path on the server",
            ),
            (
                vec![resolved("acme", None, "24")],
                "the action `acme/Actions@v1` runs on Node `24`, which is not a version",
            ),
            (
                vec![
                    resolved("acme", None, "24.21.0"),
                    resolved("other", None, "24.22.1"),
                ],
                "the assignment runs actions on Node 24.21.0 and 24.22.1, which would share \
                 /var/lib/bosun/agents/1/externals/node24",
            ),
        ] {
            let refused = local(&actions, &dir, "linux-x64").expect_err("a refusal");
            assert_eq!(refused, expected);
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
                Out::Renew { .. } => panic!("an unexpected renewal"),
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
