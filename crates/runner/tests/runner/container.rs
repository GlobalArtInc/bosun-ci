use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bosun_runner::container::{
    Call, ContainerJob, Credentials, Engine, Libc, Mounts, Outcome, Pending, Provision, PullPolicy,
    Recovery,
};
use bosun_runner::{Cleanup, Job, NodeAction};
use bosun_types::ir::{
    Action, ActionMetadata, ActionReference, Container, NodeRuns, NodeRuntime, Runs,
};
use bosun_types::status::Conclusion;
use rustix::process::{Pid, Signal, kill_process_group};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{mpsc, watch};

use crate::{Finished, job, run, run_cancelled_on, step, template};

const PASSWORD: &str = "hunter2-of-the-registry";

#[derive(Default)]
struct Behaviour {
    absent: bool,
    fail: &'static str,
    fail_first: &'static str,
    hang: &'static str,
    said: &'static str,
    libc: &'static str,
}

type Labels = BTreeMap<String, String>;

#[derive(Default)]
struct State {
    calls: Vec<(Vec<String>, Option<String>)>,
    containers: BTreeMap<String, Labels>,
    networks: BTreeMap<String, Labels>,
    volumes: BTreeMap<String, Labels>,
    launchers: Vec<String>,
    linked_sources: Vec<String>,
    created: u32,
    failed_first: bool,
}

#[derive(Debug)]
struct Fake {
    work: PathBuf,
    externals: PathBuf,
    tools: PathBuf,
    pids: PathBuf,
    behaviour: Behaviour,
    state: Mutex<State>,
}

impl std::fmt::Debug for Behaviour {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Behaviour")
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("State")
    }
}

fn flag_values<'a>(words: &'a [String], name: &str) -> Vec<&'a str> {
    let attached = format!("{name}=");
    let mut found = Vec::new();
    let mut position = 0;
    while position < words.len() {
        if words[position] == name {
            if let Some(value) = words.get(position + 1) {
                found.push(value.as_str());
            }
            position += 2;
        } else {
            if let Some(value) = words[position].strip_prefix(&attached) {
                found.push(value);
            }
            position += 1;
        }
    }
    found
}

fn flag_value<'a>(words: &'a [String], name: &str) -> &'a str {
    let found = flag_values(words, name);
    assert_eq!(found.len(), 1, "{name} is not given once in {words:?}");
    found[0]
}

fn labels_of(words: &[String]) -> Labels {
    flag_values(words, "--label")
        .into_iter()
        .filter_map(|label| label.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn selected(words: &[String], held: &Labels) -> bool {
    flag_values(words, "--filter")
        .into_iter()
        .all(|filter| match filter.strip_prefix("label=") {
            Some(label) => match label.split_once('=') {
                Some((key, value)) => held.get(key).is_some_and(|found| found == value),
                None => held.contains_key(label),
            },
            None => false,
        })
}

fn labelled(execution: &str, agent: Option<&str>) -> Labels {
    let mut labels = Labels::new();
    labels.insert("bosun.execution".to_owned(), execution.to_owned());
    if let Some(agent) = agent {
        labels.insert("bosun.agent".to_owned(), agent.to_owned());
    }
    labels
}

fn outcome(code: i32, stdout: &str, stderr: &str) -> io::Result<Outcome> {
    Ok(Outcome {
        code: Some(code),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    })
}

impl Fake {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn kinds(&self) -> Vec<String> {
        self.state()
            .calls
            .iter()
            .map(|(words, _)| match words[0].as_str() {
                "image" | "network" | "volume" => words[..2].join(" "),
                "rm" if words.contains(&"--ignore".to_owned()) => "rm --ignore".to_owned(),
                first => first.to_owned(),
            })
            .collect()
    }

    fn calls(&self, first: &str) -> Vec<Vec<String>> {
        self.state()
            .calls
            .iter()
            .filter(|(words, _)| words[0] == first)
            .map(|(words, _)| words.clone())
            .collect()
    }

    fn leaked(&self) -> Vec<String> {
        let state = self.state();
        state
            .containers
            .keys()
            .chain(state.networks.keys())
            .cloned()
            .collect()
    }

    fn host(&self, container: &str) -> PathBuf {
        let temp = self.work.join("_temp");
        for (prefix, host) in [
            ("/github/home", temp.join("_github_home")),
            ("/github/workflow", temp.join("_github_workflow")),
            ("/__w", self.work.clone()),
            ("/__e", self.externals.clone()),
            ("/__t", self.tools.clone()),
        ] {
            if let Some(rest) = container.strip_prefix(prefix) {
                return PathBuf::from(format!("{}{rest}", host.display()));
            }
        }
        PathBuf::from(container)
    }

    fn signal(&self, signal: Signal) {
        let pids: Vec<i32> = fs::read_to_string(&self.pids)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        for pid in pids {
            if let Some(pid) = Pid::from_raw(pid) {
                let _ = kill_process_group(pid, signal);
            }
        }
    }
}

impl Engine for Fake {
    fn run(&self, call: Call) -> Pending<'_, io::Result<Outcome>> {
        Box::pin(async move {
            let words: Vec<String> = call
                .arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            let stdin = call
                .stdin
                .as_ref()
                .map(|input| String::from_utf8_lossy(input).into_owned());
            self.state().calls.push((words.clone(), stdin));
            let first = words[0].as_str();
            if self.behaviour.hang == first {
                std::future::pending::<()>().await;
            }
            if self.behaviour.fail == first {
                return outcome(125, "", self.behaviour.said);
            }
            if self.behaviour.fail_first == first
                && !std::mem::replace(&mut self.state().failed_first, true)
            {
                return outcome(125, "", self.behaviour.said);
            }
            let mut state = self.state();
            match (first, words.get(1).map(String::as_str)) {
                ("image", Some("exists")) => outcome(i32::from(self.behaviour.absent), "", ""),
                ("network", Some("create")) => {
                    let name = words.last().cloned().unwrap_or_default();
                    state.networks.insert(name, labels_of(&words));
                    outcome(0, "", "")
                }
                ("create", _) => {
                    for source in flag_values(&words, "--volume") {
                        let host = source.split(':').next().unwrap_or_default();
                        if fs::symlink_metadata(host).is_ok_and(|found| found.is_symlink()) {
                            state.linked_sources.push(host.to_owned());
                        }
                    }
                    let name = flag_value(&words, "--name").to_owned();
                    state.containers.insert(name, labels_of(&words));
                    state.created += 1;
                    outcome(0, &format!("{:064x}\n", state.created), "")
                }
                ("exec", _) => {
                    let script = words.last().cloned().unwrap_or_default();
                    drop(state);
                    if let Some(signal) = script
                        .strip_prefix("kill -")
                        .and_then(|rest| rest.strip_suffix(" -1"))
                    {
                        self.signal(match signal {
                            "INT" => Signal::INT,
                            "TERM" => Signal::TERM,
                            _ => Signal::KILL,
                        });
                        return outcome(0, "", "");
                    }
                    outcome(0, &format!("{}\n", self.behaviour.libc), "")
                }
                ("rm", _) => {
                    for name in words
                        .iter()
                        .skip(1)
                        .filter(|word| !word.starts_with('-') && *word != "0")
                    {
                        state.containers.remove(name);
                    }
                    outcome(0, "", "")
                }
                ("ps", _) => {
                    let found: Vec<String> = state
                        .containers
                        .iter()
                        .filter(|(_, labels)| selected(&words, labels))
                        .map(|(name, _)| name.clone())
                        .collect();
                    outcome(0, &found.join("\n"), "")
                }
                ("network", Some("ls")) => {
                    let found: Vec<String> = state
                        .networks
                        .iter()
                        .filter(|(_, labels)| selected(&words, labels))
                        .map(|(name, _)| name.clone())
                        .collect();
                    outcome(0, &found.join("\n"), "")
                }
                ("volume", Some("ls")) => {
                    let found: Vec<String> = state
                        .volumes
                        .iter()
                        .filter(|(_, labels)| selected(&words, labels))
                        .map(|(name, _)| name.clone())
                        .collect();
                    outcome(0, &found.join("\n"), "")
                }
                ("volume", Some("rm")) => {
                    for name in words.iter().skip(3) {
                        state.volumes.remove(name);
                    }
                    outcome(0, "", "")
                }
                ("network", Some("rm")) => {
                    for name in words.iter().skip(3) {
                        state.networks.remove(name);
                    }
                    outcome(0, "", "")
                }
                _ => outcome(0, "", ""),
            }
        })
    }

    fn exec(&self, container: &str, launcher: &str) -> (OsString, Vec<OsString>) {
        self.state().calls.push((
            vec![
                "exec-step".to_owned(),
                container.to_owned(),
                launcher.to_owned(),
            ],
            None,
        ));
        let text = fs::read_to_string(self.host(launcher)).expect("the launcher exists");
        self.state().launchers.push(text.clone());
        let temp = self.work.join("_temp");
        let rewritten = text
            .replace("/github/home", &temp.join("_github_home").to_string_lossy())
            .replace(
                "/github/workflow",
                &temp.join("_github_workflow").to_string_lossy(),
            )
            .replace("/__w", &self.work.to_string_lossy())
            .replace("/__e", &self.externals.to_string_lossy())
            .replace("/__t", &self.tools.to_string_lossy());
        let file = self.host(launcher).with_extension("run");
        fs::write(&file, rewritten).expect("a launcher to run");
        (
            "sh".into(),
            vec![
                "-c".into(),
                format!(
                    "echo $$ >> '{}'; exec sh '{}'",
                    self.pids.display(),
                    file.display()
                )
                .into(),
            ],
        )
    }
}

#[derive(Debug)]
struct Nodes {
    directory: PathBuf,
    asked: Mutex<Vec<Libc>>,
}

impl Provision for Nodes {
    fn nodes(&self, libc: Libc) -> Pending<'_, Result<Vec<PathBuf>, String>> {
        Box::pin(async move {
            self.asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(libc);
            Ok(vec![self.directory.join("node24-musl/bin/node")])
        })
    }
}

