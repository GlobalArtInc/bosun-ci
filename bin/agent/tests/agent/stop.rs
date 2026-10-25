use bosun_proto::agent::v1::Conclusion;
use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use rustix::process::Signal;

use crate::server::{FakeServer, logs};
use crate::{Home, assignment, gone, pid, step, text};

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_or_sigint_cancels_the_running_job_reports_it_and_stops_the_agent() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    for signal in [Signal::TERM, Signal::INT] {
        let running = home.start();
        let mut stream = server.accept().await;
        stream.welcome(server.agent_id());
        stream.idle().await;

        let job = assignment(vec![
            step(0, "sleep 600 &\necho \"sleeping $!\"\nwait"),
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

        running.signal(signal);
        let step = stream
            .until("the end of the step", |message| match message {
                Inbound::StepFinished(finished) => Some(finished.clone()),
                _ => None,
            })
            .await;
        assert_eq!(
            (step.step_index, step.outcome(), step.conclusion()),
            (0, Conclusion::Cancelled, Conclusion::Cancelled),
            "{signal:?}"
        );
        let finished = stream
            .until("the end of the job", |message| match message {
                Inbound::JobFinished(finished) => Some(finished.clone()),
                Inbound::StepStarted(started) => {
                    panic!("a step started after {signal:?}: {started:?}")
                }
                _ => None,
            })
            .await;
        assert_eq!(finished.execution_id, execution);
        assert_eq!(finished.conclusion(), Conclusion::Cancelled, "{signal:?}");

        let of_this_job: Vec<Inbound> = stream
            .received
            .iter()
            .filter(|message| matches!(message, Inbound::Log(log) if log.execution_id == execution))
            .cloned()
            .collect();
        let chunks = logs(&of_this_job);
        let after = stream.closed().await;
        assert!(
            after.is_empty(),
            "the stopping agent sent more after the end of its job: {after:#?}"
        );
        let output = running.exited().await;
        assert!(
            output.iter().any(|line| line["message"] == "stopped"),
            "{signal:?}: {output:#?}"
        );
        let sleeping = pid(&text(&chunks.values().collect::<Vec<_>>()), "sleeping");
        assert!(
            gone(sleeping).await,
            "the process {sleeping:?} outlived the agent stopped by {signal:?}"
        );
    }
}
