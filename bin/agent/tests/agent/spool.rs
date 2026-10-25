use std::collections::BTreeMap;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Conclusion, Log};

use crate::server::{FakeServer, logs};
use crate::{Home, assignment, step, text};

#[tokio::test(flavor = "multi_thread")]
async fn unacknowledged_log_chunks_survive_an_agent_restart_and_are_delivered_once() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;

    let first = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;
    let job = assignment(vec![step(
        0,
        "for batch in 1 2 3 4; do\n\
         for line in 1 2 3; do echo \"batch $batch line $line\"; done\n\
         sleep 0.5\n\
         done\n",
    )]);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    let finished = stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.clone()),
            _ => None,
        })
        .await;
    assert_eq!(finished.conclusion(), Conclusion::Success);
    let before: BTreeMap<u64, Log> = logs(&stream.received);
    assert!(
        before.len() >= 3,
        "the job produced only {} chunks: {before:#?}",
        before.len()
    );
    let first_seq = *before.keys().next().expect("a chunk");
    stream.acknowledge(&execution, first_seq);
    stream.barrier().await;
    first.kill().await;

    let second = home.start();
    let mut stream = server.accept().await;
    assert!(
        stream.hello.executions.is_empty(),
        "the restarted agent claims executions it does not run: {:?}",
        stream.hello.executions
    );
    stream.welcome(server.agent_id());
    stream.idle().await;
    let resent = logs(&stream.received);
    let expected: BTreeMap<u64, Log> = before
        .iter()
        .filter(|(seq, _)| **seq != first_seq)
        .map(|(seq, log)| (*seq, log.clone()))
        .collect();
    assert_eq!(
        resent, expected,
        "the restarted agent did not resend exactly the unacknowledged chunks"
    );
    let resent_count = stream
        .received
        .iter()
        .filter(|message| matches!(message, Inbound::Log(_)))
        .count();
    assert_eq!(resent_count, expected.len(), "a chunk was resent twice");
    let last = *resent.keys().next_back().expect("a resent chunk");
    stream.acknowledge(&execution, last);
    stream.barrier().await;
    second.stop().await;
    stream.closed().await;

    let third = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;
    assert!(
        !stream
            .received
            .iter()
            .any(|message| matches!(message, Inbound::Log(_))),
        "acknowledged chunks were delivered again: {:#?}",
        stream.received
    );
    third.stop().await;

    let delivered: Vec<&Log> = before.values().collect();
    let wanted: Vec<String> = (1..=4)
        .flat_map(|batch| (1..=3).map(move |line| format!("batch {batch} line {line}")))
        .collect();
    assert_eq!(
        text(&delivered),
        wanted,
        "the log is not every line exactly once"
    );
}
