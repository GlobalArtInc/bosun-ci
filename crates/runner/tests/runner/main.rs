mod actions;
mod cancel;
mod cases;
#[cfg(target_os = "linux")]
mod cgroup;
mod cleanup;
mod environment;
mod hash_files;
mod order;
mod prepare;
mod timeout;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bosun_proto::assignment::{Contexts, Evaluated};
use bosun_runner::{Annotation, BosunFacts, Event, Job, Runner, Tracking};
use bosun_types::ir::{
    Action, Defaults, Evaluable, JobKind, JobTemplate, RunsOn, SecretReferences, Span, Step,
    Template,
};
use bosun_types::status::Conclusion;
use rustix::process::{Pid, test_kill_process};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep, timeout};

pub const WAIT: Duration = Duration::from_secs(60);

const SPAN: Span = Span {
    line: 1,
    column: 1,
    end_line: 1,
    end_column: 1,
};

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn job(directory: &Path, steps: Vec<Step>) -> Job {
    static JOBS: AtomicU64 = AtomicU64::new(0);
    Job {
        id: format!(
            "test-{}-{}",
            std::process::id(),
            JOBS.fetch_add(1, Ordering::Relaxed)
        ),
        workspace: directory.join("work/globalart.keel/keel"),
        temp: directory.join("work/_temp"),
        tool_cache: directory.join("tools"),
        runner: Runner {
            name: "runner1-1".to_owned(),
            os: "Linux".to_owned(),
            arch: "X64".to_owned(),
        },
        bosun: BosunFacts {
            url: "https://ci.example.org".to_owned(),
            run_id: "0192f3a4-5b6c-7d8e-9f01-000000000007".to_owned(),
            run_url: "https://ci.example.org/runs/0192f3a4-5b6c-7d8e-9f01-000000000007".to_owned(),
            job_id: "0192f3a4-5b6c-7d8e-9f01-000000000009".to_owned(),
            draft: false,
        },
        proxies: BTreeMap::new(),
        template: JobTemplate {
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
            github: Map::new(),
            needs: Map::new(),
            strategy: Value::Null,
            matrix: Value::Null,
            inputs: Map::new(),
            vars: Map::new(),
            secrets: Map::new(),
            bosun: Map::new(),
        },
        evaluated: Evaluated {
            env: BTreeMap::new(),
            shell: None,
            working_directory: None,
            timeout_minutes: 360,
            continue_on_error: false,
        },
        actions: Vec::new(),
        fetched: Ok(()),
        minute: Duration::from_secs(60),
    }
}

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

pub fn template(text: &str) -> Template {
    bosun_expr::template(text).unwrap_or_else(|error| panic!("`{text}`: {error:?}"))
}

pub struct Finished {
    pub conclusion: Conclusion,
    pub outputs: BTreeMap<String, String>,
    pub events: Vec<Event>,
}

impl Finished {
    pub fn lines(&self, index: u32) -> Vec<String> {
        lines(&self.events, index)
    }

    pub fn ended(&self, index: u32) -> Option<(Conclusion, Conclusion, BTreeMap<String, String>)> {
        self.events.iter().find_map(|event| match event {
            Event::StepFinished {
                index: finished,
                outcome,
                conclusion,
                outputs,
                ..
            } if *finished == index => Some((*outcome, *conclusion, outputs.clone())),
            _ => None,
        })
    }

    pub fn reported(&self, index: u32) -> Option<(Vec<Annotation>, Option<String>)> {
        self.events.iter().find_map(|event| match event {
            Event::StepFinished {
                index: finished,
                annotations,
                summary,
                ..
            } if *finished == index => Some((annotations.clone(), summary.clone())),
            _ => None,
        })
    }

    pub fn outcome(&self, index: u32) -> Option<Conclusion> {
        self.ended(index).map(|(outcome, conclusion, _)| {
            assert_eq!(outcome, conclusion, "step {index}");
            outcome
        })
    }

    pub fn started(&self, index: u32) -> bool {
        self.events.iter().any(
            |event| matches!(event, Event::StepStarted { index: started, .. } if *started == index),
        )
    }
}

pub fn lines(events: &[Event], index: u32) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Line {
                index: line, text, ..
            } if *line == index => Some(String::from_utf8_lossy(text).into_owned()),
            _ => None,
        })
        .collect()
}

pub async fn run(job: Job) -> Finished {
    run_cancelled_on(job, None).await
}

pub async fn run_cancelled_on(job: Job, cancel_on: Option<&str>) -> Finished {
    let (cancel, cancelled) = watch::channel(false);
    let (events, mut received) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move {
        let tracking = Tracking::probe();
        bosun_runner::run(job, &tracking, events, cancelled).await
    });
    let mut seen = Vec::new();
    timeout(WAIT, async {
        while let Some(event) = received.recv().await {
            if let (Some(trigger), Event::Line { text, .. }) = (cancel_on, &event)
                && text == trigger.as_bytes()
            {
                cancel.send_replace(true);
            }
            seen.push(event);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the job did not finish within {WAIT:?}: {seen:#?}"));
    let completed = running.await.expect("the job runs to its end");
    Finished {
        conclusion: completed.conclusion,
        outputs: completed.outputs,
        events: seen,
    }
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
