use std::collections::{BTreeMap, HashMap};

use axum::http::{Method, StatusCode};
use bosun_db::roles::Subject;
use bosun_db::runs::NewRun;
use bosun_types::actor::Actor;
use bosun_types::api::{Project, Workflow};
use bosun_types::id::RunId;
use bosun_types::status::{RunConclusion, RunStatus};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::access::{Answer, Site, document, forbidden, owner_credentials};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cases {
    runs: Vec<Planted>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Planted {
    name: String,
    project: String,
    workflow: String,
    event: String,
    r#ref: Option<String>,
    status: RunStatus,
    conclusion: Option<RunConclusion>,
    actor: Actor,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    description: String,
    query: BTreeMap<String, String>,
    runs: Vec<String>,
}

async fn project_at(site: &Site, path: &str) -> Project {
    let (organisation, slug) = path.split_once('/').expect("a path of a project");
    let found = bosun_db::projects::find_by_slugs(site.db(), organisation, slug)
        .await
        .expect("a lookup");
    match found {
        Some(project) => project,
        None => site.second_project(slug).await,
    }
}

async fn workflow_named(site: &Site, project: &Project, name: &str) -> Workflow {
    let found = bosun_db::workflows::list(site.db(), project.id, Some(name), None, 1)
        .await
        .expect("a lookup")
        .pop();
    match found {
        Some(workflow) => workflow,
        None => site.workflow(project.id, name).await,
    }
}

async fn plant(site: &Site, workflow: &Workflow, planted: &Planted) -> RunId {
    let transaction = site.db().begin().await.expect("a transaction");
    let run = bosun_db::runs::dispatch(
        &transaction,
        NewRun {
            workflow,
            revision_id: workflow.revision_id.expect("a current revision"),
            ir: &super::workflow(Vec::new()),
            event: &planted.event,
            git_ref: planted.r#ref.as_deref(),
            sha: None,
            payload: &json!({}),
            actor: &planted.actor,
        },
    )
    .await
    .expect("a run");
    let mut locked = bosun_db::runs::lock(&transaction, run.id)
        .await
        .expect("a lock")
        .expect("the run");
    if matches!(planted.status, RunStatus::InProgress | RunStatus::Completed) {
        locked
            .set_status(RunStatus::InProgress, None)
            .await
            .expect("a started run");
    }
    if planted.status == RunStatus::Completed {
        locked
            .set_status(RunStatus::Completed, planted.conclusion)
            .await
            .expect("a completed run");
    }
    transaction.commit().await.expect("a commit");
    run.id
}

fn listed(answer: &Answer, names: &HashMap<String, String>) -> Vec<String> {
    assert_eq!(answer.status, StatusCode::OK, "{answer:?}");
    answer.body["items"]
        .as_array()
        .expect("a page of runs")
        .iter()
        .map(|run| {
            let id = run["id"].as_str().expect("the id of a run");
            names.get(id).cloned().unwrap_or_else(|| id.to_owned())
        })
        .collect()
}

#[tokio::test]
async fn the_run_list_filters_by_each_field_alone_and_together_as_the_cases_say() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let cases: Cases = serde_json::from_str(&document("contracts/fixtures/runs/filters.json"))
        .expect("the cases of the run filters");
    let mut workflows: HashMap<String, Workflow> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    for planted in &cases.runs {
        let project = project_at(&site, &planted.project).await;
        let workflow = workflow_named(&site, &project, &planted.workflow).await;
        let run = plant(&site, &workflow, planted).await;
        names.insert(run.to_string(), planted.name.clone());
        workflows.insert(planted.workflow.clone(), workflow);
    }
    for case in &cases.cases {
        let mut query = vec!["limit=500".to_owned()];
        for (name, value) in &case.query {
            let value = match name.as_str() {
                "workflow" => workflows[value].id.to_string(),
                _ => value.clone(),
            };
            query.push(format!("{name}={value}"));
        }
        let answer = site
            .get(&format!("/api/v1/runs?{}", query.join("&")), &owner)
            .await;
        assert_eq!(listed(&answer, &names), case.runs, "{}", case.description);
    }

    let first = site
        .get("/api/v1/runs?status=failure&limit=2", &owner)
        .await;
    assert_eq!(
        listed(&first, &names),
        ["nightly-push-main-failure", "ci-issues-failure"]
    );
    let next = first.body["next"].as_str().expect("a next page");
    let second = site
        .get(
            &format!("/api/v1/runs?status=failure&limit=2&cursor={next}"),
            &owner,
        )
        .await;
    assert_eq!(listed(&second, &names), ["ci-push-main-failure"]);
    assert_eq!(second.body["next"], Value::Null);

    let shown = site.get("/api/v1/runs?branch=feature/guide", &owner).await;
    assert_eq!(
        (
            &shown.body["items"][0]["ref"],
            &shown.body["items"][0]["sha"]
        ),
        (&json!("refs/heads/feature/guide"), &Value::Null)
    );
}

