use std::time::Duration;

use bosun_runner::{Event, Tracking};
use bosun_types::status::Conclusion;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, timeout};

use crate::{WAIT, job, lines, step};

struct Cancelled {
    conclusion: Conclusion,
    events: Vec<Event>,
    after: Duration,
}

async fn cancel_after_ready(script: &str) -> Cancelled {
    let directory = tempfile::tempdir().expect("a directory");
    let job = job(directory.path(), vec![step(script), step("echo never")]);
    let (cancel, cancelled) = watch::channel(false);
    let (events, mut received) = mpsc::unbounded_channel();
    let running = tokio::spawn(async move {
        let tracking = Tracking::probe();
        bosun_runner::run(job, &tracking, events, cancelled).await
    });
    let mut seen = Vec::new();
    let mut cancelled_at = None;
    let mut finished_at = None;
    while let Some(event) = timeout(WAIT, received.recv())
        .await
        .unwrap_or_else(|_| panic!("the job sent nothing for {WAIT:?}: {seen:#?}"))
    {
        match &event {
            Event::Line { index: 0, text, .. } if text == b"ready" => {
                cancel.send_replace(true);
                cancelled_at = Some(Instant::now());
            }
            Event::StepFinished { index: 0, .. } => finished_at = Some(Instant::now()),
            _ => {}
        }
        seen.push(event);
    }
    let conclusion = running.await.expect("the job runs to its end").conclusion;
    let (Some(cancelled_at), Some(finished_at)) = (cancelled_at, finished_at) else {
        panic!("the step never became ready or never finished: {seen:#?}");
    };
    Cancelled {
        conclusion,
        events: seen,
        after: finished_at - cancelled_at,
    }
}

fn outcome(events: &[Event]) -> Option<Conclusion> {
    events.iter().find_map(|event| match event {
        Event::StepFinished {
            index: 0, outcome, ..
        } => Some(*outcome),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_interrupts_the_step_first_and_skips_the_steps_after_it() {
    let cancelled = cancel_after_ready(
        "trap 'echo interrupted; exit 130' INT\n\
         echo ready\n\
         while true; do sleep 0.1 || true; done\n",
    )
    .await;
    assert_eq!(cancelled.conclusion, Conclusion::Cancelled);
    assert_eq!(outcome(&cancelled.events), Some(Conclusion::Cancelled));
    assert_eq!(lines(&cancelled.events, 0), vec!["ready", "interrupted"]);
    assert!(
        cancelled.after < Duration::from_secs(5),
        "the step took {:?} to end after SIGINT",
        cancelled.after
    );
    assert!(
        !cancelled
            .events
            .iter()
            .any(|event| matches!(event, Event::StepStarted { index: 1, .. })),
        "a step after the cancel started: {:#?}",
        cancelled.events
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_that_survives_sigint_gets_sigterm_seven_and_a_half_seconds_later() {
    let cancelled = cancel_after_ready(
        "trap 'echo interrupted' INT\n\
         trap 'echo terminated; exit 143' TERM\n\
         echo ready\n\
         while true; do sleep 0.1 || true; done\n",
    )
    .await;
    assert_eq!(cancelled.conclusion, Conclusion::Cancelled);
    assert_eq!(outcome(&cancelled.events), Some(Conclusion::Cancelled));
    let markers: Vec<String> = lines(&cancelled.events, 0)
        .into_iter()
        .filter(|line| ["ready", "interrupted", "terminated"].contains(&line.as_str()))
        .collect();
    assert_eq!(markers, vec!["ready", "interrupted", "terminated"]);
    assert!(
        cancelled.after >= Duration::from_millis(7400) && cancelled.after < Duration::from_secs(10),
        "the step ended {:?} after the cancel",
        cancelled.after
    );
}
