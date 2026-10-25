use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{AgentMessage, Annotation, AnnotationKind, Conclusion, StepFinished};
use bosun_types::ir::{Action, Step};
use prost::Message as _;
use serde_json::json;

use crate::server::{FakeServer, Stream};
use crate::{Home, assignment_of, documents, step};

const SECRET: &str = "s3cr3t-t0ken";

const SUMMARY_LIMIT: usize = 1024 * 1024;

fn run(text: &str) -> Step {
    let mut run = step("");
    run.action = Action::Run {
        script: bosun_expr::template(text).expect("a template"),
    };
    run
}

async fn steps_of(stream: &mut Stream, count: usize) -> Vec<StepFinished> {
    stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.conclusion()),
            Inbound::Reject(reject) => panic!("the job was rejected: {reject:?}"),
            _ => None,
        })
        .await;
    let finished: Vec<StepFinished> = stream
        .received
        .iter()
        .filter_map(|message| match message {
            Inbound::StepFinished(step) => Some(step.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), count, "{finished:#?}");
    finished
}

#[tokio::test(flavor = "multi_thread")]
async fn annotations_and_the_step_summary_reach_the_server_masked_in_the_step_report() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let printing = run(
        "echo \"::warning file=src/${{ secrets.TOKEN }}.ts,line=3,title=about \
         ${{ secrets.TOKEN }}::the token is ${{ secrets.TOKEN }}\"\n\
         printf '## Token\\n\\n%s\\n' '${{ secrets.TOKEN }}' >> \"$GITHUB_STEP_SUMMARY\"",
    );
    let mut job = documents(vec![printing]);
    job.contexts
        .secrets
        .insert("TOKEN".to_owned(), json!(SECRET));
    stream.send(Outgoing::Assignment(assignment_of(&job)));
    let finished = steps_of(&mut stream, 1).await;
    assert_eq!(finished[0].conclusion(), Conclusion::Success);
    assert_eq!(
        finished[0].annotations,
        [Annotation {
            kind: AnnotationKind::Warning.into(),
            message: "the token is ***".to_owned(),
            title: Some("about ***".to_owned()),
            file: Some("src/***.ts".to_owned()),
            line: Some(3),
            end_line: None,
            column: None,
            end_column: None,
        }]
    );
    assert_eq!(finished[0].summary, "## Token\n\n***\n");
    for message in &stream.received {
        let encoded = AgentMessage {
            message: Some(message.clone()),
        }
        .encode_to_vec();
        assert!(
            !encoded
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes()),
            "the secret left the agent in {message:?}"
        );
    }
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_summary_of_more_than_1_mib_arrives_cut_at_1_mib_with_a_notice() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let job = documents(vec![
        run(&format!(
            "head -c {SUMMARY_LIMIT} /dev/zero | tr '\\0' x > \"$GITHUB_STEP_SUMMARY\""
        )),
        run(&format!(
            "head -c {} /dev/zero | tr '\\0' y > \"$GITHUB_STEP_SUMMARY\"",
            SUMMARY_LIMIT + 10
        )),
    ]);
    stream.send(Outgoing::Assignment(assignment_of(&job)));
    let finished = steps_of(&mut stream, 2).await;
    assert_eq!(finished[0].summary, "x".repeat(SUMMARY_LIMIT));
    assert_eq!(finished[0].annotations, []);
    assert_eq!(finished[1].summary, "y".repeat(SUMMARY_LIMIT));
    assert_eq!(
        finished[1]
            .annotations
            .iter()
            .map(|annotation| (annotation.kind(), annotation.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            AnnotationKind::Notice,
            "the step summary is longer than 1 MiB, so only its first 1 MiB is kept"
        )]
    );
    running.stop().await;
}
