use axum::http::{Method, StatusCode};
use bosun_db::entity::run;
use bosun_db::roles::Subject;
use bosun_db::workflows::NewWorkflow;
use sea_orm::EntityTrait;
use serde_json::{Value, json};

use super::access::{Site, forbidden};

const DEPLOY: &str = concat!(
    "name: deploy\n",
    "on:\n",
    "  repository_dispatch:\n",
    "    types: [deploy]\n",
    "jobs:\n",
    "  deploy:\n",
    "    runs-on: [self-hosted]\n",
    "    steps:\n",
    "      - run: echo ${{ github.event.client_payload.environment }}\n",
);

const BACKUP: &str = concat!(
    "name: backup\n",
    "on: workflow_dispatch\n",
    "jobs:\n",
    "  backup:\n",
    "    runs-on: [self-hosted]\n",
    "    steps:\n",
    "      - run: echo backup\n",
);

#[tokio::test]
async fn an_event_runs_the_server_side_workflows_of_a_project_without_a_repository_and_a_viewer_sends_none()
 {
    let site = Site::new().await;
    let project = site.project().await;
    for (name, source, byte) in [("deploy", DEPLOY, 8), ("backup", BACKUP, 9)] {
        bosun_db::workflows::create(
            site.db(),
            NewWorkflow {
                project_id: project.id,
                name,
                source,
                sha256: vec![byte; 32],
            },
        )
        .await
        .expect("a workflow");
    }
    let (viewer, viewing) = site.user("vera").await;
    site.bind(
        "viewer",
        Subject::User(viewer),
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
    let event = json!({ "event_type": "deploy", "client_payload": { "environment": "prod1" } });
    let path = format!("/api/v1/projects/globalart/{}/events", project.slug);

    let refused = site
        .send(Method::POST, &path, &viewing, Some(event.clone()))
        .await;
    forbidden(&refused, "a viewer's event");
    assert_eq!(
        refused.body["error"]["message"],
        format!(
            "`vera` does not hold `run.dispatch` on project `{}`",
            project.slug
        )
    );

    let planned = site
        .send(Method::POST, &path, &developing, Some(event))
        .await;
    assert_eq!(planned.status, StatusCode::CREATED, "{planned:?}");
    let runs = planned.body["runs"].as_array().expect("the runs");
    assert_eq!(runs.len(), 1, "{planned:?}");
    assert_eq!(runs[0]["display_name"], "deploy");
    assert_eq!(runs[0]["event"], "repository_dispatch");
    assert_eq!(runs[0]["status"], "queued");
    let id: sea_orm::prelude::Uuid = runs[0]["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("the id of the run");
    let stored = run::Entity::find_by_id(id)
        .one(site.db())
        .await
        .expect("a read")
        .expect("the run");
    assert_eq!(
        stored.trigger["payload"],
        json!({
            "action": "deploy",
            "branch": null,
            "client_payload": { "environment": "prod1" }
        })
    );
    assert_eq!((stored.git_ref, stored.sha), (None, None));

    let unmatched = site
        .send(
            Method::POST,
            &path,
            &developing,
            Some(json!({ "event_type": "backup" })),
        )
        .await;
    assert_eq!(unmatched.status, StatusCode::CREATED, "{unmatched:?}");
    assert_eq!(unmatched.body["runs"], Value::Array(Vec::new()));
}
