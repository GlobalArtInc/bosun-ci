use bosun_testkit::TestDatabase;
use reqwest::Method;
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::api::{Api, fixture, send, text};
use crate::strings;

const FIRST: &str = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      - run: cargo build\n";

const SECOND: &str = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      - run: cargo build --workspace\n";

async fn count(database: &TestDatabase, table: &str) -> usize {
    strings(
        database.connection(),
        &format!("SELECT count(*)::text AS value FROM bosun.{table}"),
    )
    .await[0]
        .parse()
        .expect("a count")
}

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items in {page}"))
        .iter()
        .map(|item| text(&item["id"]).to_owned())
        .collect()
}

#[tokio::test]
async fn an_invalid_workflow_is_refused_with_its_diagnostics_and_creates_no_revision() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let workflows = format!("/api/v1/projects/{}/workflows", text(&project["id"]));

    let (status, refused) = api
        .post(
            &workflows,
            &json!({ "name": "ci", "source": fixture("workflows/unknown-key.yml") }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    let mut expected: Value =
        serde_json::from_str(&fixture("diagnostics/unknown-key.json")).expect("a diagnostic");
    expected["file"] = json!("ci");
    assert_eq!(
        refused,
        json!({
            "error": {
                "code": "invalid_input",
                "message": "the workflow is not valid",
                "fields": [{ "field": "source", "message": "the workflow has 1 error" }],
                "diagnostics": [expected],
            }
        })
    );
    let (status, refused) = api
        .post(
            &workflows,
            &json!({ "name": "ci", "source": fixture("workflows/needs-cycle.yml") }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["diagnostics"][0]["code"], "needs_cycle");
    let (status, refused) = api
        .post(
            &workflows,
            &json!({ "name": "CI pipeline", "source": FIRST }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["fields"][0]["field"], "name");
    assert_eq!(
        (
            count(&database, "workflow").await,
            count(&database, "workflow_revision").await
        ),
        (0, 0)
    );
    assert_eq!(
        api.get(&workflows).await,
        (200, json!({ "items": [], "next": null }))
    );

    let workflow = api.workflow(&project, "ci", FIRST).await;
    let id = text(&workflow["id"]);
    let (status, refused) = api
        .put(
            &format!("/api/v1/workflows/{id}/source"),
            &json!({ "source": fixture("workflows/unknown-key.yml") }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["diagnostics"][0]["code"], "unknown_key");
    assert_eq!(count(&database, "workflow_revision").await, 1);
    assert_eq!(
        api.get(&format!("/api/v1/workflows/{id}")).await,
        (200, workflow.clone())
    );
    let (status, revisions) = api.get(&format!("/api/v1/workflows/{id}/revisions")).await;
    assert_eq!(status, 200, "{revisions}");
    assert_eq!(
        ids(&revisions),
        vec![text(&workflow["revision_id"]).to_owned()]
    );

    let (status, taken) = api
        .post(&workflows, &json!({ "name": "ci", "source": SECOND }))
        .await;
    assert_eq!(status, 409, "{taken}");
    assert_eq!(
        taken["error"]["message"],
        "the project already has a workflow named `ci`"
    );
    assert_eq!(count(&database, "workflow_revision").await, 1);
}

#[tokio::test]
async fn saving_the_same_source_twice_yields_one_revision() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let workflow = api.workflow(&project, "ci", FIRST).await;
    let id = text(&workflow["id"]);
    let source = format!("/api/v1/workflows/{id}/source");
    let first = text(&workflow["revision_id"]).to_owned();

    let (status, same) = api.put(&source, &json!({ "source": FIRST })).await;
    assert_eq!(status, 200, "{same}");
    assert_eq!(same["id"], first.as_str());
    assert_eq!(same["source"], FIRST);
    assert_eq!(same["workflow_id"], workflow["id"]);
    let hashed = strings(
        database.connection(),
        &format!(
            "SELECT encode(sha256(convert_to(source, 'UTF8')), 'hex') AS value \
             FROM bosun.workflow_revision WHERE id = '{first}'"
        ),
    )
    .await;
    assert_eq!(hashed, vec![text(&same["sha256"]).to_owned()]);

    let (status, second) = api.put(&source, &json!({ "source": SECOND })).await;
    assert_eq!(status, 201, "{second}");
    assert_ne!(second["id"], first.as_str());
    let (status, again) = api.put(&source, &json!({ "source": SECOND })).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again, second);
    let (_, current) = api.get(&format!("/api/v1/workflows/{id}")).await;
    assert_eq!(current["revision_id"], second["id"]);

    let (status, back) = api.put(&source, &json!({ "source": FIRST })).await;
    assert_eq!(status, 200, "{back}");
    assert_eq!(back["id"], first.as_str());
    let (_, current) = api.get(&format!("/api/v1/workflows/{id}")).await;
    assert_eq!(current["revision_id"], first.as_str());

    let (status, revisions) = api.get(&format!("/api/v1/workflows/{id}/revisions")).await;
    assert_eq!(status, 200, "{revisions}");
    assert_eq!(
        ids(&revisions),
        vec![text(&second["id"]).to_owned(), first.clone()]
    );
    assert_eq!(count(&database, "workflow_revision").await, 2);
}

#[tokio::test]
async fn workflows_of_a_project_and_revisions_of_a_workflow_are_listed_by_page() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let mut created = Vec::new();
    for name in ["build", "deploy", "nightly"] {
        created.push(text(&api.workflow(&project, name, FIRST).await["id"]).to_owned());
    }
    let by_id = format!("/api/v1/projects/{}/workflows", text(&project["id"]));

    let (status, all) = api.get("/api/v1/projects/globalart/ops/workflows").await;
    assert_eq!(status, 200, "{all}");
    assert_eq!(ids(&all), created);
    assert_eq!(all["next"], Value::Null);
    let (status, first) = api.get(&format!("{by_id}?limit=2")).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(ids(&first), created[..2].to_vec());
    let cursor = text(&first["next"]);
    let (status, rest) = api.get(&format!("{by_id}?limit=2&cursor={cursor}")).await;
    assert_eq!(status, 200, "{rest}");
    assert_eq!(ids(&rest), created[2..].to_vec());
    assert_eq!(rest["next"], Value::Null);
    let (status, named) = api.get(&format!("{by_id}?name=deploy")).await;
    assert_eq!(status, 200, "{named}");
    assert_eq!(ids(&named), vec![created[1].clone()]);

    for (query, message) in [
        ("limit=0", "`limit` is `0`, and a page holds 1 to 500 items"),
        (
            "limit=501",
            "`limit` is `501`, and a page holds 1 to 500 items",
        ),
        (
            "cursor=yesterday",
            "`cursor` is `yesterday`, which is not the `next` of a page of this list",
        ),
        (
            "status=queued",
            "this list has no query parameter `status`; it takes `limit`, `cursor`, `name`",
        ),
    ] {
        let (status, refused) = api.get(&format!("{by_id}?{query}")).await;
        assert_eq!(status, 422, "{query}: {refused}");
        assert_eq!(refused["error"]["code"], "invalid_input");
        assert_eq!(refused["error"]["message"], message, "{query}");
    }

    let workflow = &created[0];
    let mut saved = vec![];
    for run in ["one", "two"] {
        let (status, revision) = api
            .put(
                &format!("/api/v1/workflows/{workflow}/source"),
                &json!({ "source": FIRST.replace("cargo build", &format!("echo {run}")) }),
            )
            .await;
        assert_eq!(status, 201, "{revision}");
        saved.push(text(&revision["id"]).to_owned());
    }
    let revisions = format!("/api/v1/workflows/{workflow}/revisions");
    let (status, newest) = api.get(&format!("{revisions}?limit=2")).await;
    assert_eq!(status, 200, "{newest}");
    assert_eq!(ids(&newest), vec![saved[1].clone(), saved[0].clone()]);
    let (status, oldest) = api
        .get(&format!(
            "{revisions}?limit=2&cursor={}",
            text(&newest["next"])
        ))
        .await;
    assert_eq!(status, 200, "{oldest}");
    assert_eq!(oldest["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(oldest["items"][0]["source"], FIRST);
    let (status, refused) = api.get(&format!("{revisions}?name=ci")).await;
    assert_eq!(status, 422, "{refused}");

    for path in [
        "/api/v1/workflows/0192f3a4-5b6c-7d8e-9f01-000000000005".to_owned(),
        "/api/v1/workflows/ci/revisions".to_owned(),
    ] {
        let (status, body) = api.get(&path).await;
        assert_eq!(status, 404, "{path}: {body}");
        assert_eq!(body["error"]["code"], "not_found");
    }
}

#[tokio::test]
async fn a_dispatch_creates_a_queued_run_with_its_first_attempt_its_jobs_and_the_ir_snapshot() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let workflow = api
        .workflow(&project, "ci", &fixture("workflows/two-jobs.yml"))
        .await;
    let dispatch = format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"]));
    let (_, me) = api.get("/api/v1/me").await;

    let (status, run) = api
        .post(&dispatch, &json!({ "ref": null, "inputs": {} }))
        .await;
    assert_eq!(status, 201, "{run}");
    let token = strings(
        database.connection(),
        "SELECT id::text AS value FROM bosun.api_token",
    )
    .await;
    assert_eq!(
        run["actor"],
        json!({
            "kind": "token",
            "id": token[0],
            "user_id": me["user"]["id"],
            "login": "roman",
        })
    );
    assert_eq!(run["project_id"], project["id"]);
    assert_eq!(run["workflow_id"], workflow["id"]);
    assert_eq!(run["revision_id"], workflow["revision_id"]);
    assert_eq!(run["number"], 1);
    assert!(run["global_number"].is_u64(), "{run}");
    assert_eq!(run["event"], "workflow_dispatch");
    assert_eq!(run["status"], "queued");
    assert_eq!(run["conclusion"], Value::Null);
    assert_eq!(run["attempt"], 1);
    assert_eq!(run["started_at"], Value::Null);
    assert_eq!(run["completed_at"], Value::Null);
    let run_id = text(&run["id"]);

    let snapshot = strings(
        database.connection(),
        &format!("SELECT ir::text AS value FROM bosun.run WHERE id = '{run_id}'"),
    )
    .await;
    let snapshot: Value = serde_json::from_str(&snapshot[0]).expect("the IR as JSON");
    let expected: Value = serde_json::from_str(&fixture("ir/two-jobs.json")).expect("the IR");
    assert_eq!(snapshot, expected);
    assert_eq!(
        strings(
            database.connection(),
            &format!(
                "SELECT concat_ws(' ', number, actor->>'kind') AS value FROM bosun.attempt \
                 WHERE run_id = '{run_id}'"
            ),
        )
        .await,
        vec!["1 token"]
    );
    assert_eq!(
        strings(
            database.connection(),
            &format!(
                "SELECT concat_ws(' ', job.key, job.name, job.status, array_to_string(job.needs, ','), \
                 array_to_string(job.labels, ','), array_length(job.pool_ids, 1)) \
                 AS value FROM bosun.job JOIN bosun.attempt ON attempt.id = job.attempt_id \
                 WHERE attempt.run_id = '{run_id}' ORDER BY job.id"
            ),
        )
        .await,
        vec![
            "build Build queued  self-hosted,linux,x64 1",
            "test test pending build "
        ]
    );
    assert_eq!(
        strings(
            database.connection(),
            &format!(
                "SELECT kind AS value FROM bosun.run_event WHERE run_id = '{run_id}' ORDER BY seq"
            ),
        )
        .await,
        vec![
            "run.created",
            "attempt.created",
            "job.created",
            "job.created",
            "job.status"
        ]
    );

    let (status, second) = api
        .post(&dispatch, &json!({ "ref": null, "inputs": {} }))
        .await;
    assert_eq!(status, 201, "{second}");
    assert_eq!(second["number"], 2);
    assert_ne!(second["id"], run["id"]);
    assert!(second["global_number"].as_u64() > run["global_number"].as_u64());
}

#[tokio::test]
async fn dispatch_with_an_idempotency_key_repeated_returns_the_same_run() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let workflow = api.workflow(&project, "ci", FIRST).await;
    let dispatch = format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"]));
    let body = json!({ "ref": null, "inputs": {} });
    let keyed = |key: &str, body: &Value| {
        api.request(Method::POST, &dispatch)
            .header("idempotency-key", key)
            .json(body)
    };

    let (status, first) = send(keyed("dispatch-1", &body)).await;
    assert_eq!(status, 201, "{first}");
    let (status, again) = send(keyed("dispatch-1", &body)).await;
    assert_eq!(status, 201, "{again}");
    assert_eq!(again, first);
    assert_eq!(count(&database, "run").await, 1);

    let (status, refused) = send(keyed(
        "dispatch-1",
        &json!({ "ref": null, "inputs": {}, "note": 1 }),
    ))
    .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"]["code"], "conflict");
    assert_eq!(
        refused["error"]["message"],
        "the Idempotency-Key `dispatch-1` came with a different request within the last 24 hours"
    );
    let (status, other) = send(keyed("dispatch-2", &body)).await;
    assert_eq!(status, 201, "{other}");
    assert_eq!(other["number"], 2);
    let (status, unkeyed) = api.post(&dispatch, &body).await;
    assert_eq!(status, 201, "{unkeyed}");
    assert_eq!(unkeyed["number"], 3);

    let mut racing = Vec::new();
    for _ in 0..8 {
        racing.push(tokio::spawn(send(keyed("dispatch-3", &body))));
    }
    let mut runs = Vec::new();
    for race in racing {
        let (status, run) = race.await.expect("a racing request");
        assert_eq!(status, 201, "{run}");
        runs.push(run);
    }
    assert!(runs.iter().all(|run| run == &runs[0]), "{runs:#?}");
    assert_eq!(runs[0]["number"], 4);
    assert_eq!(count(&database, "run").await, 4);
    assert_eq!(count(&database, "idempotency_key").await, 3);

    for key in ["", &"k".repeat(256), "with space"] {
        let (status, refused) = send(keyed(key, &body)).await;
        assert_eq!(status, 422, "{key:?}: {refused}");
        assert_eq!(
            refused["error"]["message"],
            "the Idempotency-Key header must be 1 to 255 visible ASCII characters"
        );
    }
    assert_eq!(count(&database, "run").await, 4);
}

#[tokio::test]
async fn a_create_with_an_idempotency_key_repeated_returns_the_first_response() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let create = |key: &str, path: &str, body: Value| {
        send(
            api.request(Method::POST, path)
                .header("idempotency-key", key)
                .json(&body),
        )
    };
    let organisation = json!({ "slug": "globalart", "name": "GlobalArt" });
    let (status, first) = create("org", "/api/v1/organisations", organisation.clone()).await;
    assert_eq!(status, 201, "{first}");
    let (status, again) = create("org", "/api/v1/organisations", organisation.clone()).await;
    assert_eq!((status, again), (201, first.clone()));
    let (status, taken) = api.post("/api/v1/organisations", &organisation).await;
    assert_eq!(status, 409, "{taken}");
    let (status, refused) = create(
        "org",
        "/api/v1/projects",
        json!({ "organisation": "globalart", "slug": "ops", "name": "Ops" }),
    )
    .await;
    assert_eq!(status, 409, "{refused}");

    let project = json!({ "organisation": "globalart", "slug": "ops", "name": "Ops" });
    let (status, first) = create("project", "/api/v1/projects", project.clone()).await;
    assert_eq!(status, 201, "{first}");
    assert_eq!(
        create("project", "/api/v1/projects", project).await,
        (201, first.clone())
    );

    let workflows = format!("/api/v1/projects/{}/workflows", text(&first["id"]));
    let workflow = json!({ "name": "ci", "source": FIRST });
    let (status, created) = create("workflow", &workflows, workflow.clone()).await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(
        create("workflow", &workflows, workflow).await,
        (201, created)
    );
    assert_eq!(count(&database, "workflow").await, 1);
}

