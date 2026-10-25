mod cancel;
mod jobs;
mod masking;
mod reconnect;
mod register;
mod server;
mod spool;
mod stop;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use std::collections::{BTreeMap, BTreeSet};

use bosun_proto::agent::v1::{Assignment, Log};
use bosun_proto::assignment::{Contexts, Documents, Evaluated};
use bosun_types::ir::{
    Action, Defaults, Evaluable, JobKind, JobTemplate, RunsOn, SecretReferences, Span, Step,
    Template,
};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

use crate::server::{FakeServer, TOKEN};

pub const WAIT: Duration = Duration::from_secs(60);

pub const NAME: &str = "runner1-1";

pub fn agent() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bosun-agent"));
    command.stdin(Stdio::null()).kill_on_drop(true);
    command
}

pub async fn output(mut command: Command) -> Output {
    timeout(WAIT, command.output())
        .await
        .unwrap_or_else(|_| panic!("bosun-agent did not exit within {WAIT:?}"))
        .expect("bosun-agent runs")
}

pub struct Home {
    directory: TempDir,
}

impl Home {
    pub fn empty() -> Home {
        Home {
            directory: tempfile::tempdir().expect("a home for the agent"),
        }
    }

    pub fn config(&self) -> PathBuf {
        self.directory.path().join("agent-1.toml")
    }

    pub fn dir(&self) -> PathBuf {
        self.directory.path().join("agents/1")
    }

    pub fn register_command(&self, server: &FakeServer, token: &str, labels: &[&str]) -> Command {
        let mut command = agent();
        command
            .arg("register")
            .arg("--config")
            .arg(self.config())
            .args(["--url", &server.url(), "--token", token, "--name", NAME])
            .arg("--dir")
            .arg(self.dir());
        for label in labels {
            command.args(["--label", label]);
        }
        command
    }

    pub async fn registered(server: &mut FakeServer, labels: &[&str]) -> Home {
        let home = Home::empty();
        let finished = output(home.register_command(server, TOKEN, labels)).await;
        assert!(finished.status.success(), "{finished:#?}");
        server.registration().await;
        home
    }

    pub fn start(&self) -> Running {
        let mut command = agent();
        command
            .arg("run")
            .arg("--config")
            .arg(self.config())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("bosun-agent starts");
        let (sender, lines) = mpsc::unbounded_channel();
        for stream in [
            Box::new(child.stdout.take().expect("a piped stdout"))
                as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            Box::new(child.stderr.take().expect("a piped stderr")),
        ] {
            let sender = sender.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                    if sender.send(value).is_err() {
                        break;
                    }
                }
            });
        }
        Running {
            child,
            lines,
            seen: Vec::new(),
        }
    }
}

pub struct Running {
    child: Child,
    lines: UnboundedReceiver<Value>,
    seen: Vec<Value>,
}

