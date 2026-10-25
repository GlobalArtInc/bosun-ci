use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{
    Conclusion, ForgeToken, JobFinished, Log, RenewForgeToken, RenewedForgeToken,
};
use bosun_proto::assignment::Documents;
use bosun_types::ir::{Action, Step};
use serde_json::json;
use uuid::Uuid;

use crate::server::{FakeServer, Stream};
use crate::{Home, assignment_of, documents, step, text, workspace};

fn run(script: &str) -> Step {
    let mut run = step("");
    run.action = Action::Run {
        script: bosun_expr::template(script).expect("a template"),
    };
    run
}

fn token() -> String {
    format!("ghs_{}", Uuid::now_v7().simple())
}

fn holding(steps: Vec<Step>, token: &str) -> Documents {
    let mut documents = documents(steps);
    documents
        .contexts
        .github
        .insert("token".to_owned(), json!(token));
    documents
        .contexts
        .secrets
        .insert("GITHUB_TOKEN".to_owned(), json!(token));
    documents
        .evaluated
        .env
        .insert("JOB_TOKEN".to_owned(), format!("x-access-token:{token}"));
    documents
}

fn lifetime(seconds: u64) -> Option<prost_types::Duration> {
    prost_types::Duration::try_from(Duration::from_secs(seconds)).ok()
}

async fn renewal(stream: &mut Stream) -> RenewForgeToken {
    stream
        .until(
            "a request for a fresh forge token",
            |message| match message {
                Inbound::RenewForgeToken(renew) => Some(renew.clone()),
                Inbound::JobFinished(finished) => panic!("the job ended first: {finished:?}"),
                _ => None,
            },
        )
        .await
}

async fn finished(stream: &mut Stream) -> JobFinished {
    stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.clone()),
            Inbound::Reject(reject) => panic!("the job was rejected: {reject:?}"),
            _ => None,
        })
        .await
}

fn lines_of(stream: &Stream, execution: &str) -> Vec<String> {
    let chunks: BTreeMap<u64, &Log> = stream
        .received
        .iter()
        .filter_map(|message| match message {
            Inbound::Log(log) if log.execution_id == execution => Some((log.seq, log)),
            _ => None,
        })
        .collect();
    text(&chunks.into_values().collect::<Vec<&Log>>())
}

const RECORD: &str = "printf '%s %s %s\\n' '${{ github.token }}' '${{ secrets.GITHUB_TOKEN }}' \
                      \"$JOB_TOKEN\"";

#[tokio::test(flavor = "multi_thread")]
async fn a_later_step_of_a_long_job_receives_a_fresh_forge_token_and_every_token_is_masked() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let first = token();
    let fresh = token();
    let mut job = assignment_of(&holding(
        vec![
            run(&format!("{RECORD} > first\n{RECORD}\nsleep 6")),
            run(&format!("{RECORD} > second\n{RECORD}")),
        ],
        &first,
    ));
    job.forge_token = Some(Box::new(ForgeToken {
        token: first.clone(),
        expires_in: lifetime(45 * 60 + 5),
    }));
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));

    let asked = renewal(&mut stream).await;
    assert_eq!(asked.execution_id, execution);
    stream.send(Outgoing::RenewedForgeToken(RenewedForgeToken {
        execution_id: execution.clone(),
        token: Some(ForgeToken {
            token: fresh.clone(),
            expires_in: lifetime(3600),
        }),
        error: String::new(),
    }));
    assert_eq!(
        finished(&mut stream).await.conclusion(),
        Conclusion::Success
    );
    let renewals = stream
        .received
        .iter()
        .filter(|message| matches!(message, Inbound::RenewForgeToken(_)))
        .count();
    assert_eq!(renewals, 1);

    let directory = workspace(&home);
    let read = |name: &str| {
        fs::read_to_string(directory.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    assert_eq!(
        read("first"),
        format!("{first} {first} x-access-token:{first}\n")
    );
    assert_eq!(
        read("second"),
        format!("{fresh} {fresh} x-access-token:{fresh}\n")
    );
    assert_eq!(
        lines_of(&stream, &execution),
        ["*** *** x-access-token:***", "*** *** x-access-token:***"]
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_runs_with_the_token_it_holds_when_the_server_has_no_fresh_one() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let held = token();
    let mut job = assignment_of(&holding(vec![run(RECORD)], &held));
    job.forge_token = Some(Box::new(ForgeToken {
        token: held.clone(),
        expires_in: lifetime(60),
    }));
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));

    let asked = renewal(&mut stream).await;
    stream.send(Outgoing::RenewedForgeToken(RenewedForgeToken {
        execution_id: asked.execution_id,
        token: None,
        error: "the forge could not be reached".to_owned(),
    }));
    assert_eq!(
        finished(&mut stream).await.conclusion(),
        Conclusion::Success
    );
    assert_eq!(
        lines_of(&stream, &execution),
        [
            "the forge token could not be renewed, so the step runs with the token it holds: the \
             forge could not be reached",
            "*** *** x-access-token:***",
        ]
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_without_a_forge_token_asks_for_none() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut documents = holding(vec![run(RECORD)], "");
    documents.evaluated.env.clear();
    let job = assignment_of(&documents);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    assert_eq!(
        finished(&mut stream).await.conclusion(),
        Conclusion::Success
    );
    assert!(
        !stream
            .received
            .iter()
            .any(|message| matches!(message, Inbound::RenewForgeToken(_)))
    );
    assert_eq!(lines_of(&stream, &execution), ["  "]);
    running.stop().await;
}