#[tokio::test]
async fn a_dispatch_that_cannot_start_a_run_is_refused_and_starts_none() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let workflow = api.workflow(&project, "ci", FIRST).await;
    let id = text(&workflow["id"]);
    let dispatch = format!("/api/v1/workflows/{id}/dispatch");

    let (status, refused) = api
        .post(
            &dispatch,
            &json!({ "ref": "main", "inputs": { "env": "prod", "canary": 10 } }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["message"], "the dispatch is not valid");
    let fields: Vec<&str> = refused["error"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| text(&field["field"]))
        .collect();
    assert_eq!(fields, vec!["ref", "inputs.canary", "inputs.env"]);
    let (status, refused) = api.post(&dispatch, &json!({})).await;
    assert_eq!(status, 422, "{refused}");

    for path in [
        "/api/v1/workflows/0192f3a4-5b6c-7d8e-9f01-000000000005/dispatch",
        "/api/v1/workflows/ci/dispatch",
    ] {
        let (status, body) = api.post(path, &json!({ "ref": null, "inputs": {} })).await;
        assert_eq!(status, 404, "{path}: {body}");
        assert_eq!(body["error"]["code"], "not_found");
    }

    database
        .connection()
        .execute_unprepared(&format!(
            "UPDATE bosun.workflow_revision SET source = 'jobs: [' WHERE workflow_id = '{id}'"
        ))
        .await
        .expect("a revision the parser refuses");
    let (status, refused) = api
        .post(&dispatch, &json!({ "ref": null, "inputs": {} }))
        .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"]["code"], "invalid_state");
    assert_eq!(refused["error"]["diagnostics"][0]["code"], "yaml_syntax");

    let (status, disabled) = api
        .patch(
            &format!("/api/v1/workflows/{id}"),
            &json!({ "enabled": false }),
        )
        .await;
    assert_eq!(status, 200, "{disabled}");
    let (status, refused) = api
        .post(&dispatch, &json!({ "ref": null, "inputs": {} }))
        .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"]["code"], "invalid_state");
    assert_eq!(
        refused["error"]["message"],
        "workflow `ci` is disabled, so it cannot be dispatched"
    );
    assert_eq!(count(&database, "run").await, 0);
    assert_eq!(
        strings(
            database.connection(),
            &format!("SELECT last_run_number::text AS value FROM bosun.workflow WHERE id = '{id}'"),
        )
        .await,
        vec!["0"]
    );
}