struct Fixture {
    _directory: TempDir,
    base: PathBuf,
    fake: Arc<Fake>,
}

impl Fixture {
    fn new(behaviour: Behaviour) -> Fixture {
        let directory = tempfile::tempdir().expect("a directory");
        let base = fs::canonicalize(directory.path()).expect("a path");
        let fake = Arc::new(Fake {
            work: base.join("work"),
            externals: base.join("externals"),
            tools: base.join("tools"),
            pids: base.join("pids"),
            behaviour,
            state: Mutex::new(State::default()),
        });
        Fixture {
            _directory: directory,
            base,
            fake,
        }
    }

    fn job(&self, steps: Vec<bosun_types::ir::Step>) -> Job {
        let mut job = job(&self.base, steps);
        let engine: Arc<dyn Engine> = Arc::clone(&self.fake) as Arc<dyn Engine>;
        job.container = Some(ContainerJob {
            image: "alpine:3".to_owned(),
            credentials: None,
            allow_privileged: false,
            untrusted: false,
            pull_policy: PullPolicy::Missing,
            engine,
            mounts: Mounts {
                work: self.base.join("work"),
                externals: self.base.join("externals"),
                tools: self.base.join("tools"),
            },
            state: self.base.join("state"),
            provision: None,
        });
        job.template.container = Some(container(None, &[]));
        job.minute = Duration::from_millis(300);
        job
    }
}

fn container(options: Option<&str>, volumes: &[&str]) -> Container {
    Container {
        image: template("alpine:3"),
        credentials: None,
        env: BTreeMap::new(),
        ports: Vec::new(),
        volumes: volumes.iter().map(|volume| template(volume)).collect(),
        options: options.map(template),
    }
}

