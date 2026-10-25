use std::fs;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Assignment, Conclusion, Log, Reject};
use bosun_types::ir::{Action, ActionReference, Condition, Expr, Segment, Shell, Span, Template};
use serde_json::{Value, json};

use crate::server::{FakeServer, Stream, logs};
use crate::{Home, JOB_ID, NAME, assignment, assignment_of, documents, step, text, workspace};

#[tokio::test(flavor = "multi_thread")]
async fn a_job_runs_its_steps_in_the_workspace_and_reports_each_result_and_its_logs() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut first = step("echo \"$GREETING from $STEP\"\npwd");
    first
        .env
        .insert("STEP".to_owned(), Template::text("the first step"));
    let mut documents = documents(vec![
        first,
        step("echo out\necho err >&2\nexit 3"),
        step("echo never"),
    ]);
    documents
        .evaluated
        .env
        .insert("GREETING".to_owned(), "hello".to_owned());
    let job = assignment_of(&documents);
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

async fn rejected(stream: &mut Stream, job: Assignment) -> Reject {
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
    stream.idle().await;
    reject
}

#[tokio::test(flavor = "multi_thread")]
async fn every_step_sees_the_variables_of_its_context_with_the_facts_of_the_agent() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut documents = documents(vec![step(
        "printf '%s\\n' \"$GITHUB_EVENT_NAME\" \"$GITHUB_JOB\" \"$GITHUB_ACTION\" \
         \"$GITHUB_WORKSPACE\" \"$RUNNER_NAME\" \"$RUNNER_TEMP\" \"$RUNNER_TOOL_CACHE\" \
         \"$BOSUN_RUN_URL\" \"$BOSUN_JOB_ID\" \"$KEPT\"\n\
         cp \"$GITHUB_EVENT_PATH\" \"$GITHUB_WORKSPACE/event.json\"",
    )]);
    documents
        .evaluated
        .env
        .insert("KEPT".to_owned(), "the job's".to_owned());
    let job = assignment_of(&documents);
    stream.send(Outgoing::Assignment(job));
    let finished = stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.clone()),
            Inbound::Reject(reject) => panic!("the job was rejected: {reject:?}"),
            _ => None,
        })
        .await;
    assert_eq!(finished.conclusion(), Conclusion::Success);
    let chunks = logs(&stream.received);
    let lines = text(&chunks.values().collect::<Vec<&Log>>());
    let dir = fs::canonicalize(home.dir()).expect("the agent directory");
    assert_eq!(
        lines,
        [
            "workflow_dispatch".to_owned(),
            "build".to_owned(),
            "__run".to_owned(),
            workspace(&home).display().to_string(),
            NAME.to_owned(),
            dir.join("work/_temp").display().to_string(),
            dir.join("tools").display().to_string(),
            "https://ci.example.org/runs/0192f3a4-5b6c-7d8e-9f01-000000000007".to_owned(),
            JOB_ID.to_owned(),
            "the job's".to_owned(),
        ]
    );
    let payload: Value = serde_json::from_slice(
        &fs::read(workspace(&home).join("event.json")).expect("the copied payload"),
    )
    .expect("JSON");
    assert_eq!(
        payload,
        json!({ "inputs": null, "ref": null, "workflow": "ci" })
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assignment_with_a_field_the_agent_does_not_know_is_rejected_naming_the_field() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let known = assignment(vec![step("echo")]);
    let mut job: Value = serde_json::from_str(&known.job).expect("the job as JSON");
    job["container"] = json!({ "image": "debian:13" });
    let mut contexts: Value = serde_json::from_str(&known.contexts).expect("the contexts");
    contexts["runner"] = json!({});
    let mut evaluated: Value = serde_json::from_str(&known.evaluated).expect("the values");
    evaluated["services"] = json!({});
    let mut step_field: Value = serde_json::from_str(&known.job).expect("the job as JSON");
    step_field["kind"]["steps"][0]["retry"] = json!(3);
    for (document, field, assignment) in [
        (
            "job",
            "container",
            Assignment {
                job: job.to_string(),
                ..assignment(vec![step("echo")])
            },
        ),
        (
            "contexts",
            "runner",
            Assignment {
                contexts: contexts.to_string(),
                ..assignment(vec![step("echo")])
            },
        ),
        (
            "evaluated",
            "services",
            Assignment {
                evaluated: evaluated.to_string(),
                ..assignment(vec![step("echo")])
            },
        ),
        (
            "job",
            "retry",
            Assignment {
                job: step_field.to_string(),
                ..assignment(vec![step("echo")])
            },
        ),
    ] {
        let reject = rejected(&mut stream, assignment).await;
        let named = format!(
            "the document `{document}` of the assignment is not one this agent reads: unknown \
             field `{field}`"
        );
        assert!(reject.reason.starts_with(&named), "{}", reject.reason);
    }
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

    let span = Span {
        line: 1,
        column: 1,
        end_line: 1,
        end_column: 1,
    };
    let mut uses = step("echo");
    uses.action = Action::Uses(ActionReference {
        owner: "actions".to_owned(),
        repository: "checkout".to_owned(),
        path: None,
        r#ref: "v4".to_owned(),
    });
    let mut expression = step("echo");
    expression.env.insert(
        "SHA".to_owned(),
        Template::new(vec![Segment::Expression(Expr::Context {
            name: "github".to_owned(),
            span,
        })]),
    );
    let mut condition = step("echo");
    condition.r#if = Some(Condition::Expression(Expr::Boolean { value: true, span }));
    let mut bad_shell = step("echo");
    bad_shell.shell = Some(Shell::Custom("perl -w".to_owned()));
    let mut bad_slug = assignment(vec![step("echo")]);
    bad_slug.project = "../escape".to_owned();
    for (job, reason) in [
        (
            assignment(vec![step("echo"), uses]),
            "step 1 uses the action `actions/checkout@v4`, which agents run from M1 step 33",
        ),
        (
            assignment(vec![expression]),
            "step 0 holds an expression in `env.SHA`, which agents evaluate from M1 step 21",
        ),
        (
            assignment(vec![condition]),
            "step 0 sets `if`, which agents apply from M1 step 21",
        ),
        (
            assignment(vec![bad_shell]),
            "the shell of step 0 is `perl -w`, which is neither",
        ),
        (bad_slug, "`../escape` is not the slug of a project"),
    ] {
        let reject = rejected(&mut stream, job).await;
        assert!(reject.reason.contains(reason), "{}", reject.reason);
    }

    let busy = assignment(vec![step("echo ready\nsleep 5")]);
    let other = assignment(vec![step("echo")]);
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
