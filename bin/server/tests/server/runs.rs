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
use crate::api::{Api, fixture, send, text};

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
        outputs: Default::default(),
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
        outputs: Default::default(),
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

const PLACED: &str = "run-name: ${{ github.event_name }} by ${{ github.actor }}\n\
                      on: workflow_dispatch\njobs:\n  setup:\n    runs-on: [self-hosted]\n    \
                      outputs:\n      label: ${{ steps.pick.outputs.label }}\n    steps:\n      \
                      - id: pick\n        run: echo label=gpu >> \"$GITHUB_OUTPUT\"\n  build:\n    \
                      needs: setup\n    runs-on: ${{ needs.setup.outputs.label }}\n    steps:\n      \
                      - run: make\n";

async fn dispatch_source(api: &Api, name: &str, source: &str) -> Value {
    let project = api.project().await;
    let workflow = api.workflow(&project, name, source).await;
    let (status, run) = api
        .post(
            &format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"])),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
    assert_eq!(status, 201, "{run}");
    run
}

fn job_named<'a>(page: &'a Value, key: &str) -> &'a Value {
    page["items"]
        .as_array()
        .expect("a list of jobs")
        .iter()
        .find(|job| job["key"] == key)
        .unwrap_or_else(|| panic!("no job `{key}` in {page}"))
}

#[tokio::test]
async fn a_job_runs_where_the_output_its_dependency_reported_places_it() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatch_source(&api, "ci", PLACED).await;
    assert_eq!(run["display_name"], "workflow_dispatch by roman");
    let id = text(&run["id"]);
    let page = read(&api, &format!("/api/v1/runs/{id}/attempts/1/jobs")).await;
    let setup = text(&job_named(&page, "setup")["id"]).to_owned();
    let build = text(&job_named(&page, "build")["id"]).to_owned();

    let (status, token) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": true, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{token}");
    let first = FakeAgent::register(api.address, text(&token["token"]), "runner1", &[]).await;
    let mut connection = first.connect(api.address).await;
    connection.idle();
    let assignment = connection.assignment().await;
    assert_eq!(assignment.job_key, "setup");
    connection.send(Report::Accept(Accept {
        execution_id: assignment.execution_id.clone(),
    }));
    connection.finish_job(
        &assignment.execution_id,
        Conclusion::Success,
        &[("label", "GPU"), ("unsaid", "x")],
    );
    let placed = until(&api, &format!("/api/v1/jobs/{build}"), |job| {
        job["status"] == "queued"
    })
    .await;
    assert_eq!(placed["labels"], json!(["gpu"]));
    assert_eq!(
        read(&api, &format!("/api/v1/jobs/{setup}")).await["outputs"],
        json!({ "label": "GPU" })
    );

    let gpu = FakeAgent::register(api.address, text(&token["token"]), "gpu1", &["gpu"]).await;
    let mut on_gpu = gpu.connect(api.address).await;
    on_gpu.idle();
    let (assignment, documents) = on_gpu.job().await;
    assert_eq!(
        (assignment.job_key.as_str(), assignment.job_id),
        ("build", build)
    );
    assert_eq!(
        Value::Object(documents.contexts.needs),
        json!({ "setup": { "result": "success", "outputs": { "label": "GPU" } } })
    );
}

