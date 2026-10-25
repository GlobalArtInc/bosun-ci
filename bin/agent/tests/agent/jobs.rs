use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::step::Action;
use bosun_proto::agent::v1::{Conclusion, Log, Segment};

use crate::server::{FakeServer, logs};
use crate::{Home, assignment, step, template, text, workspace};

#[tokio::test(flavor = "multi_thread")]
async fn a_job_runs_its_steps_in_the_workspace_and_reports_each_result_and_its_logs() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut first = step(0, "echo \"$GREETING from $STEP\"\npwd");
    first
        .env
        .insert("STEP".to_owned(), template("the first step"));
    let mut job = assignment(vec![
        first,
        step(1, "echo out\necho err >&2\nexit 3"),
        step(2, "echo never"),
    ]);
    job.env.insert("GREETING".to_owned(), "hello".to_owned());
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));

    let mut reports = Vec::new();
    loop {
        let message = stream.next().await;
        match &message {
            Inbound::Accept(accept) => assert_eq!(accept.execution_id, execution),
            Inbound::StepStarted(started) => {
                assert_eq!(started.execution_id, execution);
                assert!(started.started_at.is_some());
                reports.push(format!("started {}", started.step_index));
            }
            Inbound::StepFinished(finished) => {
                assert_eq!(finished.execution_id, execution);
                assert!(finished.completed_at.is_some());
                assert_eq!(finished.outcome, finished.conclusion);
                reports.push(format!(
                    "finished {} {:?}",
                    finished.step_index,
                    finished.outcome()
                ));
            }
            Inbound::Log(log) => {
                assert_eq!(log.execution_id, execution);
                reports.push(format!("log {}", log.step_index));
            }
            Inbound::JobFinished(finished) => {
                assert_eq!(finished.execution_id, execution);
                assert_eq!(finished.conclusion(), Conclusion::Failure);
                break;
            }
            other => panic!("an unexpected message during the job: {other:?}"),
        }
    }
    reports.dedup();
    assert_eq!(
        reports,
        vec![
            "started 0",
            "log 0",
            "finished 0 Success",
            "started 1",
            "log 1",
            "finished 1 Failure",
        ]
    );
    stream.idle().await;

    let chunks = logs(&stream.received);
    let seqs: Vec<u64> = chunks.keys().copied().collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    let of_step = |index: u32| -> Vec<&Log> {
        chunks
            .values()
            .filter(|log| log.step_index == index)
            .collect()
    };
    assert_eq!(
        text(&of_step(0)),
        vec![
            "hello from the first step".to_owned(),
            workspace(&home).display().to_string(),
        ]
    );
    assert_eq!(text(&of_step(1)), vec!["out", "err"]);
    assert!(of_step(2).is_empty());
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assignment_the_agent_cannot_run_is_rejected_with_its_reason_and_the_slot_stays_free() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut no_action = step(0, "echo");
    no_action.action = None;
    let mut unknown_segment = step(0, "echo");
    unknown_segment.action = Some(Action::Run(bosun_proto::agent::v1::Template {
        segments: vec![Segment { segment: None }],
    }));
    let mut bad_shell = step(0, "echo");
    bad_shell.shell = Some("perl -w".to_owned());
    let mut bad_slug = assignment(vec![step(0, "echo")]);
    bad_slug.project = "../escape".to_owned();
    for (job, reason) in [
        (
            assignment(vec![no_action]),
            "step 0 has no action this agent knows",
        ),
        (
            assignment(vec![unknown_segment]),
            "step 0 holds a template segment this agent does not know",
        ),
        (
            assignment(vec![bad_shell]),
            "the shell of step 0 is `perl -w`, which is neither",
        ),
        (bad_slug, "`../escape` is not the slug of a project"),
    ] {
        let execution = job.execution_id.clone();
        stream.send(Outgoing::Assignment(job));
        let reject = stream
            .until("a Reject", |message| match message {
                Inbound::Reject(reject) => Some(reject.clone()),
                Inbound::Accept(accept) => panic!("an invalid job was accepted: {accept:?}"),
                _ => None,
            })
            .await;
        assert_eq!(reject.execution_id, execution);
        assert!(reject.reason.contains(reason), "{}", reject.reason);
        stream.idle().await;
    }

    let busy = assignment(vec![step(0, "echo ready\nsleep 5")]);
    let other = assignment(vec![step(0, "echo")]);
    let other_execution = other.execution_id.clone();
    stream.send(Outgoing::Assignment(busy));
    stream
        .until("the first line", |message| {
            matches!(message, Inbound::Log(_)).then_some(())
        })
        .await;
    stream.send(Outgoing::Assignment(other));
    let reject = stream
        .until("a Reject", |message| match message {
            Inbound::Reject(reject) => Some(reject.clone()),
            _ => None,
        })
        .await;
    assert_eq!(reject.execution_id, other_execution);
    assert!(
        reject.reason.contains("is running the execution"),
        "{}",
        reject.reason
    );
    running.stop().await;
}
