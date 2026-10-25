use std::time::{Duration, SystemTime};

use bosun_runner::Event;
use bosun_types::ir::Evaluable;
use bosun_types::status::Conclusion;

use crate::{Finished, job, run, step};

const MINUTE: Duration = Duration::from_millis(500);

fn span(finished: &Finished, index: u32) -> Duration {
    let started = finished.events.iter().find_map(|event| match event {
        Event::StepStarted { index: found, at } if *found == index => Some(*at),
        _ => None,
    });
    let ended = finished.events.iter().find_map(|event| match event {
        Event::StepFinished {
            index: found, at, ..
        } if *found == index => Some(*at),
        _ => None,
    });
    match (started, ended) {
        (Some(started), Some(ended)) => ended
            .duration_since(started)
            .unwrap_or_else(|_| panic!("step {index} ended before it started")),
        _ => panic!("step {index} did not start and end: {:#?}", finished.events),
    }
}

fn markers(finished: &Finished, index: u32) -> Vec<String> {
    finished
        .lines(index)
        .into_iter()
        .filter(|line| ["ready", "interrupted", "terminated"].contains(&line.as_str()))
        .collect()
}

const STUBBORN: &str = "trap 'echo interrupted' INT\n\
                        trap 'echo terminated; exit 143' TERM\n\
                        echo ready\n\
                        while true; do sleep 0.1 || true; done\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_step_over_its_timeout_gets_the_signals_of_a_cancel_and_times_out() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut slow = step(STUBBORN);
    slow.timeout_minutes = Some(Evaluable::Literal(2));
    let mut job = job(directory.path(), vec![slow, step("echo never")]);
    job.minute = MINUTE;
    let before = SystemTime::now();
    let finished = run(job).await;
    assert_eq!(finished.conclusion, Conclusion::Failure);
    assert_eq!(finished.outcome(0), Some(Conclusion::TimedOut));
    assert_eq!(finished.outcome(1), Some(Conclusion::Skipped));
    assert_eq!(
        markers(&finished, 0),
        ["ready", "interrupted", "terminated"]
    );
    let took = span(&finished, 0);
    assert!(
        took >= Duration::from_millis(1000 + 7400) && took < Duration::from_millis(1000 + 10_000),
        "the step ended {took:?} after it started"
    );
    assert!(before.elapsed().expect("a clock") < Duration::from_secs(30));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_over_its_timeout_interrupts_its_step_and_starts_no_other() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut cleanup = step("echo never");
    cleanup.r#if = bosun_expr::condition("always()").expect("a condition");
    let mut job = job(
        directory.path(),
        vec![
            step(
                "trap 'echo interrupted; exit 130' INT\necho ready\nwhile true; do sleep 0.1 || true; done\n",
            ),
            cleanup,
        ],
    );
    job.minute = MINUTE;
    job.evaluated.timeout_minutes = 2;
    let finished = run(job).await;
    assert_eq!(finished.conclusion, Conclusion::TimedOut);
    assert_eq!(finished.outcome(0), Some(Conclusion::TimedOut));
    assert_eq!(markers(&finished, 0), ["ready", "interrupted"]);
    let took = span(&finished, 0);
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_secs(6),
        "the step ended {took:?} after it started"
    );
    assert!(!finished.started(1) && finished.ended(1).is_none());
}
