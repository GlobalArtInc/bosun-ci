use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Conclusion, Control, ControlKind};
use tokio::time::Instant;
use uuid::Uuid;

use crate::server::{FakeServer, logs};
use crate::{Home, assignment, gone, pid, step, text};

#[tokio::test(flavor = "multi_thread")]
async fn a_step_that_ignores_sigint_and_sigterm_is_killed_within_15_seconds_of_a_cancel() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let job = assignment(vec![
        step(
            0,
            "trap '' INT TERM\nsleep 600 &\necho \"sleeping $!\"\nwait",
        ),
        step(1, "echo never"),
    ]);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    stream
        .until("the line of the sleeping step", |message| match message {
            Inbound::Log(log) => String::from_utf8_lossy(&log.lines)
                .contains(" sleeping ")
                .then_some(()),
            _ => None,
        })
        .await;

    let control = Uuid::now_v7().to_string();
    stream.send(Outgoing::Control(Control {
        id: control.clone(),
        execution_id: execution.clone(),
        kind: ControlKind::Cancel.into(),
    }));
    let cancelled_at = Instant::now();
    let ack = stream
        .until(
            "the acknowledgement of the cancel",
            |message| match message {
                Inbound::ControlAck(ack) if ack.id == control => Some(ack.clone()),
                _ => None,
            },
        )
        .await;
    assert!(ack.took_effect, "the cancel did not take effect");
    let step = stream
        .until("the end of the step", |message| match message {
            Inbound::StepFinished(finished) => Some(finished.clone()),
            _ => None,
        })
        .await;
    assert_eq!(step.step_index, 0);
    assert_eq!(step.outcome(), Conclusion::Cancelled);
    assert_eq!(step.conclusion(), Conclusion::Cancelled);
    let finished = stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.clone()),
            Inbound::StepStarted(started) => panic!("a step started after the cancel: {started:?}"),
            _ => None,
        })
        .await;
    let elapsed = cancelled_at.elapsed();
    assert_eq!(finished.execution_id, execution);
    assert_eq!(finished.conclusion(), Conclusion::Cancelled);
    assert!(
        elapsed < Duration::from_secs(15),
        "the step was killed {elapsed:?} after the cancel"
    );
    assert!(
        elapsed >= Duration::from_secs(9),
        "the step ended {elapsed:?} after the cancel, before SIGKILL was due"
    );
    stream.idle().await;

    stream.send(Outgoing::Control(Control {
        id: control.clone(),
        execution_id: execution.clone(),
        kind: ControlKind::Cancel.into(),
    }));
    let repeated = stream
        .until(
            "the acknowledgement of the resent cancel",
            |message| match message {
                Inbound::ControlAck(ack) if ack.id == control => Some(ack.clone()),
                _ => None,
            },
        )
        .await;
    assert_eq!(
        repeated, ack,
        "a resent cancel was not answered with its first acknowledgement"
    );

    let chunks = logs(&stream.received);
    let lines = text(&chunks.values().collect::<Vec<_>>());
    let sleeping = pid(&lines, "sleeping");
    assert!(
        gone(sleeping).await,
        "the ignoring process {sleeping:?} survived the cancel"
    );
    running.stop().await;
}