#[tokio::test]
async fn the_server_stores_the_outputs_an_agent_reports_for_a_step() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatched(&api).await;
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
    connection.send(Report::StepStarted(StepStarted {
        execution_id: execution.clone(),
        step_index: 0,
        started_at: None,
    }));
    connection.send(Report::StepFinished(StepFinished {
        execution_id: execution.clone(),
        step_index: 0,
        completed_at: None,
        outcome: Conclusion::Success.into(),
        conclusion: Conclusion::Success.into(),
        outputs: [
            ("digest".to_owned(), "sha256:0f1e".to_owned()),
            ("lines".to_owned(), "a\nb".to_owned()),
        ]
        .into(),
    }));
    report_step(&connection, &execution, 1, Conclusion::Success);
    connection.finish_job(&execution, Conclusion::Success, &[]);
    until(
        &api,
        &format!("/api/v1/runs/{}/attempts/1/jobs", text(&run["id"])),
        |page| job_named(page, "build")["status"] == "completed",
    )
    .await;
    let stored = crate::strings(
        database.connection(),
        &format!(
            "SELECT concat_ws(' ', index, outputs) AS value FROM bosun.step \
             WHERE execution_id = '{execution}' ORDER BY index"
        ),
    )
    .await;
    assert_eq!(
        stored,
        [r#"0 {"lines": "a\nb", "digest": "sha256:0f1e"}"#, "1 {}"]
    );
}

#[tokio::test]
async fn the_warnings_of_its_revision_are_the_notices_of_a_run() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatch_source(&api, "ci", &fixture("workflows/template-condition.yml")).await;
    let shown = read(&api, &format!("/api/v1/runs/{}", text(&run["id"]))).await;
    let expected: Value = serde_json::from_str(&fixture("api/run.json")).expect("a run");
    assert_eq!(
        (&shown["display_name"], &shown["notices"]),
        (&expected["display_name"], &expected["notices"])
    );
}

#[tokio::test]
async fn an_expression_that_fails_on_the_server_fails_its_job_with_an_error_notice() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatch_source(&api, "ci", &fixture("workflows/timeout-minutes.yml")).await;
    let id = text(&run["id"]);
    let page = read(&api, &format!("/api/v1/runs/{id}/attempts/1/jobs")).await;
    let slow = job_named(&page, "slow");
    assert_eq!(
        (&slow["status"], &slow["conclusion"]),
        (&json!("completed"), &json!("failure"))
    );
    assert_eq!(job_named(&page, "build")["status"], "queued");
    let event: Value = serde_json::from_str(&fixture("events/run-notice.json")).expect("an event");
    let mut expected = json!({ "job_id": slow["id"] });
    for (key, value) in event["data"].as_object().expect("the data of the event") {
        expected[key] = value.clone();
    }
    let shown = read(&api, &format!("/api/v1/runs/{id}")).await;
    assert_eq!(shown["notices"], json!([expected]));
    assert_eq!(run["notices"], shown["notices"]);
}

const MATRIX: &str = "name: ci\non: workflow_dispatch\njobs:\n  setup:\n    runs-on: [self-hosted]\n    \
                      outputs:\n      matrix: ${{ steps.list.outputs.matrix }}\n    steps:\n      \
                      - id: list\n        run: make\n  test:\n    needs: setup\n    \
                      runs-on: [self-hosted]\n    strategy:\n      max-parallel: 1\n      \
                      matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}\n    steps:\n      \
                      - run: make test\n";

fn members_of(page: &Value, group: &Value) -> Vec<Value> {
    page["items"]
        .as_array()
        .expect("a list of jobs")
        .iter()
        .filter(|job| job["group_id"] == group["id"])
        .map(|job| {
            json!([
                job["name"],
                job["matrix"],
                job["status"],
                job["waiting_reason"],
                job["conclusion"]
            ])
        })
        .collect()
}

