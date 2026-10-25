use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Conclusion, Drain};

use crate::server::FakeServer;
use crate::{Home, assignment, step};

#[tokio::test(flavor = "multi_thread")]
async fn a_drained_agent_finishes_the_job_it_runs_and_stays_connected() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let gate = tempfile::tempdir().expect("a directory for the gate");
    let opened = gate.path().join("open");
    let mut running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let job = assignment(vec![step(&format!(
        "while [ ! -e '{}' ]; do sleep 0.05; done\necho finished",
        opened.display()
    ))]);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    stream
        .until("the start of the step", |message| {
            matches!(message, Inbound::StepStarted(_)).then_some(())
        })
        .await;
    stream.send(Outgoing::Drain(Drain {}));
    let logged = running
        .logged(
            "the server drained this agent: it finishes the job it runs and is offered no other \
             until the drain is lifted",
        )
        .await;
    assert_eq!(logged["execution_id"], execution.as_str());

    std::fs::write(&opened, b"").expect("the gate opens");
    let finished = stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.clone()),
            _ => None,
        })
        .await;
    assert_eq!(finished.execution_id, execution);
    assert_eq!(finished.conclusion(), Conclusion::Success);
    stream.idle().await;

    stream.send(Outgoing::Drain(Drain {}));
    running
        .logged("the server drained this agent: it is offered no job until the drain is lifted")
        .await;
    stream.barrier().await;
    running.stop().await;
}
