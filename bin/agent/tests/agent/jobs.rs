use std::collections::BTreeMap;
use std::fs;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Assignment, Conclusion, Log, Reject};
use bosun_types::ir::{Action, ActionReference, Evaluable, Shell, Template};
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
                assert!(finished.outputs.is_empty());
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
            "finished 2 Skipped",
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
        (
            "actions",
            "inputs",
            Assignment {
                actions: json!([{ "inputs": {} }]).to_string(),
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

    let mut uses = step("echo");
    uses.action = Action::Uses(ActionReference {
        owner: "actions".to_owned(),
        repository: "checkout".to_owned(),
        path: None,
        r#ref: "v4".to_owned(),
    });
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

#[tokio::test(flavor = "multi_thread")]
async fn a_job_reports_the_outputs_of_its_steps_the_steps_its_conditions_skip_and_its_outputs() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let template = |text: &str| bosun_expr::template(text).expect("a template");
    let run = |text: &str| {
        let mut run = step("");
        run.action = Action::Run {
            script: template(text),
        };
        run
    };
    let mut version = run(
        "echo \"version=${{ github.job }}-1\" >> \"$GITHUB_OUTPUT\"\n\
         printf 'notes<<EOF\\nfirst\\nsecond\\nEOF\\n' >> \"$GITHUB_OUTPUT\"\n\
         exit 3",
    );
    version.key = Some("version".parse().expect("a step id"));
    version.continue_on_error = Evaluable::Literal(true);
    let mut skipped = step("echo never");
    skipped.r#if = bosun_expr::condition("failure()").expect("a condition");
    let mut reading =
        run("echo \"${{ steps.version.outputs.version }} ${{ steps.version.outcome }}\"");
    reading.r#if = bosun_expr::condition("${{ success() }}").expect("a condition");
    let mut documents = documents(vec![version, skipped, reading]);
    documents.job.outputs = [
        (
            "version".to_owned(),
            template("${{ steps.version.outputs.version }}"),
        ),
        (
            "notes".to_owned(),
            template("${{ steps.version.outputs.notes }}"),
        ),
        ("empty".to_owned(), Template::text("")),
    ]
    .into();
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
    assert_eq!(
        finished.outputs,
        [
            ("version".to_owned(), "build-1".to_owned()),
            ("notes".to_owned(), "first\nsecond".to_owned()),
        ]
        .into()
    );
    let started: Vec<u32> = stream
        .received
        .iter()
        .filter_map(|message| match message {
            Inbound::StepStarted(started) => Some(started.step_index),
            _ => None,
        })
        .collect();
    assert_eq!(started, [0, 2]);
    let reports: Vec<String> = stream
        .received
        .iter()
        .filter_map(|message| match message {
            Inbound::StepFinished(step) => {
                let outputs: BTreeMap<&String, &String> = step.outputs.iter().collect();
                Some(format!(
                    "{} {:?} {:?} {outputs:?}",
                    step.step_index,
                    step.outcome(),
                    step.conclusion()
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        reports,
        [
            "0 Failure Success {\"notes\": \"first\\nsecond\", \"version\": \"build-1\"}",
            "1 Skipped Skipped {}",
            "2 Success Success {}",
        ]
    );
    let chunks = logs(&stream.received);
    let lines = text(
        &chunks
            .values()
            .filter(|log| log.step_index == 2)
            .collect::<Vec<&Log>>(),
    );
    assert_eq!(lines, ["build-1 failure"]);
    running.stop().await;
}