#[tokio::test]
async fn the_members_of_a_computed_matrix_are_listed_with_their_group_and_run_one_at_a_time() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatch_source(&api, "ci", MATRIX).await;
    let id = text(&run["id"]);
    let jobs_path = format!("/api/v1/runs/{id}/attempts/1/jobs");
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
    let assignment = connection.assignment().await;
    assert_eq!(assignment.job_key, "setup");
    connection.send(Report::Accept(Accept {
        execution_id: assignment.execution_id.clone(),
    }));
    connection.finish_job(
        &assignment.execution_id,
        Conclusion::Success,
        &[("matrix", r#"{"os":["linux","macos"]}"#)],
    );

    let page = until(&api, &jobs_path, |page| {
        page["items"]
            .as_array()
            .is_some_and(|items| items.len() == 4)
    })
    .await;
    let group = job_named(&page, "test").clone();
    assert_eq!(
        (&group["group_id"], &group["matrix"], &group["status"]),
        (&Value::Null, &Value::Null, &json!("pending"))
    );
    assert_eq!(
        members_of(&page, &group),
        [
            json!(["test (linux)", { "os": "linux" }, "queued", null, null]),
            json!(["test (macos)", { "os": "macos" }, "waiting", "max_parallel", null]),
        ]
    );

    for (index, os) in ["linux", "macos"].into_iter().enumerate() {
        connection.idle();
        let (assignment, documents) = connection.job().await;
        assert_eq!(
            (assignment.job_key.as_str(), assignment.job_name.as_str()),
            ("test", format!("test ({os})").as_str())
        );
        assert_eq!(
            (&documents.contexts.matrix, &documents.contexts.strategy),
            (
                &json!({ "os": os }),
                &json!({
                    "fail-fast": true,
                    "job-index": index,
                    "job-total": 2,
                    "max-parallel": 1
                })
            )
        );
        connection.send(Report::Accept(Accept {
            execution_id: assignment.execution_id.clone(),
        }));
        connection.finish_job(&assignment.execution_id, Conclusion::Success, &[]);
    }
    let finished = until(&api, &format!("/api/v1/runs/{id}"), |run| {
        run["status"] == "completed"
    })
    .await;
    assert_eq!(finished["conclusion"], "success");
    let page = read(&api, &jobs_path).await;
    let group = job_named(&page, "test").clone();
    assert_eq!(
        (&group["status"], &group["conclusion"]),
        (&json!("completed"), &json!("success"))
    );
    assert_eq!(
        members_of(&page, &group),
        [
            json!(["test (linux)", { "os": "linux" }, "completed", null, "success"]),
            json!(["test (macos)", { "os": "macos" }, "completed", null, "success"]),
        ]
    );
}

const TIMED_OUT: &str = "on: workflow_dispatch\njobs:\n  slow:\n    runs-on: [self-hosted]\n    \
                         timeout-minutes: 1\n    steps:\n      - run: sleep 120\n  tolerated:\n    \
                         runs-on: [self-hosted]\n    timeout-minutes: 1\n    \
                         continue-on-error: true\n    steps:\n      - run: sleep 120\n  report:\n    \
                         needs: [slow, tolerated]\n    if: always()\n    runs-on: [self-hosted]\n    \
                         steps:\n      - run: echo report\n";

#[tokio::test]
async fn a_job_that_timed_out_is_a_failure_in_needs_unless_it_may_fail() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let run = dispatch_source(&api, "ci", TIMED_OUT).await;
    let id = text(&run["id"]);
    let (status, token) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": false, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{token}");
    let agent = FakeAgent::register(api.address, text(&token["token"]), "runner1", &[]).await;
    let mut connection = agent.connect(api.address).await;
    for _ in 0..2 {
        connection.idle();
        let assignment = connection.assignment().await;
        connection.send(Report::Accept(Accept {
            execution_id: assignment.execution_id.clone(),
        }));
        connection.finish_job(&assignment.execution_id, Conclusion::TimedOut, &[]);
        until(
            &api,
            &format!("/api/v1/jobs/{}", assignment.job_id),
            |job| job["status"] == "completed",
        )
        .await;
    }
    connection.idle();
    let (assignment, documents) = connection.job().await;
    assert_eq!(assignment.job_key, "report");
    assert_eq!(
        Value::Object(documents.contexts.needs),
        json!({
            "slow": { "result": "failure", "outputs": {} },
            "tolerated": { "result": "success", "outputs": {} },
        })
    );
    let page = read(&api, &format!("/api/v1/runs/{id}/attempts/1/jobs")).await;
    for key in ["slow", "tolerated"] {
        assert_eq!(job_named(&page, key)["conclusion"], "timed_out", "{key}");
    }
}
