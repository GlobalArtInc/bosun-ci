use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Conclusion, JobFinished, StepFinished, StepStarted};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::{Connection, FakeAgent};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::WAIT;
use crate::api::{Api, send, text};

const SOURCE: &str = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    \
                      steps:\n      - name: Compile\n        run: make\n      - run: make test\n  \
                      test:\n    needs: build\n    runs-on: [self-hosted]\n    steps:\n      - run: \
                      echo done\n";

async fn dispatched(api: &Api) -> Value {
    let project = api.project().await;
    let workflow = api.workflow(&project, "ci", SOURCE).await;
    let (status, run) = api
        .post(
            &format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"])),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
    assert_eq!(status, 201, "{run}");
    run
}

async fn read(api: &Api, path: &str) -> Value {
    let (status, body) = api.get(path).await;
    assert_eq!(status, 200, "GET {path}: {body}");
    body
}

async fn until(api: &Api, path: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let body = read(api, path).await;
        if done(&body) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "GET {path} did not reach the expected state within {WAIT:?}: {body}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

fn report_step(connection: &Connection, execution: &str, index: u32, conclusion: Conclusion) {
    connection.send(Report::StepStarted(StepStarted {
        execution_id: execution.to_owned(),
        step_index: index,
        started_at: None,
    }));
    connection.send(Report::StepFinished(StepFinished {
        execution_id: execution.to_owned(),
        step_index: index,
        completed_at: None,
        outcome: conclusion.into(),
        conclusion: conclusion.into(),
    }));
}

#[tokio::test]
async fn a_run_its_jobs_and_their_execution_read_as_the_agent_reports_them() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatched(&api).await;
    let id = text(&run["id"]);
    assert_eq!(read(&api, &format!("/api/v1/runs/{id}")).await, run);

    let jobs_path = format!("/api/v1/runs/{id}/attempts/1/jobs");
    let page = read(&api, &jobs_path).await;
    assert_eq!(page["next"], Value::Null);
    let jobs = page["items"].as_array().expect("a list of jobs");
    let summary: Vec<Value> = jobs
        .iter()
        .map(|job| {
            json!([
                job["key"],
                job["status"],
                job["needs"],
                job["execution_id"],
                job["run_id"]
            ])
        })
        .collect();
    assert_eq!(
        summary,
        [
            json!(["build", "queued", [], null, id]),
            json!(["test", "pending", ["build"], null, id]),
        ]
    );
    let build = text(&jobs[0]["id"]).to_owned();
    let test = text(&jobs[1]["id"]).to_owned();
    let first = read(&api, &format!("{jobs_path}?limit=1")).await;
    assert_eq!(first["items"], json!([jobs[0]]));
    assert_eq!(first["next"], build.as_str());
    let second = read(&api, &format!("{jobs_path}?limit=1&cursor={build}")).await;
    assert_eq!(second, json!({ "items": [jobs[1]], "next": null }));
    assert_eq!(read(&api, &format!("/api/v1/jobs/{build}")).await, jobs[0]);

    let (status, token) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": false, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{token}");
    let agent = FakeAgent::register(api.address, text(&token["token"]), "runner1", &[]).await;
    let mut connection = agent.connect(api.address).await;
    connection.idle();
    let execution = connection.assignment().await.execution_id;
    connection.send(Report::Accept(Accept {
        execution_id: execution.clone(),
    }));
    let job = until(&api, &format!("/api/v1/jobs/{build}"), |job| {
        job["status"] == "in_progress"
    })
    .await;
    assert_eq!(job["execution_id"], execution.as_str());
    let started = read(&api, &format!("/api/v1/executions/{execution}")).await;
    assert_eq!(
        (
            &started["job_id"],
            &started["agent_id"],
            &started["number"],
            &started["status"]
        ),
        (
            &json!(build),
            &json!(agent.id().to_string()),
            &json!(1),
            &json!("in_progress")
        )
    );
    let steps: Vec<Value> = started["steps"]
        .as_array()
        .expect("the steps of the execution")
        .iter()
        .map(|step| {
            json!([
                step["index"],
                step["name"],
                step["status"],
                step["execution_id"]
            ])
        })
        .collect();
    assert_eq!(
        steps,
        [
            json!([0, "Compile", "pending", execution]),
            json!([1, "Run make test", "pending", execution]),
        ]
    );

    report_step(&connection, &execution, 0, Conclusion::Success);
    report_step(&connection, &execution, 1, Conclusion::Failure);
    connection.send(Report::JobFinished(JobFinished {
        execution_id: execution.clone(),
        conclusion: Conclusion::Failure.into(),
    }));
    let ended = until(&api, &format!("/api/v1/runs/{id}"), |run| {
        run["status"] == "completed"
    })
    .await;
    assert_eq!(ended["conclusion"], "failure");
    assert!(
        ended["started_at"].is_string() && ended["completed_at"].is_string(),
        "{ended}"
    );
    let finished = read(&api, &format!("/api/v1/executions/{execution}")).await;
    assert_eq!(
        (&finished["status"], &finished["conclusion"]),
        (&json!("completed"), &json!("failure"))
    );
    let conclusions: Vec<Value> = finished["steps"]
        .as_array()
        .expect("the steps of the execution")
        .iter()
        .map(|step| json!([step["status"], step["outcome"], step["conclusion"]]))
        .collect();
    assert_eq!(
        conclusions,
        [
            json!(["completed", "success", "success"]),
            json!(["completed", "failure", "failure"]),
        ]
    );
    let page = read(&api, &jobs_path).await;
    let summary: Vec<Value> = page["items"]
        .as_array()
        .expect("a list of jobs")
        .iter()
        .map(|job| {
            json!([
                job["id"],
                job["status"],
                job["conclusion"],
                job["execution_id"]
            ])
        })
        .collect();
    assert_eq!(
        summary,
        [
            json!([build, "completed", "failure", execution]),
            json!([test, "completed", "skipped", null]),
        ]
    );
}