impl Running {
    fn pid(&self) -> Pid {
        self.child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw)
            .expect("a running agent")
    }

    pub fn signal(&self, signal: Signal) {
        kill_process(self.pid(), signal).expect("the signal reaches the agent");
    }

    pub async fn logged(&mut self, message: &str) -> Value {
        let found = timeout(WAIT, async {
            loop {
                let line = self.lines.recv().await?;
                self.seen.push(line.clone());
                if line["message"] == message {
                    return Some(line);
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the agent did not log `{message}` within {WAIT:?}"));
        found.unwrap_or_else(|| panic!("the agent ended its output before it logged `{message}`"))
    }

    pub async fn stop(self) -> Vec<Value> {
        self.signal(Signal::TERM);
        self.exited().await
    }

    pub async fn exited(mut self) -> Vec<Value> {
        let status = timeout(WAIT, self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("the agent did not stop within {WAIT:?}"))
            .expect("the agent exits");
        let logs = self.logs();
        assert!(
            status.success(),
            "the agent stopped with {status}: {logs:#?}"
        );
        logs
    }

    pub async fn kill(mut self) {
        self.child.start_kill().expect("SIGKILL reaches the agent");
        let _ = self.child.wait().await;
    }

    fn logs(&mut self) -> Vec<Value> {
        let mut logs = std::mem::take(&mut self.seen);
        while let Ok(line) = self.lines.try_recv() {
            logs.push(line);
        }
        logs
    }
}

const SPAN: Span = Span {
    line: 1,
    column: 1,
    end_line: 1,
    end_column: 1,
};

pub const JOB_ID: &str = "0192f3a4-5b6c-7d8e-9f01-000000000009";

pub fn step(script: &str) -> Step {
    Step {
        key: None,
        name: None,
        r#if: None,
        action: Action::Run {
            script: Template::text(script),
        },
        shell: None,
        working_directory: None,
        with: BTreeMap::new(),
        env: BTreeMap::new(),
        continue_on_error: Evaluable::Literal(false),
        timeout_minutes: None,
        span: SPAN,
    }
}

pub fn documents(steps: Vec<Step>) -> Documents {
    let object = |value: Value| value.as_object().cloned().expect("an object");
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
            kind: JobKind::Steps(steps),
            span: SPAN,
        },
        contexts: Contexts {
            github: object(json!({
                "event_name": "workflow_dispatch",
                "job": "build",
                "workspace": "",
                "event": { "inputs": null, "ref": null, "workflow": "ci" },
            })),
            needs: Map::new(),
            strategy: Value::Null,
            matrix: Value::Null,
            inputs: Map::new(),
            vars: Map::new(),
            secrets: Map::new(),
            bosun: object(json!({
                "url": "https://ci.example.org",
                "run_url": "https://ci.example.org/runs/0192f3a4-5b6c-7d8e-9f01-000000000007",
                "run_id": "0192f3a4-5b6c-7d8e-9f01-000000000007",
                "attempt": 1,
                "draft": false,
                "project": "globalart/keel",
                "trigger": { "kind": "user", "login": "roman" },
            })),
        },
        evaluated: Evaluated {
            env: BTreeMap::new(),
            shell: None,
            working_directory: None,
            timeout_minutes: 360,
            continue_on_error: false,
        },
    }
}

pub fn assignment_of(documents: &Documents) -> Assignment {
    let mut assignment = Assignment {
        run_id: Uuid::now_v7().to_string(),
        attempt_id: Uuid::now_v7().to_string(),
        job_id: JOB_ID.to_owned(),
        execution_id: Uuid::now_v7().to_string(),
        job_key: "build".to_owned(),
        job_name: "build".to_owned(),
        organisation: "globalart".to_owned(),
        project: "keel".to_owned(),
        ..Assignment::default()
    };
    documents
        .write(&mut assignment)
        .expect("the documents are JSON");
    assignment
}

pub fn assignment(steps: Vec<Step>) -> Assignment {
    assignment_of(&documents(steps))
}

pub fn text(logs: &[&Log]) -> Vec<String> {
    let mut lines = Vec::new();
    for log in logs {
        let chunk = String::from_utf8(log.lines.clone()).expect("UTF-8 lines");
        assert!(
            chunk.ends_with('\n'),
            "a chunk ends in the middle of a line: {chunk:?}"
        );
        for line in chunk.lines() {
            let (stamp, text) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("a line without a timestamp: {line:?}"));
            assert!(
                is_timestamp(stamp),
                "`{stamp}` is not an RFC 3339 time with nanoseconds"
            );
            lines.push(text.to_owned());
        }
    }
    lines
}

fn is_timestamp(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    let shape = "dddd-dd-ddTdd:dd:dd.dddddddddZ";
    bytes.len() == shape.len()
        && shape
            .bytes()
            .zip(bytes)
            .all(|(expected, found)| match expected {
                b'd' => found.is_ascii_digit(),
                other => other == *found,
            })
}

pub fn pid(lines: &[String], label: &str) -> Pid {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{label} ")))
        .and_then(|pid| pid.trim().parse().ok())
        .and_then(Pid::from_raw)
        .unwrap_or_else(|| panic!("no line `{label} <pid>` in {lines:?}"))
}

pub async fn gone(pid: Pid) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if test_kill_process(pid).is_err() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(20)).await;
    }
}

pub fn workspace(home: &Home) -> PathBuf {
    std::fs::canonicalize(home.dir())
        .expect("the agent directory exists")
        .join(Path::new("work/globalart.keel/keel"))
}
