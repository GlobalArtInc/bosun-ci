use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Hello, Log};
use tonic::Status;

use crate::server::{FakeServer, logs};
use crate::{Home, assignment, step, text};

fn listed(hello: &Hello) -> Vec<(String, u64)> {
    hello
        .executions
        .iter()
        .map(|running| (running.execution_id.clone(), running.last_log_seq))
        .collect()
}

fn describe(message: &Inbound) -> String {
    match message {
        Inbound::Log(log) => format!("log {}", log.seq),
        Inbound::StepStarted(started) => format!("started {}", started.step_index),
        Inbound::StepFinished(finished) => {
            format!("finished {} {:?}", finished.step_index, finished.outcome())
        }
        Inbound::JobFinished(finished) => format!("job {:?}", finished.conclusion()),
        Inbound::Idle(_) => "idle".to_owned(),
        other => format!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_runs_on_without_a_stream_and_the_agent_delivers_its_chunks_then_its_held_reports() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let mut running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let job = assignment(vec![
        step(0, "echo before\nsleep 3\necho after"),
        step(1, "echo second"),
    ]);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    let first = stream
        .until("the first chunk", |message| match message {
            Inbound::Log(log) => Some(log.clone()),
            _ => None,
        })
        .await;
    assert_eq!(text(&[&first]), vec!["before"]);
    stream.fail(Status::unavailable("the fake server lost the stream"));

    let held = server.accept().await;
    let running_executions = listed(&held.hello);
    assert_eq!(running_executions.len(), 1, "{running_executions:?}");
    assert_eq!(running_executions[0].0, execution);
    assert!(running_executions[0].1 >= first.seq);
    running.logged("finished a job").await;
    held.fail(Status::unavailable(
        "the fake server never welcomed the agent",
    ));

    let mut stream = server.accept().await;
    let held_executions = listed(&stream.hello);
    assert_eq!(
        held_executions.len(),
        1,
        "the Hello does not list the execution whose end the agent holds: {held_executions:?}"
    );
    assert_eq!(held_executions[0].0, execution);
    let last = held_executions[0].1;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let order: Vec<String> = stream.received.iter().map(describe).collect();
    let expected: Vec<String> = (1..=last)
        .map(|seq| format!("log {seq}"))
        .chain(
            [
                "finished 0 Success",
                "started 1",
                "finished 1 Success",
                "job Success",
                "idle",
            ]
            .map(str::to_owned),
        )
        .collect();
    assert_eq!(
        order, expected,
        "the spooled chunks did not come first, then the held reports in order, then Idle"
    );
    let resent = logs(&stream.received);
    assert_eq!(
        resent.get(&first.seq),
        Some(&first),
        "the unacknowledged first chunk was not resent as it was"
    );
    let delivered: Vec<&Log> = resent.values().collect();
    assert_eq!(text(&delivered), vec!["before", "after", "second"]);
    running.stop().await;
}
