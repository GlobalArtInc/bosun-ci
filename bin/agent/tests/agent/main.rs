mod cancel;
mod jobs;
mod reconnect;
mod register;
mod server;
mod spool;
mod stop;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use bosun_proto::agent::v1::segment::Segment as Text;
use bosun_proto::agent::v1::step::Action;
use bosun_proto::agent::v1::{Assignment, Log, Segment, Step, Template};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::Value;
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

pub fn template(text: &str) -> Template {
    Template {
        segments: vec![Segment {
            segment: Some(Text::Text(text.to_owned())),
        }],
    }
}

pub fn step(index: u32, script: &str) -> Step {
    Step {
        index,
        key: None,
        name: None,
        action: Some(Action::Run(template(script))),
        shell: None,
        working_directory: None,
        env: Default::default(),
    }
}

pub fn assignment(steps: Vec<Step>) -> Assignment {
    Assignment {
        run_id: Uuid::now_v7().to_string(),
        attempt_id: Uuid::now_v7().to_string(),
        job_id: Uuid::now_v7().to_string(),
        execution_id: Uuid::now_v7().to_string(),
        job_key: "build".to_owned(),
        job_name: "build".to_owned(),
        env: Default::default(),
        steps,
        organisation: "globalart".to_owned(),
        project: "keel".to_owned(),
    }
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