async fn finished(job: Job, cancel_on: Option<&str>) -> Finished {
    run_cancelled_on(job, cancel_on).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_problem_matcher_of_an_action_in_the_container_is_read_from_the_directory_of_the_actions()
{
    let fixture = Fixture::new(Behaviour::default());
    let actions = fixture.base.join("work/_actions/actions/checkout/abc/dist");
    fs::create_dir_all(&actions).expect("an action directory");
    fs::write(
        actions.join("problem-matcher.json"),
        r#"{"problemMatcher": [{"owner": "checkout-git", "pattern": [{"regexp": "^(.*)$", "message": 1}]}]}"#,
    )
    .expect("a matcher");
    let job = fixture.job(vec![
        step(
            "w=w; echo \"::add-matcher::/__${w}/_actions/actions/checkout/abc/dist/problem-matcher.json\"",
        ),
        step("w=w; echo \"::add-matcher::/__${w}/_temp/other.json\""),
    ]);
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Failure,
        "{:#?}",
        result.events
    );
    assert!(
        result
            .lines(0)
            .contains(&"added the problem matchers `checkout-git`".to_owned()),
        "{:#?}",
        result.events
    );
    let (outcome, _, _) = result.ended(0).expect("the first step ended");
    assert_eq!(outcome, Conclusion::Success);
    let (outcome, _, _) = result.ended(1).expect("the second step ended");
    assert_eq!(outcome, Conclusion::Failure);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_container_runs_its_steps_through_exec_in_the_workspace_of_the_container() {
    let fixture = Fixture::new(Behaviour::default());
    let mut job = fixture.job(vec![step("echo hello"), step("exit 0")]);
    job.evaluated.needs_podman = true;
    job.template.outputs = [
        ("id".to_owned(), template("${{ job.container.id }}")),
        (
            "network".to_owned(),
            template("${{ job.container.network }}"),
        ),
    ]
    .into();
    let id = job.id.clone();
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Success,
        "{:#?}",
        result.events
    );
    assert_eq!(result.lines(0), vec!["hello"]);
    assert_eq!(result.outputs["id"], format!("{:064x}", 1));
    assert_eq!(result.outputs["network"], format!("bosun-{id}"));
    assert_eq!(
        fixture.fake.kinds(),
        [
            "image exists",
            "network create",
            "create",
            "start",
            "exec-step",
            "exec-step",
            "rm --ignore",
            "ps",
            "volume ls",
            "network ls",
            "network rm"
        ]
    );
    let created = &fixture.fake.calls("create")[0];
    let joined = created.join(" ");
    let work = fixture.base.join("work");
    for expected in [
        format!("--name bosun-job-{id}"),
        format!("--label bosun.execution={id}"),
        format!("--network=bosun-{id}"),
        "--pull=never".to_owned(),
        "--entrypoint [\"tail\"]".to_owned(),
        format!("--volume={}:/__w", work.display()),
        format!("--volume={}:/__w/_temp", work.join("_temp").display()),
        format!(
            "--volume={}:/__e:ro",
            fixture.base.join("externals").display()
        ),
        format!("--volume={}:/__t", fixture.base.join("tools").display()),
        format!(
            "--volume={}:/github/home",
            work.join("_temp/_github_home").display()
        ),
        format!(
            "--volume={}:/github/workflow",
            work.join("_temp/_github_workflow").display()
        ),
        "--workdir=/__w/globalart.keel/keel".to_owned(),
    ] {
        assert!(joined.contains(&expected), "{expected} is not in {joined}");
    }
    assert_eq!(
        &created[created.len() - 4..],
        ["--", "alpine:3", "-f", "/dev/null"]
    );
    let labels = labels_of(created);
    assert_eq!(labels["bosun.execution"], id);
    assert_eq!(labels["bosun.agent"], owner(&fixture));
    assert_eq!(flag_value(created, "--name"), format!("bosun-job-{id}"));
    assert_eq!(flag_value(created, "--network"), format!("bosun-{id}"));
    assert_eq!(flag_value(created, "--workdir"), "/__w/globalart.keel/keel");
    let networks = fixture.fake.calls("network");
    assert_eq!(labels_of(&networks[0]), labels);
    let launchers = fixture.fake.state().launchers.clone();
    let launcher = &launchers[0];
    for expected in [
        "cd -- '/__w/globalart.keel/keel'",
        "export GITHUB_WORKSPACE='/__w/globalart.keel/keel'",
        "export RUNNER_TEMP='/__w/_temp'",
        "export RUNNER_TOOL_CACHE='/__t'",
        "export RUNNER_WORKSPACE='/__w/globalart.keel'",
        "export GITHUB_EVENT_PATH='/github/workflow/event.json'",
        "export HOME='/github/home'",
        "export GITHUB_ENV='/__w/_temp/_runner_file_commands/set_env_",
    ] {
        assert!(
            launcher.contains(expected),
            "{expected} is not in {launcher}"
        );
    }
    assert!(
        !launcher.contains(&fixture.base.to_string_lossy().into_owned()),
        "a path of the host reached the container: {launcher}"
    );
    assert!(fixture.fake.leaked().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_container_step_writes_to_the_file_commands_reaches_the_next_step_and_the_outputs() {
    let fixture = Fixture::new(Behaviour::default());
    let mut first = step(
        "echo \"FROM_ENV=one\" >> \"$GITHUB_ENV\"\n\
         echo \"out=two\" >> \"$GITHUB_OUTPUT\"\n\
         mkdir -p \"$RUNNER_TEMP/bin\"\n\
         printf '#!/bin/sh\\necho added-tool\\n' > \"$RUNNER_TEMP/bin/tool\"\n\
         chmod +x \"$RUNNER_TEMP/bin/tool\"\n\
         echo \"$RUNNER_TEMP/bin\" >> \"$GITHUB_PATH\"\n",
    );
    first.key = Some("one".parse().expect("a step id"));
    let mut job = fixture.job(vec![first, step("echo \"$FROM_ENV\"; tool")]);
    job.template.outputs = [("x".to_owned(), template("${{ steps.one.outputs.out }}"))].into();
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Success,
        "{:#?}",
        result.events
    );
    assert_eq!(result.lines(1), vec!["one", "added-tool"]);
    assert_eq!(result.outputs["x"], "two");
}

struct Exit {
    name: &'static str,
    behaviour: Behaviour,
    steps: &'static [&'static str],
    cancel_on: Option<&'static str>,
    job_timeout: Option<u32>,
    expected: Conclusion,
}