#[tokio::test]
async fn runs_attempts_jobs_and_executions_that_do_not_exist_are_not_found() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatched(&api).await;
    let id = text(&run["id"]);
    let stranger = Uuid::now_v7();
    for (path, message) in [
        (
            "/api/v1/runs/yesterday".to_owned(),
            "there is no run `yesterday`".to_owned(),
        ),
        (
            format!("/api/v1/runs/{stranger}"),
            format!("there is no run `{stranger}`"),
        ),
        (
            format!("/api/v1/runs/{stranger}/attempts/1/jobs"),
            format!("there is no run `{stranger}`"),
        ),
        (
            format!("/api/v1/runs/{id}/attempts/2/jobs"),
            format!("there is no attempt `2` of run `{id}`"),
        ),
        (
            format!("/api/v1/runs/{id}/attempts/first/jobs"),
            format!("there is no attempt `first` of run `{id}`"),
        ),
        (
            format!("/api/v1/jobs/{stranger}"),
            format!("there is no job `{stranger}`"),
        ),
        (
            "/api/v1/jobs/build".to_owned(),
            "there is no job `build`".to_owned(),
        ),
        (
            format!("/api/v1/executions/{stranger}"),
            format!("there is no execution `{stranger}`"),
        ),
    ] {
        let (status, missing) = api.get(&path).await;
        assert_eq!(status, 404, "{path}: {missing}");
        assert_eq!(missing["error"]["code"], "not_found", "{path}");
        assert_eq!(missing["error"]["message"], message.as_str(), "{path}");
    }

    let jobs = format!("/api/v1/runs/{id}/attempts/1/jobs");
    let (status, refused) = api.get(&format!("{jobs}?status=queued")).await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["code"], "invalid_input");
    let (status, refused) = api.get(&format!("{jobs}?limit=0")).await;
    assert_eq!(status, 422, "{refused}");
    let (status, _) = send(api.request(Method::POST, &format!("/api/v1/runs/{id}"))).await;
    assert_eq!(status, 404);
    let (status, unauthenticated) =
        send(crate::client().get(format!("http://{}/api/v1/runs/{id}", api.address))).await;
    assert_eq!(status, 401, "{unauthenticated}");
}
