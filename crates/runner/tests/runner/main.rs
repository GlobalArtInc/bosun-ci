mod cancel;
mod cases;
#[cfg(target_os = "linux")]
mod cgroup;
mod cleanup;
mod environment;
mod order;
mod prepare;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bosun_runner::{BosunFacts, Event, Job, Runner, Step, Tracking};
use bosun_types::status::Conclusion;
use rustix::process::{Pid, test_kill_process};
use serde_json::Map;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep};

pub const WAIT: Duration = Duration::from_secs(60);

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
        github: Map::new(),
        bosun: BosunFacts {
            url: "https://ci.example.org".to_owned(),
            run_id: "0192f3a4-5b6c-7d8e-9f01-000000000007".to_owned(),
            run_url: "https://ci.example.org/runs/0192f3a4-5b6c-7d8e-9f01-000000000007".to_owned(),
            job_id: "0192f3a4-5b6c-7d8e-9f01-000000000009".to_owned(),
            draft: false,
        },
        env: BTreeMap::new(),
        steps,
    }
}

pub fn step(index: u32, script: &str) -> Step {
    Step {
        index,
        key: None,
        script: script.to_owned(),
        shell: None,
        working_directory: None,
        env: BTreeMap::new(),
    }
}

pub struct Finished {
    pub conclusion: Conclusion,
    pub events: Vec<Event>,
}

impl Finished {
    pub fn lines(&self, index: u32) -> Vec<String> {
        lines(&self.events, index)
    }

    pub fn outcome(&self, index: u32) -> Option<Conclusion> {
        self.events.iter().find_map(|event| match event {
            Event::StepFinished {
                index: finished,
                outcome,
                conclusion,
                ..
            } if *finished == index => {
                assert_eq!(outcome, conclusion, "step {index}");
                Some(*outcome)
            }
            _ => None,
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
    let (_cancel, cancelled) = watch::channel(false);
    let (events, mut received) = mpsc::unbounded_channel();
    let tracking = Tracking::probe();
    let conclusion =
        tokio::time::timeout(WAIT, bosun_runner::run(job, &tracking, events, cancelled))
            .await
            .unwrap_or_else(|_| panic!("the job did not finish within {WAIT:?}"));
    let mut events = Vec::new();
    while let Some(event) = received.recv().await {
        events.push(event);
    }
    Finished { conclusion, events }
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