#[tokio::test(flavor = "multi_thread")]
async fn teardown_removes_the_container_the_network_and_what_is_labelled_on_every_way_a_job_ends() {
    for exit in [
        Exit {
            name: "success",
            behaviour: Behaviour::default(),
            steps: &["true"],
            cancel_on: None,
            job_timeout: None,
            expected: Conclusion::Success,
        },
        Exit {
            name: "a failing step",
            behaviour: Behaviour::default(),
            steps: &["exit 3", "true"],
            cancel_on: None,
            job_timeout: None,
            expected: Conclusion::Failure,
        },
        Exit {
            name: "a cancel",
            behaviour: Behaviour::default(),
            steps: &["echo ready; sleep 300"],
            cancel_on: Some("ready"),
            job_timeout: None,
            expected: Conclusion::Cancelled,
        },
        Exit {
            name: "the timeout of the job",
            behaviour: Behaviour::default(),
            steps: &["sleep 300"],
            cancel_on: None,
            job_timeout: Some(1),
            expected: Conclusion::TimedOut,
        },
        Exit {
            name: "a container that cannot be created",
            behaviour: Behaviour {
                fail: "create",
                said: "Error: no space left",
                ..Behaviour::default()
            },
            steps: &["true"],
            cancel_on: None,
            job_timeout: None,
            expected: Conclusion::Failure,
        },
        Exit {
            name: "a container that cannot be started",
            behaviour: Behaviour {
                fail: "start",
                said: "Error: crun failed",
                ..Behaviour::default()
            },
            steps: &["true"],
            cancel_on: None,
            job_timeout: None,
            expected: Conclusion::Failure,
        },
        Exit {
            name: "an image that is not on the host with the pull policy never",
            behaviour: Behaviour {
                absent: true,
                ..Behaviour::default()
            },
            steps: &["true"],
            cancel_on: None,
            job_timeout: None,
            expected: Conclusion::Failure,
        },
    ] {
        let fixture = Fixture::new(exit.behaviour);
        let mut job = fixture.job(
            exit.steps
                .iter()
                .map(|script| step(script))
                .collect::<Vec<_>>(),
        );
        if exit.name.contains("never") {
            job.container.as_mut().expect("a container").pull_policy = PullPolicy::Never;
        }
        if let Some(minutes) = exit.job_timeout {
            job.evaluated.timeout_minutes = minutes;
        }
        let result = finished(job, exit.cancel_on).await;
        assert_eq!(
            result.conclusion, exit.expected,
            "{}: {:#?}",
            exit.name, result.events
        );
        assert!(
            fixture.fake.leaked().is_empty(),
            "{}: {:?} is left",
            exit.name,
            fixture.fake.leaked()
        );
        let kinds = fixture.fake.kinds();
        assert!(
            kinds.contains(&"rm --ignore".to_owned()),
            "{}: {kinds:?}",
            exit.name
        );
        if exit.name.contains("never") {
            assert!(!kinds.contains(&"pull".to_owned()), "{kinds:?}");
            assert!(!kinds.contains(&"network create".to_owned()), "{kinds:?}");
            assert!(
                result
                    .lines(0)
                    .iter()
                    .any(|line| line.contains("the image `alpine:3` is not on this host")),
                "{:?}",
                result.lines(0)
            );
        }
        if exit.name.contains("cannot be") {
            assert!(
                result
                    .lines(0)
                    .iter()
                    .any(|line| line.contains("podman exited with 125")),
                "{:?}",
                result.lines(0)
            );
            assert!(
                !fixture.fake.kinds().contains(&"exec-step".to_owned()),
                "{}: a step ran",
                exit.name
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_while_the_container_is_being_created_ends_the_job_and_removes_the_network() {
    let fixture = Fixture::new(Behaviour {
        hang: "create",
        ..Behaviour::default()
    });
    let job = fixture.job(vec![step("true")]);
    let (cancel, cancelled) = watch::channel(false);
    let (events, mut received) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move {
        let tracking = bosun_runner::Tracking::probe();
        bosun_runner::run(job, &tracking, events, cancelled).await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.send_replace(true);
    let completed = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .expect("the job ended")
        .expect("the job ran");
    assert_eq!(completed.conclusion, Conclusion::Cancelled);
    assert!(
        fixture.fake.leaked().is_empty(),
        "{:?}",
        fixture.fake.leaked()
    );
    while let Ok(event) = received.try_recv() {
        assert!(
            !matches!(event, bosun_runner::Event::StepStarted { .. }),
            "{event:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flag_that_weakens_isolation_fails_the_job_before_its_first_step_unless_the_pool_allows_it()
 {
    const OPTION: &str = "weakens the isolation of the container, and the pool does not set \
                          `allow_privileged`";
    for (options, volumes, expected) in [
        (
            Some("--privileged"),
            &[][..],
            format!("the option `--privileged` of `container` {OPTION}"),
        ),
        (
            Some("--cap-add NET_ADMIN"),
            &[][..],
            format!("the option `--cap-add NET_ADMIN` of `container` {OPTION}"),
        ),
        (
            Some("--device=/dev/fuse"),
            &[][..],
            format!("the option `--device=/dev/fuse` of `container` {OPTION}"),
        ),
        (
            Some("--pid=host"),
            &[][..],
            "the option `--pid=host` of `container` shares a namespace with the host or with \
             another container, and the pool does not set `allow_privileged`"
                .to_owned(),
        ),
        (
            Some("--network host"),
            &[][..],
            "the option `--network host` of `container` is chosen by the agent, and the pool \
             does not set `allow_privileged`"
                .to_owned(),
        ),
        (
            None,
            &["/etc:/etc"][..],
            "the volume `/etc:/etc` of `container` mounts `/etc`, which is outside the \
             workspace, and the pool does not set `allow_privileged`"
                .to_owned(),
        ),
    ] {
        let named = expected.clone();
        let fixture = Fixture::new(Behaviour::default());
        let mut job = fixture.job(vec![step("echo ran")]);
        job.template.container = Some(container(options, volumes));
        let refused = finished(job, None).await;
        assert_eq!(refused.conclusion, Conclusion::Failure, "{named}");
        assert_eq!(
            refused.lines(0),
            vec![format!(
                "the job could not be prepared on this agent: {expected}"
            )]
        );
        let kinds = fixture.fake.kinds();
        assert!(
            !kinds
                .iter()
                .any(|kind| ["create", "network create", "image exists", "start"]
                    .contains(&kind.as_str())),
            "{named}: {kinds:?}"
        );
        let allowed = Fixture::new(Behaviour::default());
        let mut job = allowed.job(vec![step("echo ran")]);
        job.template.container = Some(container(options, volumes));
        job.container
            .as_mut()
            .expect("a container")
            .allow_privileged = true;
        let started = finished(job, None).await;
        assert_eq!(
            started.conclusion,
            Conclusion::Success,
            "{named}: {:#?}",
            started.events
        );
        assert_eq!(started.lines(0), vec!["ran"]);
        assert!(allowed.fake.leaked().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_password_goes_to_podman_on_stdin_only_and_never_reaches_the_log() {
    let fixture = Fixture::new(Behaviour {
        absent: true,
        fail: "pull",
        said: "Error: unauthorized: hunter2-of-the-registry was refused by ghcr.io",
        ..Behaviour::default()
    });
    let mut job = fixture.job(vec![step("true")]);
    let id = job.id.clone();
    {
        let container = job.container.as_mut().expect("a container");
        container.image = "ghcr.io/acme/builder:1".to_owned();
        container.credentials = Some(Credentials {
            username: "ci".to_owned(),
            password: PASSWORD.to_owned(),
        });
        assert!(!format!("{container:?}").contains(PASSWORD));
    }
    let result = finished(job, None).await;
    assert_eq!(result.conclusion, Conclusion::Failure);
    let calls = fixture.fake.state().calls.clone();
    for (words, _) in &calls {
        assert!(
            words.iter().all(|word| !word.contains(PASSWORD)),
            "the password is in the arguments {words:?}"
        );
    }
    let login = calls
        .iter()
        .find(|(words, _)| words[0] == "login")
        .expect("a login");
    assert_eq!(login.1.as_deref(), Some(PASSWORD));
    assert!(login.0.contains(&"--username=ci".to_owned()));
    assert!(login.0.contains(&"--password-stdin".to_owned()));
    assert!(login.0.contains(&"ghcr.io".to_owned()));
    let order = fixture.fake.kinds();
    let position = |kind: &str| order.iter().position(|found| found == kind);
    assert!(position("login") < position("pull"), "{order:?}");
    let lines = result.lines(0);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("***") && line.contains("was refused by ghcr.io")),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains(PASSWORD)),
        "{lines:?}"
    );
    assert!(!fixture.base.join("state").join(&id).exists());
    let auth = calls
        .iter()
        .find(|(words, _)| words[0] == "pull")
        .expect("a pull")
        .0
        .clone();
    let file = auth
        .iter()
        .position(|word| word == "--authfile")
        .map(|position| auth[position + 1].clone())
        .expect("an auth file");
    assert!(
        !PathBuf::from(file).exists(),
        "the auth file outlived the job"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_that_podman_refuses_does_not_show_the_password_and_pulls_nothing() {
    let fixture = Fixture::new(Behaviour {
        absent: true,
        fail: "login",
        said: "Error: logging in with hunter2-of-the-registry failed",
        ..Behaviour::default()
    });
    let mut job = fixture.job(vec![step("true")]);
    job.container.as_mut().expect("a container").credentials = Some(Credentials {
        username: "ci".to_owned(),
        password: PASSWORD.to_owned(),
    });
    let result = finished(job, None).await;
    assert_eq!(result.conclusion, Conclusion::Failure);
    let lines = result.lines(0);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("podman could not log in to docker.io")
                && line.contains("logging in with *** failed")),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains(PASSWORD)),
        "{lines:?}"
    );
    assert!(fixture.fake.calls("pull").is_empty());
    assert!(fixture.fake.leaked().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_is_pulled_and_the_registry_logged_in_to_only_when_the_image_is_missing() {
    for (absent, pulls) in [(true, 1), (false, 0)] {
        let fixture = Fixture::new(Behaviour {
            absent,
            ..Behaviour::default()
        });
        let mut job = fixture.job(vec![step("true")]);
        job.container.as_mut().expect("a container").credentials = Some(Credentials {
            username: "ci".to_owned(),
            password: PASSWORD.to_owned(),
        });
        let result = finished(job, None).await;
        assert_eq!(result.conclusion, Conclusion::Success);
        assert_eq!(fixture.fake.calls("pull").len(), pulls, "absent: {absent}");
        assert_eq!(fixture.fake.calls("login").len(), pulls, "absent: {absent}");
    }
}

fn seed(directories: &[&std::path::Path]) {
    for directory in directories {
        fs::create_dir_all(directory.join("nested")).expect("a directory");
        fs::write(directory.join("nested/left"), "x").expect("a file");
    }
}

fn is_empty(directory: &std::path::Path) -> bool {
    fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_none())
}

#[tokio::test(flavor = "multi_thread")]
async fn directories_are_emptied_before_and_after_the_job_on_the_host_and_in_a_container() {
    for contained in [false, true] {
        let fixture = Fixture::new(Behaviour::default());
        let workspace = fixture.base.join("work/globalart.keel/keel");
        let actions = fixture.base.join("work/_actions");
        let tools = fixture.base.join("tools");
        seed(&[&workspace, &actions, &tools]);
        let mut job = if contained {
            fixture.job(vec![step("test -z \"$(ls -A \"$GITHUB_WORKSPACE\")\" && echo clean; echo mine > \"$GITHUB_WORKSPACE/file\""), step("exit 1")])
        } else {
            let mut job = job(
                &fixture.base,
                vec![
                    step(
                        "test -z \"$(ls -A \"$GITHUB_WORKSPACE\")\" && echo clean; echo mine > \"$GITHUB_WORKSPACE/file\"",
                    ),
                    step("exit 1"),
                ],
            );
            job.minute = Duration::from_millis(300);
            job
        };
        job.cleanup = Cleanup {
            roots: vec![fixture.base.join("work"), tools.clone()],
            before_download: Vec::new(),
            before: vec![workspace.clone()],
            after: vec![workspace.clone(), actions.clone(), tools.clone()],
        };
        let result = run(job).await;
        assert_eq!(
            result.conclusion,
            Conclusion::Failure,
            "contained: {contained}"
        );
        assert_eq!(result.lines(0), vec!["clean"], "contained: {contained}");
        for directory in [&workspace, &actions, &tools] {
            assert!(
                is_empty(directory),
                "contained: {contained}: {} is not empty",
                directory.display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_musl_image_gets_the_musl_build_of_node_for_its_actions() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(Behaviour {
        libc: "musl",
        ..Behaviour::default()
    });
    let externals = fixture.base.join("externals");
    let node = externals.join("node24-musl/bin/node");
    fs::create_dir_all(node.parent().expect("a directory")).expect("a directory");
    fs::write(&node, "#!/bin/sh\necho \"musl node ran\"\n").expect("a node");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).expect("permissions");
    let provision = Arc::new(Nodes {
        directory: externals.clone(),
        asked: Mutex::new(Vec::new()),
    });
    let reference = ActionReference {
        owner: "acme".to_owned(),
        repository: "actions".to_owned(),
        path: None,
        r#ref: "v1".to_owned(),
    };
    let mut uses = step("");
    uses.action = Action::Uses(reference.clone());
    let mut job = fixture.job(vec![uses]);
    job.container.as_mut().expect("a container").provision = Some(provision.clone());
    job.actions = vec![NodeAction {
        uses: reference,
        directory: fixture.base.join("work/_actions/acme/actions/0123456"),
        node: externals.join("node24/bin/node"),
        metadata: ActionMetadata {
            name: None,
            description: None,
            author: None,
            branding: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            runs: Runs::Node(NodeRuns {
                using: NodeRuntime::Node20,
                main: "main.js".to_owned(),
                pre: None,
                pre_if: None,
                post: None,
                post_if: None,
            }),
        },
    }];
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Success,
        "{:#?}",
        result.events
    );
    assert_eq!(result.lines(0), vec!["musl node ran"]);
    assert_eq!(
        *provision
            .asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        vec![Libc::Musl]
    );
    let launchers = fixture.fake.state().launchers.clone();
    assert!(
        launchers[0]
            .contains("'/__e/node24-musl/bin/node' '/__w/_actions/acme/actions/0123456/main.js'"),
        "{}",
        launchers[0]
    );
    assert!(
        launchers[0].contains("export GITHUB_ACTION_PATH='/__w/_actions/acme/actions/0123456'")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_cannot_make_the_agent_read_or_overwrite_a_file_of_the_host_through_a_link() {
    let fixture = Fixture::new(Behaviour::default());
    let precious = fixture.base.join("precious");
    fs::write(&precious, "leaked=the credential of the agent\n").expect("a file");
    let plant = format!(
        "rm -f \"$GITHUB_OUTPUT\"\nln -s '{0}' \"$GITHUB_OUTPUT\"\nln -s '{0}' \"$RUNNER_TEMP/step-1.sh\"\n",
        precious.display()
    );
    let mut planting = step(&plant);
    planting.key = Some("plant".parse().expect("a step id"));
    let mut after = step("echo ran");
    after.r#if = bosun_expr::condition("always()").expect("a condition");
    let mut job = fixture.job(vec![planting, after]);
    job.template.outputs = [(
        "leaked".to_owned(),
        template("${{ steps.plant.outputs.leaked }}"),
    )]
    .into();
    let result = finished(job, None).await;
    assert!(
        result.lines(0).iter().any(|line| {
            line.contains("GITHUB_OUTPUT could not be read") && line.contains("symbolic link")
        }),
        "{:?}",
        result.lines(0)
    );
    assert_eq!(
        result
            .ended(0)
            .map(|(outcome, _, outputs)| (outcome, outputs)),
        Some((Conclusion::Failure, BTreeMap::new()))
    );
    assert!(!result.outputs.contains_key("leaked"));
    assert_eq!(result.lines(1), vec!["ran"]);
    assert_eq!(
        fs::read_to_string(&precious).expect("a file"),
        "leaked=the credential of the agent\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_needs_podman_and_has_no_container_fails_before_its_first_step_and_runs_nothing()
{
    let fixture = Fixture::new(Behaviour::default());
    let marker = fixture.base.join("ran");
    let mut hosted = job(
        &fixture.base,
        vec![step(&format!("touch '{}'", marker.display()))],
    );
    hosted.evaluated.needs_podman = true;
    let refused = finished(hosted, None).await;
    assert_eq!(refused.conclusion, Conclusion::Failure);
    let lines = refused.lines(0);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("needs podman and carries no container")),
        "{lines:?}"
    );
    assert!(!marker.exists());
    assert!(fixture.fake.kinds().is_empty());

    let shell = Fixture::new(Behaviour::default());
    let marker = shell.base.join("ran");
    let job = job(
        &shell.base,
        vec![step(&format!("touch '{}'", marker.display()))],
    );
    let ran = finished(job, None).await;
    assert_eq!(ran.conclusion, Conclusion::Success, "{:#?}", ran.events);
    assert!(marker.exists());
}

fn leftovers(fixture: &Fixture, owner: &str) {
    let mut state = fixture.fake.state();
    for (name, labels) in [
        ("bosun-job-a", labelled("exec-a", Some(owner))),
        ("bosun-job-b", labelled("exec-b", Some(owner))),
        (
            "of-another-agent",
            labelled("exec-c", Some("0123456789abcdef")),
        ),
        ("before-the-owner-label", labelled("exec-d", None)),
        ("someone-elses", Labels::new()),
    ] {
        state.containers.insert(name.to_owned(), labels);
    }
    for (name, labels) in [
        ("bosun-exec-a", labelled("exec-a", Some(owner))),
        ("bosun-exec-b", labelled("exec-b", Some(owner))),
        ("podman", Labels::new()),
    ] {
        state.networks.insert(name.to_owned(), labels);
    }
    for (name, labels) in [
        ("cache-a", labelled("exec-a", Some(owner))),
        ("data", Labels::new()),
    ] {
        state.volumes.insert(name.to_owned(), labels);
    }
    drop(state);
    for execution in ["exec-a", "exec-b"] {
        let directory = fixture.base.join("state").join(execution);
        fs::create_dir_all(&directory).expect("a state directory");
        fs::write(directory.join("auth.json"), "{}").expect("an auth file");
    }
}

fn owner(fixture: &Fixture) -> String {
    use std::os::unix::ffi::OsStrExt;

    let digest = Sha256::digest(fixture.base.join("state").as_os_str().as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sweep_removes_what_this_agent_left_and_touches_nothing_else() {
    let fixture = Fixture::new(Behaviour::default());
    leftovers(&fixture, &owner(&fixture));
    let engine: Arc<dyn Engine> = Arc::clone(&fixture.fake) as Arc<dyn Engine>;
    let faults = bosun_runner::container::sweep(&engine, &fixture.base.join("state")).await;
    assert!(faults.is_empty(), "{faults:?}");
    assert_eq!(
        fixture.fake.leaked(),
        [
            "before-the-owner-label",
            "of-another-agent",
            "someone-elses",
            "podman"
        ]
    );
    assert_eq!(
        fixture
            .fake
            .state()
            .volumes
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["data"]
    );
    assert!(is_empty(&fixture.base.join("state")));

    let again = bosun_runner::container::sweep(&engine, &fixture.base.join("never-made")).await;
    assert!(again.is_empty(), "{again:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removal_that_fails_is_reported_and_the_sweep_goes_on() {
    let fixture = Fixture::new(Behaviour {
        fail: "rm",
        said: "Error: container is busy",
        ..Behaviour::default()
    });
    leftovers(&fixture, &owner(&fixture));
    let engine: Arc<dyn Engine> = Arc::clone(&fixture.fake) as Arc<dyn Engine>;
    let faults = bosun_runner::container::sweep(&engine, &fixture.base.join("state")).await;
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert!(
        faults[0].starts_with("a container of an earlier job could not be removed")
            && faults[0].contains("container is busy"),
        "{faults:?}"
    );
    assert_eq!(
        fixture.fake.leaked(),
        [
            "before-the-owner-label",
            "bosun-job-a",
            "bosun-job-b",
            "of-another-agent",
            "someone-elses",
            "podman"
        ]
    );
    assert!(fixture.fake.state().volumes.contains_key("data"));
    assert!(!fixture.fake.state().volumes.contains_key("cache-a"));
    let left: Vec<_> = fs::read_dir(fixture.base.join("state"))
        .expect("listed")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(left, ["dirty"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_of_an_untrusted_run_without_a_container_fails_before_its_first_step_and_runs_nothing()
 {
    let fixture = Fixture::new(Behaviour::default());
    let marker = fixture.base.join("ran");
    let mut hosted = job(
        &fixture.base,
        vec![step(&format!("touch '{}'", marker.display()))],
    );
    hosted.evaluated.untrusted = true;
    let refused = finished(hosted, None).await;
    assert_eq!(refused.conclusion, Conclusion::Failure);
    let lines = refused.lines(0);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("a job of an untrusted run runs only in a container")),
        "{lines:?}"
    );
    assert!(!marker.exists());
    assert!(!fixture.base.join("work").exists());
    assert!(fixture.fake.kinds().is_empty());

    let contained = Fixture::new(Behaviour::default());
    let mut job = contained.job(vec![step("echo ran")]);
    job.evaluated.untrusted = true;
    job.container.as_mut().expect("a container").untrusted = true;
    let ran = finished(job, None).await;
    assert_eq!(ran.conclusion, Conclusion::Success, "{:#?}", ran.events);
    assert_eq!(ran.lines(0), vec!["ran"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untrusted_job_starts_and_ends_with_an_empty_work_directory_and_tool_cache() {
    let fixture = Fixture::new(Behaviour::default());
    let work = fixture.base.join("work");
    let tools = fixture.base.join("tools");
    seed(&[
        &work.join("junk"),
        &work.join("_actions/acme/actions/0123456"),
        &work.join("globalart.other/other"),
        &tools.join("node/20"),
    ]);
    let mut job = fixture.job(vec![step(
        "echo mine > \"$RUNNER_TEMP/../left-by-the-job\"; mkdir -p \"$RUNNER_TOOL_CACHE/new\"; \
         echo mine > \"$RUNNER_TOOL_CACHE/new/file\"; echo mine > \"$GITHUB_WORKSPACE/file\"",
    )]);
    job.evaluated.untrusted = true;
    job.container.as_mut().expect("a container").untrusted = true;
    job.cleanup = Cleanup {
        roots: vec![work.clone(), tools.clone()],
        before_download: vec![work.clone(), tools.clone()],
        before: Vec::new(),
        after: vec![work.clone(), tools.clone()],
    };
    let engine: Arc<dyn Engine> = Arc::clone(&fixture.fake) as Arc<dyn Engine>;
    job.cleanup
        .empty_before_download(Some(&engine))
        .await
        .expect("emptied before the download");
    assert!(is_empty(&work) && is_empty(&tools));
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Success,
        "{:#?}",
        result.events
    );
    assert!(
        is_empty(&work),
        "{:?}",
        fs::read_dir(&work).map(|e| e.count())
    );
    assert!(is_empty(&tools));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_planted_in_the_temp_directory_never_becomes_a_mount_source_or_a_place_to_write() {
    let fixture = Fixture::new(Behaviour::default());
    let victim = fixture.base.join("victim");
    fs::create_dir_all(&victim).expect("a directory");
    let temp = fixture.base.join("work/_temp");
    fs::create_dir_all(&temp).expect("a directory");
    for planted in ["_github_home", "_github_workflow"] {
        std::os::unix::fs::symlink(&victim, temp.join(planted)).expect("a link");
    }
    let job = fixture.job(vec![step("true")]);
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Success,
        "{:#?}",
        result.events
    );
    assert_eq!(fixture.fake.state().linked_sources, Vec::<String>::new());
    assert_eq!(fs::read_dir(&victim).expect("listed").count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn preparing_the_container_ends_with_the_timeout_of_the_job_and_still_tears_down() {
    let fixture = Fixture::new(Behaviour {
        absent: true,
        hang: "pull",
        ..Behaviour::default()
    });
    let mut job = fixture.job(vec![step("echo ran")]);
    job.evaluated.timeout_minutes = 1;
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::TimedOut,
        "{:#?}",
        result.events
    );
    assert_eq!(
        result.lines(0),
        vec![
            "the job could not be prepared on this agent: the job ran beyond its \
             `timeout-minutes` while its container was being prepared"
        ]
    );
    assert!(fixture.fake.calls("create").is_empty());
    assert!(fixture.fake.kinds().contains(&"rm --ignore".to_owned()));
    assert!(fixture.fake.leaked().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_checked_whenever_they_are_given_even_when_the_image_is_on_the_host() {
    for (username, password) in [("ci", ""), ("", PASSWORD)] {
        let fixture = Fixture::new(Behaviour::default());
        let mut job = fixture.job(vec![step("echo ran")]);
        job.container.as_mut().expect("a container").credentials = Some(Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        });
        let result = finished(job, None).await;
        assert_eq!(result.conclusion, Conclusion::Failure, "{username:?}");
        assert_eq!(
            result.lines(0),
            vec![
                "the job could not be prepared on this agent: `container.credentials` needs a \
                 username and a password, and one of them is empty"
            ]
        );
        assert!(fixture.fake.calls("image").is_empty());
        assert!(fixture.fake.calls("login").is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn credentials_that_are_both_empty_pull_without_logging_in() {
    let fixture = Fixture::new(Behaviour {
        absent: true,
        ..Behaviour::default()
    });
    let mut job = fixture.job(vec![step("echo ran")]);
    job.container.as_mut().expect("a container").credentials = Some(Credentials {
        username: String::new(),
        password: String::new(),
    });
    let result = finished(job, None).await;
    assert_eq!(
        result.conclusion,
        Conclusion::Success,
        "{:#?}",
        result.events
    );
    assert!(fixture.fake.calls("login").is_empty());
    let pulls = fixture.fake.calls("pull");
    assert_eq!(pulls.len(), 1);
    assert!(!pulls[0].contains(&"--authfile".to_owned()), "{pulls:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ports_and_volumes_are_checked_and_a_pool_of_untrusted_code_publishes_none() {
    let mut with_port = container(None, &[]);
    with_port.ports = vec![template("8080:80")];
    let fixture = Fixture::new(Behaviour::default());
    let mut job = fixture.job(vec![step("echo ran")]);
    job.template.container = Some(with_port.clone());
    let published = finished(job, None).await;
    assert_eq!(published.conclusion, Conclusion::Success);
    let created = &fixture.fake.calls("create")[0];
    assert_eq!(flag_value(created, "--publish"), "8080:80");

    let fixture = Fixture::new(Behaviour::default());
    let mut job = fixture.job(vec![step("echo ran")]);
    job.template.container = Some(with_port);
    job.evaluated.untrusted = true;
    job.container.as_mut().expect("a container").untrusted = true;
    let refused = finished(job, None).await;
    assert_eq!(refused.conclusion, Conclusion::Failure);
    assert_eq!(
        refused.lines(0),
        vec![
            "the job could not be prepared on this agent: `ports` of `container` publishes a \
             port on the host, which a pool of untrusted code never does"
        ]
    );
    assert!(fixture.fake.calls("create").is_empty());

    let fixture = Fixture::new(Behaviour::default());
    let mut job = fixture.job(vec![step("echo ran")]);
    job.template.container = Some(container(None, &["data\n:/x"]));
    job.container
        .as_mut()
        .expect("a container")
        .allow_privileged = true;
    let malformed = finished(job, None).await;
    assert_eq!(malformed.conclusion, Conclusion::Failure);
    assert_eq!(
        malformed.lines(0),
        vec![
            "the job could not be prepared on this agent: the volume `data\\n:/x` of \
             `container` holds the control character U+000A, which podman may read differently \
             from the agent"
        ]
    );
}

fn cleaning(fixture: &Fixture) -> Cleanup {
    let work = fixture.base.join("work");
    let tools = fixture.base.join("tools");
    Cleanup {
        roots: vec![work.clone(), tools.clone()],
        before_download: Vec::new(),
        before: vec![work.clone(), tools.clone()],
        after: vec![work, tools],
    }
}

fn untrusted(mut job: Job) -> Job {
    job.evaluated.untrusted = true;
    job.container.as_mut().expect("a container").untrusted = true;
    job
}

#[tokio::test(flavor = "multi_thread")]
async fn a_container_that_could_not_be_removed_keeps_the_directories_until_the_next_job_removes_it()
{
    let fixture = Fixture::new(Behaviour {
        fail_first: "rm",
        said: "Error: container is busy",
        ..Behaviour::default()
    });
    let marker = fixture.base.join("state/dirty");
    let mut first = untrusted(fixture.job(vec![step(
        "echo mine > \"$GITHUB_WORKSPACE/file\"; echo mine > \"$RUNNER_TOOL_CACHE/file\"",
    )]));
    first.cleanup = cleaning(&fixture);
    let left = finished(first, None).await;
    assert_eq!(left.conclusion, Conclusion::Success, "{:#?}", left.events);
    let lines = left.lines(0);
    assert!(
        lines.iter().any(|line| line.contains("container is busy")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("does not empty the directories of the job now")),
        "{lines:?}"
    );
    assert!(fixture.base.join("work/globalart.keel/keel/file").exists());
    assert!(fixture.base.join("tools/file").exists());
    assert!(marker.exists());
    let seen = fixture.fake.kinds().len();

    let mut second = untrusted(fixture.job(vec![step(
        "test -z \"$(ls -A \"$GITHUB_WORKSPACE\")\" && echo clean",
    )]));
    second.cleanup = cleaning(&fixture);
    let next = finished(second, None).await;
    assert_eq!(next.conclusion, Conclusion::Success, "{:#?}", next.events);
    assert_eq!(next.lines(0), vec!["clean"]);
    let kinds = fixture.fake.kinds();
    assert_eq!(
        &kinds[seen..seen + 4],
        ["ps", "volume ls", "network ls", "image exists"]
    );
    assert!(!marker.exists());
    assert!(is_empty(&fixture.base.join("work")) && is_empty(&fixture.base.join("tools")));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_still_cannot_remove_the_container_refuses_the_next_job_and_empties_nothing()
{
    let fixture = Fixture::new(Behaviour {
        fail: "rm",
        said: "Error: container is busy",
        ..Behaviour::default()
    });
    let mut first = untrusted(fixture.job(vec![step("echo mine > \"$GITHUB_WORKSPACE/file\"")]));
    first.cleanup = cleaning(&fixture);
    let left = finished(first, None).await;
    assert_eq!(left.conclusion, Conclusion::Success, "{:#?}", left.events);
    let file = fixture.base.join("work/globalart.keel/keel/file");
    assert!(file.exists());
    let seen = fixture.fake.kinds().len();

    let marker = fixture.base.join("ran");
    let mut second = untrusted(fixture.job(vec![step(&format!("touch '{}'", marker.display()))]));
    second.cleanup = cleaning(&fixture);
    let refused = finished(second, None).await;
    assert_eq!(refused.conclusion, Conclusion::Failure);
    let lines = refused.lines(0);
    assert!(
        lines.iter().any(|line| line.starts_with(
            "the job could not be prepared on this agent: a container of an earlier job could \
             not be removed"
        ) && line.contains("container is busy")
            && line.contains("takes no job until it can")),
        "{lines:?}"
    );
    assert!(!marker.exists());
    assert!(file.exists());
    assert!(fixture.base.join("state/dirty").exists());
    let kinds = fixture.fake.kinds();
    assert!(
        kinds[seen..]
            .iter()
            .all(|kind| matches!(kind.as_str(), "ps" | "rm" | "volume ls" | "network ls")),
        "{kinds:?}"
    );
}

fn shell_with_podman(fixture: &Fixture, script: &str) -> Job {
    let mut job = job(&fixture.base, vec![step(script)]);
    job.recovery = Some(Recovery {
        engine: Arc::clone(&fixture.fake) as Arc<dyn Engine>,
        state: fixture.base.join("state"),
    });
    job.cleanup = cleaning(fixture);
    job.cleanup.before = vec![fixture.base.join("work/globalart.keel/keel")];
    job
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_job_on_an_agent_with_podman_removes_a_leftover_before_it_cleans_and_cleans_nothing_while_it_cannot()
 {
    let fixture = Fixture::new(Behaviour::default());
    let workspace = fixture.base.join("work/globalart.keel/keel");
    seed(&[&workspace]);
    let marker = fixture.base.join("state/dirty");
    fs::create_dir_all(marker.parent().expect("a directory")).expect("a state directory");
    fs::write(&marker, "x").expect("a mark");
    let swept = finished(
        shell_with_podman(&fixture, r#"ls -A "$GITHUB_WORKSPACE""#),
        None,
    )
    .await;
    assert_eq!(swept.conclusion, Conclusion::Success, "{:#?}", swept.events);
    assert!(swept.lines(0).is_empty(), "{:?}", swept.lines(0));
    assert_eq!(
        &fixture.fake.kinds()[..3],
        ["ps", "volume ls", "network ls"]
    );
    assert!(!marker.exists());

    let stuck = Fixture::new(Behaviour {
        fail: "rm",
        said: "Error: container is busy",
        ..Behaviour::default()
    });
    leftovers(&stuck, &owner(&stuck));
    fs::write(stuck.base.join("state/dirty"), "x").expect("a mark");
    let workspace = stuck.base.join("work/globalart.keel/keel");
    seed(&[&workspace]);
    let ran = stuck.base.join("ran");
    let refused = finished(
        shell_with_podman(&stuck, &format!("touch '{}'", ran.display())),
        None,
    )
    .await;
    assert_eq!(refused.conclusion, Conclusion::Failure);
    assert!(
        refused.lines(0).iter().any(|line| line.starts_with(
            "the job could not be prepared on this agent: a container of an earlier job could \
             not be removed"
        )),
        "{:?}",
        refused.lines(0)
    );
    assert!(!ran.exists());
    assert!(workspace.join("nested/left").exists());
    assert!(stuck.base.join("state/dirty").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_while_the_container_is_being_created_logs_what_the_teardown_could_not_remove() {
    let fixture = Fixture::new(Behaviour {
        hang: "create",
        fail: "rm",
        said: "Error: container is busy",
        ..Behaviour::default()
    });
    let mut job = untrusted(fixture.job(vec![step("true")]));
    job.cleanup = Cleanup {
        before: Vec::new(),
        ..cleaning(&fixture)
    };
    fs::create_dir_all(fixture.base.join("tools")).expect("a directory");
    fs::write(fixture.base.join("tools/left"), "x").expect("a file");
    let (cancel, cancelled) = watch::channel(false);
    let (events, mut received) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move {
        let tracking = bosun_runner::Tracking::probe();
        bosun_runner::run(job, &tracking, events, cancelled).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !fixture.fake.kinds().contains(&"create".to_owned()) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no container was created"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.send_replace(true);
    let completed = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .expect("the job ended")
        .expect("the job ran");
    assert_eq!(completed.conclusion, Conclusion::Cancelled);
    let mut lines = Vec::new();
    while let Ok(event) = received.try_recv() {
        if let bosun_runner::Event::Line { text, .. } = event {
            lines.push(String::from_utf8_lossy(&text).into_owned());
        }
    }
    assert!(
        lines.iter().any(
            |line| line.contains("the job container could not be removed")
                && line.contains("container is busy")
        ),
        "{lines:?}"
    );
    assert!(fixture.base.join("tools/left").exists());
    assert!(fixture.base.join("state/dirty").exists());
}