#[tokio::test]
async fn a_filter_naming_what_the_caller_cannot_see_or_no_state_of_a_run_is_invalid_input() {
    let site = Site::new().await;
    let keel = site.project().await;
    let hidden = site.second_project("hidden").await;
    let secret = site.workflow(hidden.id, "secret").await;
    let (viewer, viewing) = site.user("vera").await;
    site.bind(
        "viewer",
        Subject::User(viewer),
        keel.organisation_id,
        Some(keel.id),
    )
    .await;
    for (query, field, message) in [
        (
            "status=sometimes".to_owned(),
            "status",
            "`sometimes` is neither a status nor a conclusion of a run; they are `queued`, \
             `in_progress`, `waiting`, `paused`, `completed`, `success`, `failure`, `cancelled`, \
             `skipped`, `timed_out`, `startup_failure`"
                .to_owned(),
        ),
        (
            "project=globalart/hidden".to_owned(),
            "project",
            "there is no project `globalart/hidden`".to_owned(),
        ),
        (
            format!("project={}", hidden.id),
            "project",
            format!("there is no project `{}`", hidden.id),
        ),
        (
            format!("workflow={}", secret.id),
            "workflow",
            format!("there is no workflow `{}`", secret.id),
        ),
        (
            "workflow=ci".to_owned(),
            "workflow",
            "there is no workflow `ci`".to_owned(),
        ),
    ] {
        let answer = site.get(&format!("/api/v1/runs?{query}"), &viewing).await;
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(answer.body["error"]["code"], "invalid_input", "{query}");
        assert_eq!(
            answer.body["error"]["message"], "the query is not valid",
            "{query}"
        );
        assert_eq!(
            answer.body["error"]["fields"],
            json!([{ "field": field, "message": message }]),
            "{query}"
        );
    }
    let unknown = site.get("/api/v1/runs?branches=main", &viewing).await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        unknown.body["error"]["message"],
        "this list has no query parameter `branches`; it takes `limit`, `cursor`, `project`, \
         `workflow`, `branch`, `event`, `status`, `actor`"
    );
    let seen = site
        .get(&format!("/api/v1/runs?project={}", keel.path), &viewing)
        .await;
    assert_eq!(seen.status, StatusCode::OK, "{seen:?}");
}

#[tokio::test]
async fn a_maintainer_disables_a_workflow_whose_dispatch_is_refused_and_a_developer_changes_nothing()
 {
    let site = Site::new().await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let (maintainer, maintaining) = site.user("maya").await;
    site.bind(
        "maintainer",
        Subject::User(maintainer),
        project.organisation_id,
        None,
    )
    .await;
    let (developer, developing) = site.user("dana").await;
    site.bind(
        "developer",
        Subject::User(developer),
        project.organisation_id,
        None,
    )
    .await;
    let path = format!("/api/v1/workflows/{}", deploy.id);

    let refused = site
        .send(
            Method::PATCH,
            &path,
            &developing,
            Some(json!({ "enabled": false })),
        )
        .await;
    forbidden(&refused, "a developer's change");
    assert_eq!(
        refused.body["error"]["message"],
        format!(
            "`dana` does not hold `workflow.manage` on project `{}`",
            project.slug
        )
    );

    let disabled = site
        .send(
            Method::PATCH,
            &path,
            &maintaining,
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "{disabled:?}");
    assert_eq!(disabled.body["enabled"], false);
    assert_eq!(disabled.body["id"], json!(deploy.id));
    let shown = site.get(&path, &developing).await;
    assert_eq!(shown.body, disabled.body);

    let dispatch = site.dispatch(&deploy, &developing).await;
    assert_eq!(dispatch.status, StatusCode::CONFLICT, "{dispatch:?}");
    assert_eq!(dispatch.body["error"]["code"], "invalid_state");
    assert_eq!(
        dispatch.body["error"]["message"],
        "workflow `deploy` is disabled, so it cannot be dispatched"
    );

    for (body, enabled) in [(json!({}), false), (json!({ "enabled": false }), false)] {
        let unchanged = site
            .send(Method::PATCH, &path, &maintaining, Some(body.clone()))
            .await;
        assert_eq!(unchanged.status, StatusCode::OK, "{body}: {unchanged:?}");
        assert_eq!(unchanged.body["enabled"], enabled, "{body}");
    }
    for body in [json!({ "enabled": null }), json!({ "enable": true })] {
        let invalid = site
            .send(Method::PATCH, &path, &maintaining, Some(body.clone()))
            .await;
        assert_eq!(
            invalid.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {invalid:?}"
        );
        assert_eq!(invalid.body["error"]["code"], "invalid_input", "{body}");
    }

    let enabled = site
        .send(
            Method::PATCH,
            &path,
            &maintaining,
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{enabled:?}");
    assert_eq!(enabled.body["enabled"], true);
    let dispatched = site.dispatch(&deploy, &developing).await;
    assert_eq!(dispatched.status, StatusCode::CREATED, "{dispatched:?}");
}

#[tokio::test]
async fn a_workflow_of_a_project_without_a_repository_carries_its_newest_run() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let path = format!("/api/v1/workflows/{}", deploy.id);
    assert_eq!(
        site.get(&path, &owner).await.body["latest_run"],
        Value::Null
    );

    let first = site.dispatch(&deploy, &owner).await;
    assert_eq!(first.status, StatusCode::CREATED, "{first:?}");
    let second = site.dispatch(&deploy, &owner).await;
    assert_eq!(second.status, StatusCode::CREATED, "{second:?}");
    let shown = site.get(&path, &owner).await;
    let run = site
        .get(
            &format!(
                "/api/v1/runs/{}",
                second.body["id"].as_str().expect("an id")
            ),
            &owner,
        )
        .await;
    assert_eq!(shown.body["latest_run"], run.body);

    let listed = site
        .get(
            &format!("/api/v1/projects/{}/workflows?name=deploy", project.path),
            &owner,
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{listed:?}");
    assert_eq!(listed.body["items"], json!([shown.body]));
}
