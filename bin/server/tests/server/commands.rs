use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{
    Accept, Conclusion, ControlKind, JobFinished, StepFinished, StepStarted,
};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::FakeAgent;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::api::{Api, fixture, send, text};
use crate::{WAIT, client, strings};

const SLEEPS: &str = "name: ci\non: workflow_dispatch\njobs:\n  sleep:\n    runs-on: [self-hosted]\n    steps:\n      - name: Sleep\n        run: sleep 600\n  after:\n    needs: sleep\n    runs-on: [self-hosted]\n    steps:\n      - run: echo done\n";

async fn dispatched(api: &Api, source: &str) -> String {
    let project = api.project().await;
    let workflow = api.workflow(&project, "ci", source).await;
    let (status, run) = api
        .post(
            &format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"])),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
    assert_eq!(status, 201, "{run}");
    text(&run["id"]).to_owned()
}

async fn job_id(database: &TestDatabase, run: &str, key: &str) -> String {
    strings(
        database.connection(),
        &format!(
            "SELECT j.id::text AS value FROM bosun.job j JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.key = '{key}'"
        ),
    )
    .await
    .pop()
    .unwrap_or_else(|| panic!("no job `{key}` in run {run}"))
}

async fn jobs(database: &TestDatabase, run: &str) -> Vec<String> {
    strings(
        database.connection(),
        &format!(
            "SELECT concat_ws(' ', j.key, j.status, j.conclusion, j.conclusion_source) AS value \
             FROM bosun.job j JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' ORDER BY j.key"
        ),
    )
    .await
}

async fn run_status(database: &TestDatabase, run: &str) -> String {
    strings(
        database.connection(),
        &format!(
            "SELECT concat_ws(' ', status, conclusion) AS value FROM bosun.run WHERE id = '{run}'"
        ),
    )
    .await
    .pop()
    .expect("the run")
}

async fn kinds(database: &TestDatabase, run: &str) -> Vec<String> {
    strings(
        database.connection(),
        &format!("SELECT kind AS value FROM bosun.run_event WHERE run_id = '{run}' ORDER BY seq"),
    )
    .await
}

async fn last_event(database: &TestDatabase, run: &str) -> Value {
    let found = strings(
        database.connection(),
        &format!(
            "SELECT json_build_object('kind', kind, 'job_id', job_id, 'data', data)::text AS value \
             FROM bosun.run_event WHERE run_id = '{run}' ORDER BY seq DESC LIMIT 1"
        ),
    )
    .await;
    serde_json::from_str(&found[0]).expect("an event as JSON")
}

async fn until(database: &TestDatabase, sql: &str, expected: &[&str]) {
    let deadline = Instant::now() + WAIT;
    loop {
        let found = strings(database.connection(), sql).await;
        if found == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{sql} gave {found:?} and not {expected:?} within {WAIT:?}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn run_cancel_and_job_cancel_go_through_the_command_endpoint_and_refusals_are_recorded() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatched(&api, &fixture("workflows/two-jobs.yml")).await;
    let commands = format!("/api/v1/runs/{run}/commands");
    let build = job_id(&database, &run, "build").await;

    let request = json!({
        "type": "job.cancel",
        "target": { "attempt": 1, "job": build, "step": null },
        "reason": "not today",
    });
    let (status, accepted) = api.post(&commands, &request).await;
    assert_eq!(status, 201, "{accepted}");
    assert_eq!(accepted["kind"], "command.accepted");
    assert_eq!(accepted["run_id"], run.as_str());
    assert_eq!(accepted["job_id"], build.as_str());
    assert_eq!(accepted["actor"]["kind"], "token");
    assert_eq!(accepted["data"], json!({ "command": request }));
    assert_eq!(
        jobs(&database, &run).await,
        [
            "build completed cancelled natural",
            "test completed skipped natural"
        ]
    );
    assert_eq!(run_status(&database, &run).await, "completed cancelled");
    assert_eq!(
        kinds(&database, &run).await,
        [
            "run.created",
            "attempt.created",
            "job.created",
            "job.created",
            "job.status",
            "command.accepted",
            "job.status",
            "job.status",
            "run.status",
        ]
    );

    let cancel: Value =
        serde_json::from_str(&fixture("api/command-run-cancel.json")).expect("a command");
    let (status, refused) = api.post(&commands, &cancel).await;
    assert_eq!(status, 409, "{refused}");
    let expected: Value =
        serde_json::from_str(&fixture("events/command-refused.json")).expect("an event");
    assert_eq!(
        refused,
        json!({
            "error": {
                "code": "invalid_state",
                "message": expected["data"]["message"],
                "fields": [],
                "diagnostics": [],
            }
        })
    );
    assert_eq!(
        last_event(&database, &run).await,
        json!({ "kind": "command.refused", "job_id": null, "data": expected["data"] })
    );
    let (status, refused) = api.post(&commands, &request).await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "job `build` is completed; `job.cancel` needs a job that is not completed"
    );
    assert_eq!(last_event(&database, &run).await["job_id"], build.as_str());
}

#[tokio::test]
async fn a_command_the_endpoint_cannot_take_is_refused_before_it_is_recorded() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatched(&api, &fixture("workflows/two-jobs.yml")).await;
    let commands = format!("/api/v1/runs/{run}/commands");
    let before = kinds(&database, &run).await;
    let stranger = Uuid::now_v7();

    for (body, field) in [
        (
            json!({ "type": "job.cancel", "target": { "attempt": null, "job": null, "step": null }, "reason": null }),
            "target.job",
        ),
        (
            json!({ "type": "job.cancel", "target": { "attempt": null, "job": stranger, "step": null }, "reason": null }),
            "target.job",
        ),
        (
            json!({ "type": "run.cancel", "target": { "attempt": 2, "job": null, "step": null }, "reason": null }),
            "target.attempt",
        ),
        (
            json!({ "type": "run.cancel", "target": { "attempt": null, "job": null, "step": stranger }, "reason": null }),
            "target.step",
        ),
    ] {
        let (status, refused) = api.post(&commands, &body).await;
        assert_eq!(status, 422, "{body}: {refused}");
        assert_eq!(refused["error"]["code"], "invalid_input");
        assert_eq!(refused["error"]["fields"][0]["field"], field, "{refused}");
    }
    let (status, refused) = api
        .post(
            &commands,
            &json!({ "type": "run.pause", "target": { "attempt": null, "job": null, "step": null }, "reason": null }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(kinds(&database, &run).await, before);

    for path in [
        "/api/v1/runs/yesterday/commands".to_owned(),
        format!("/api/v1/runs/{stranger}/commands"),
    ] {
        let (status, missing) = api
            .post(&path, &json!({ "type": "run.cancel", "target": { "attempt": null, "job": null, "step": null }, "reason": null }))
            .await;
        assert_eq!(status, 404, "{path}: {missing}");
        assert_eq!(missing["error"]["code"], "not_found");
    }
    let (status, _) = send(api.request(Method::GET, &commands)).await;
    assert_eq!(status, 404);
    let (status, unauthenticated) = send(
        client()
            .post(format!("http://{}{commands}", api.address))
            .json(&json!({ "type": "run.cancel", "target": { "attempt": null, "job": null, "step": null }, "reason": null })),
    )
    .await;
    assert_eq!(status, 401, "{unauthenticated}");
    assert_eq!(kinds(&database, &run).await, before);
}

#[tokio::test]
async fn a_command_repeated_with_its_idempotency_key_returns_the_first_event() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatched(&api, &fixture("workflows/two-jobs.yml")).await;
    let commands = format!("/api/v1/runs/{run}/commands");
    let cancel: Value =
        serde_json::from_str(&fixture("api/command-run-cancel.json")).expect("a command");

    let mut answers = Vec::new();
    for _ in 0..2 {
        answers.push(
            send(
                api.request(Method::POST, &commands)
                    .header("idempotency-key", "cancel-1")
                    .json(&cancel),
            )
            .await,
        );
    }
    assert_eq!(answers[0].0, 201, "{}", answers[0].1);
    assert_eq!(answers[0], answers[1]);
    assert_eq!(
        kinds(&database, &run)
            .await
            .iter()
            .filter(|kind| kind.starts_with("command."))
            .collect::<Vec<_>>(),
        ["command.accepted"]
    );
}

#[tokio::test]
async fn a_cancel_reaches_the_agent_running_the_job_and_the_run_ends_cancelled_when_it_stops() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let (status, token) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": false, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{token}");
    let fake = FakeAgent::register(api.address, text(&token["token"]), "runner1-1", &[]).await;
    let mut connection = fake.connect(api.address).await;
    connection.beat_every(Duration::from_secs(1));
    let run = dispatched(&api, SLEEPS).await;
    let sleeping = job_id(&database, &run, "sleep").await;
    connection.idle();
    let assignment = connection.assignment().await;
    assert_eq!(assignment.job_id, sleeping);
    let execution = assignment.execution_id;

    connection.send(Report::Accept(Accept {
        execution_id: execution.clone(),
    }));
    until(
        &database,
        &format!("SELECT status AS value FROM bosun.job WHERE id = '{sleeping}'"),
        &["in_progress"],
    )
    .await;
    assert_eq!(run_status(&database, &run).await, "in_progress");
    connection.send(Report::StepStarted(StepStarted {
        execution_id: execution.clone(),
        step_index: 0,
        started_at: None,
    }));
    until(
        &database,
        &format!(
            "SELECT concat_ws(' ', name, status) AS value FROM bosun.step \
             WHERE execution_id = '{execution}'"
        ),
        &["Sleep in_progress"],
    )
    .await;

    let (status, accepted) = api
        .post(
            &format!("/api/v1/runs/{run}/commands"),
            &json!({ "type": "run.cancel", "target": { "attempt": null, "job": null, "step": null }, "reason": null }),
        )
        .await;
    assert_eq!(status, 201, "{accepted}");
    assert_eq!(
        jobs(&database, &run).await,
        ["after completed cancelled natural", "sleep in_progress"]
    );
    let control = connection.control().await;
    assert_eq!(control.execution_id, execution);
    assert_eq!(control.kind(), ControlKind::Cancel);
    connection.acknowledge(&control, true);
    connection.send(Report::StepFinished(StepFinished {
        execution_id: execution.clone(),
        step_index: 0,
        completed_at: None,
        outcome: Conclusion::Cancelled.into(),
        conclusion: Conclusion::Cancelled.into(),
    }));
    connection.send(Report::JobFinished(JobFinished {
        execution_id: execution.clone(),
        conclusion: Conclusion::Cancelled.into(),
        outputs: Default::default(),
    }));

    until(
        &database,
        &format!(
            "SELECT concat_ws(' ', status, conclusion) AS value FROM bosun.run WHERE id = '{run}'"
        ),
        &["completed cancelled"],
    )
    .await;
    assert_eq!(
        jobs(&database, &run).await,
        [
            "after completed cancelled natural",
            "sleep completed cancelled natural"
        ]
    );
    assert_eq!(
        strings(
            database.connection(),
            &format!(
                "SELECT concat_ws(' ', status, conclusion) AS value FROM bosun.execution \
                 WHERE id = '{execution}' UNION ALL \
                 SELECT concat_ws(' ', status, outcome, conclusion) FROM bosun.step \
                 WHERE execution_id = '{execution}'"
            ),
        )
        .await,
        ["completed cancelled", "completed cancelled cancelled"]
    );
    assert_eq!(
        strings(
            database.connection(),
            &format!(
                "SELECT DISTINCT actor->>'name' AS value FROM bosun.run_event \
                 WHERE run_id = '{run}' AND kind = 'step.created'"
            ),
        )
        .await,
        ["runner1-1"]
    );
}
