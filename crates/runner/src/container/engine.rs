use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

const PROGRAM: &str = "podman";

struct Group(Option<rustix::process::Pid>);

impl Group {
    fn of(child: &tokio::process::Child) -> Group {
        Group(
            child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(rustix::process::Pid::from_raw),
        )
    }

    fn release(&mut self) {
        self.0 = None;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, PartialEq, Eq)]
pub struct Call {
    pub arguments: Vec<OsString>,
    pub stdin: Option<Vec<u8>>,
}

impl Call {
    pub fn new<I, S>(arguments: I) -> Call
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Call {
            arguments: arguments.into_iter().map(Into::into).collect(),
            stdin: None,
        }
    }

    pub fn with_stdin(mut self, stdin: Vec<u8>) -> Call {
        self.stdin = Some(stdin);
        self
    }
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("arguments", &self.arguments)
            .field("stdin", &self.stdin.as_ref().map(|input| input.len()))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn failure(&self) -> String {
        let said = self.stderr.trim();
        let said = if said.is_empty() {
            self.stdout.trim()
        } else {
            said
        };
        match (self.code, said.is_empty()) {
            (Some(code), true) => format!("exited with {code}"),
            (Some(code), false) => format!("exited with {code}: {said}"),
            (None, true) => "was ended by a signal".to_owned(),
            (None, false) => format!("was ended by a signal: {said}"),
        }
    }
}

pub(crate) async fn within(
    engine: &Arc<dyn Engine>,
    call: Call,
    limit: Duration,
) -> io::Result<Outcome> {
    match timeout(limit, engine.run(call)).await {
        Ok(outcome) => outcome,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("podman did not answer within {} seconds", limit.as_secs()),
        )),
    }
}

pub trait Engine: Send + Sync + fmt::Debug {
    fn run(&self, call: Call) -> Pending<'_, io::Result<Outcome>>;

    fn exec(&self, container: &str, launcher: &str) -> (OsString, Vec<OsString>);
}

#[derive(Debug, Clone)]
pub struct Podman {
    program: OsString,
}

impl Podman {
    pub fn new() -> Podman {
        Podman::at(PROGRAM)
    }

    pub fn at(program: impl Into<OsString>) -> Podman {
        Podman {
            program: program.into(),
        }
    }
}

impl Default for Podman {
    fn default() -> Podman {
        Podman::new()
    }
}

impl Engine for Podman {
    fn run(&self, call: Call) -> Pending<'_, io::Result<Outcome>> {
        Box::pin(async move {
            let Call { arguments, stdin } = call;
            let mut command = Command::new(&self.program);
            command
                .args(&arguments)
                .stdin(if stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .kill_on_drop(true);
            let mut child = command.spawn()?;
            let mut group = Group::of(&child);
            if let Some(input) = stdin
                && let Some(mut pipe) = child.stdin.take()
            {
                match pipe.write_all(&input).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
                    Err(error) => return Err(error),
                }
                drop(pipe);
            }
            let output = child.wait_with_output().await?;
            group.release();
            Ok(Outcome {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }

    fn exec(&self, container: &str, launcher: &str) -> (OsString, Vec<OsString>) {
        (
            self.program.clone(),
            ["exec", container, "sh", launcher]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{Call, Engine, Podman, within};

    fn alive(pid: &str) -> bool {
        let listed = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .expect("ps runs");
        let state = String::from_utf8_lossy(&listed.stdout);
        !state.trim().is_empty() && !state.trim().starts_with('Z')
    }

    #[tokio::test]
    async fn a_call_runs_the_program_with_its_arguments_and_its_input_and_answers_what_it_printed()
    {
        let echoed = Podman::at("echo")
            .run(Call::new(["a", "b"]))
            .await
            .expect("echo runs");
        assert_eq!((echoed.code, echoed.stdout.as_str()), (Some(0), "a b\n"));
        let piped = Podman::at("cat")
            .run(Call::new(Vec::<String>::new()).with_stdin(b"secret".to_vec()))
            .await
            .expect("cat runs");
        assert_eq!(piped.stdout, "secret");
        let failed = Podman::at("sh")
            .run(Call::new(["-c", "echo said >&2; exit 3"]))
            .await
            .expect("sh runs");
        assert_eq!(failed.code, Some(3));
        assert_eq!(failed.failure(), "exited with 3: said");
        assert!(
            Podman::at("/a/program/that/is/not/there")
                .run(Call::new(["x"]))
                .await
                .is_err()
        );
    }

    #[test]
    fn a_step_is_executed_in_the_container_through_sh_and_a_call_never_prints_its_input() {
        let (program, arguments) = Podman::new().exec("bosun-job-1", "/__w/_temp/step-0.launch");
        assert_eq!(program, OsString::from("podman"));
        assert_eq!(
            arguments,
            ["exec", "bosun-job-1", "sh", "/__w/_temp/step-0.launch"].map(OsString::from)
        );
        let call = Call::new(["login"]).with_stdin(b"hunter2".to_vec());
        assert!(!format!("{call:?}").contains("hunter2"));
    }

    #[tokio::test]
    async fn a_call_that_runs_out_of_time_takes_the_processes_it_started_with_it() {
        let directory = tempfile::tempdir().expect("a directory");
        let pidfile = directory.path().join("pid");
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.display());
        let engine: Arc<dyn Engine> = Arc::new(Podman::at("sh"));
        let outcome = within(
            &engine,
            Call::new(["-c", script.as_str()]),
            Duration::from_secs(3),
        )
        .await;
        assert_eq!(
            outcome.expect_err("a timeout").kind(),
            std::io::ErrorKind::TimedOut
        );
        let pid = std::fs::read_to_string(&pidfile).expect("the pid of the grandchild");
        let pid = pid.trim();
        assert!(!pid.is_empty());
        let deadline = Instant::now() + Duration::from_secs(20);
        while alive(pid) {
            assert!(Instant::now() < deadline, "process {pid} survived");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
