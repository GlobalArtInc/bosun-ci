use std::collections::BTreeSet;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use bosun_db::entity::{run, triggered_run, upstream_subscription};
use bosun_db::outbox::{Claimed, Kind};
use bosun_db::roles::Subject;
use bosun_db::runs::{NewRun, Trust};
use bosun_db::tokens::NewToken;
use bosun_types::actor::Actor;
use bosun_types::api::{Project, Workflow};
use bosun_types::command::{Command, CommandRequest, Target};
use bosun_types::event::{Event, SkipReason};
use bosun_types::id::{ProjectId, RunId, UserId};
use bosun_types::permission::Permission;
use bosun_types::status::Conclusion;
use chrono::{TimeDelta, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    Statement, TransactionTrait,
};
use serde_json::{Map, json};

use super::access::{Credentials, Site, owner_credentials};
use super::schedules::{Pushes, ROMAN};
use super::{finished, site as public};
use crate::clock::Clock;
use crate::commands::Applied;
use crate::metrics::Meters;
use crate::planner;
use crate::triggers::{self, Triggers};

const CI: &str = "name: CI\non: push\njobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      - run: make\n";

fn following(trigger: &str) -> String {
    format!(
        "name: Follow\non:\n{trigger}jobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      \
         - run: echo\n"
    )
}

const AFTER_CI: &str =
    "  workflow_run:\n    workflows: [CI]\n    types: [completed]\n    branches: [main]\n";

fn named(name: &str, trigger: &str) -> String {
    CI.replace(
        "name: CI\non: push\n",
        &format!("name: {name}\non:\n{trigger}"),
    )
}

struct Scene {
    site: Site,
    owner: Credentials,
    first: Triggers,
    second: Triggers,
}

impl Scene {
    async fn new() -> Scene {
        let site = Site::new().await;
        let owner = owner_credentials(&site).await;
        let other = bosun_db::connect(&site.world.database.url(), 4)
            .await
            .expect("the pool of a second replica");
        let replica = |db: DatabaseConnection| Triggers {
            db,
            site: public(),
            forge: None,
            clock: Clock::system(),
            meters: Arc::new(Meters::default()),
        };
        let first = replica(site.db().clone());
        Scene {
            site,
            owner,
            first,
            second: replica(other),
        }
    }

    async fn create(&self, project: &Project, name: &str, source: &str) -> Workflow {
        let created = self
            .site
            .send(
                Method::POST,
                &format!("/api/v1/projects/{}/workflows", project.id),
                &self.owner,
                Some(json!({ "name": name, "source": source })),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{created:?}");
        bosun_db::workflows::find(
            self.site.db(),
            created.body["id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .expect("a workflow id"),
        )
        .await
        .expect("a workflow")
        .expect("the workflow")
    }

    async fn named(&self, project: &Project, name: &str) -> Workflow {
        let created = bosun_db::workflows::server_side(self.site.db(), project.id)
            .await
            .expect("the workflows");
        created
            .into_iter()
            .find(|workflow| workflow.name == name)
            .expect("the workflow")
    }

    async fn start(
        &self,
        workflow: &Workflow,
        actor: &Actor,
        git_ref: Option<&str>,
        conclusion: Conclusion,
    ) -> RunId {
        let run = self
            .dispatch(workflow, actor, git_ref, Trust::Trusted)
            .await;
        self.site
            .world
            .finish(run, "build", finished(conclusion))
            .await;
        run
    }

    async fn dispatch(
        &self,
        workflow: &Workflow,
        actor: &Actor,
        git_ref: Option<&str>,
        trust: Trust,
    ) -> RunId {
        let revision = bosun_db::workflows::revision(
            self.site.db(),
            workflow.revision_id.expect("a revision"),
        )
        .await
        .expect("a revision")
        .expect("the revision");
        let parsed = bosun_workflow::parse(&workflow.name, &revision.source).expect("a workflow");
        let transaction = self.site.db().begin().await.expect("a transaction");
        let run = planner::dispatch(
            &transaction,
            NewRun {
                workflow,
                revision_id: revision.id,
                ir: &parsed.workflow,
                event: "push",
                git_ref,
                sha: Some("6113728f27ae82c7b1a177c8d03f9e96e0adf246"),
                payload: &json!({}),
                inputs: &Map::new(),
                actor,
                trust,
            },
            &[],
            &public(),
        )
        .await
        .expect("a run");
        transaction.commit().await.expect("a commit");
        run.id
    }

    async fn drain(&self) {
        for _ in 0..200 {
            if !triggers::next(&self.first).await.expect("a round") {
                return;
            }
        }
        panic!("the outbox does not drain");
    }

    async fn runs(&self, workflow: &Workflow) -> Vec<run::Model> {
        run::Entity::find()
            .filter(run::Column::WorkflowId.eq(workflow.id.as_uuid()))
            .order_by_asc(run::Column::Number)
            .all(self.site.db())
            .await
            .expect("the runs")
    }

    async fn skips(&self, run: RunId) -> Vec<(ProjectId, SkipReason)> {
        bosun_db::journal::read(self.site.db(), run, 0)
            .await
            .expect("the journal")
            .into_iter()
            .filter_map(|event| match event.event {
                Event::TriggerSkipped(skipped) => Some((skipped.project_id, skipped.reason)),
                _ => None,
            })
            .collect()
    }

    async fn follow(&self, upstream: &Workflow, follower: &Workflow) {
        bosun_db::triggers::follow(self.site.db(), upstream.project_id, follower.project_id)
            .await
            .expect("a follower");
    }

    async fn partner(&self) -> Project {
        let organisation = bosun_db::organisations::create(self.site.db(), "partner", "Partner")
            .await
            .expect("an organisation");
        bosun_db::projects::create(self.site.db(), organisation.id, "app", "App")
            .await
            .expect("a project")
    }
}

fn forge(login: &str) -> Actor {
    Actor::Forge {
        id: 9001,
        login: login.to_owned(),
    }
}

#[tokio::test]
async fn the_completion_of_a_run_on_a_listed_branch_creates_one_run_with_its_conclusion() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let ci = scene.create(&project, "main-ci", CI).await;
    let follow = scene.create(&project, "follow", &following(AFTER_CI)).await;

    let main = scene
        .start(
            &ci,
            &forge("octocat"),
            Some("refs/heads/main"),
            Conclusion::Failure,
        )
        .await;
    scene.drain().await;
    let runs = scene.runs(&follow).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let triggered = &runs[0];
    assert_eq!(triggered.event, "workflow_run");
    let payload = &triggered.trigger["payload"];
    assert_eq!(payload["action"], "completed");
    assert_eq!(payload["workflow_run"]["conclusion"], "failure");
    assert_eq!(payload["workflow_run"]["name"], "CI");
    assert_eq!(payload["workflow_run"]["head_branch"], "main");
    assert_eq!(triggered.trigger["actor"]["login"], "octocat");
    let source = bosun_db::views::run(scene.site.db(), main)
        .await
        .expect("a run")
        .expect("the run");
    assert_eq!(payload["workflow_run"]["id"], source.global_number);

    scene
        .start(
            &ci,
            &forge("octocat"),
            Some("refs/heads/dev"),
            Conclusion::Success,
        )
        .await;
    scene.drain().await;
    assert_eq!(
        scene.runs(&follow).await.len(),
        1,
        "a run on another branch"
    );
}

#[tokio::test]
async fn two_workers_on_one_row_and_a_worker_that_dies_after_its_commit_create_one_run() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let ci = scene.create(&project, "main-ci", CI).await;
    let follow = scene.create(&project, "follow", &following(AFTER_CI)).await;
    scene
        .start(
            &ci,
            &forge("octocat"),
            Some("refs/heads/main"),
            Conclusion::Success,
        )
        .await;
    let row = scene
        .site
        .db()
        .query_one(Statement::from_string(
            scene.site.db().get_database_backend(),
            "SELECT id, collapse_key, payload FROM bosun.outbox \
             WHERE kind = 'run_trigger' AND collapse_key LIKE '%/completed'",
        ))
        .await
        .expect("the outbox")
        .expect("a row for the completion");
    let claimed = Claimed {
        id: u64::try_from(row.try_get::<i64>("", "id").expect("an id")).expect("an id"),
        kind: Kind::RunTrigger,
        collapse_key: row.try_get("", "collapse_key").expect("a key"),
        payload: row.try_get("", "payload").expect("a payload"),
        attempts: 0,
        failing_since: None,
        lease: Utc::now() + TimeDelta::minutes(5),
        dropped: 0,
    };

    let (left, right) = tokio::join!(
        triggers::process(&scene.first, &claimed),
        triggers::process(&scene.second, &claimed)
    );
    assert!(left.is_ok() && right.is_ok(), "both workers finish");
    assert_eq!(scene.runs(&follow).await.len(), 1, "two replicas, one row");

    scene.drain().await;
    assert_eq!(
        scene.runs(&follow).await.len(),
        1,
        "the row survived the worker that committed and was processed again"
    );
    let decided = triggered_run::Entity::find()
        .all(scene.site.db())
        .await
        .expect("the decisions");
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0].transition, "completed");
    assert_eq!(decided[0].depth, 1);
}

async fn upstream_scene() -> (Scene, Workflow, Workflow, UserId, UserId) {
    let scene = Scene::new().await;
    let keel = scene.site.project().await;
    let release = scene
        .create(
            &keel,
            "release",
            &named("release", "  workflow_dispatch:\n"),
        )
        .await;
    let app = scene.partner().await;
    let deploy = scene
        .create(
            &app,
            "deploy",
            &following("  upstream:\n    project: globalart/keel\n    workflows: [release]\n"),
        )
        .await;
    let (alice, _) = scene.site.user("alice").await;
    scene
        .site
        .bind("developer", Subject::User(alice), app.organisation_id, None)
        .await;
    let (bob, _) = scene.site.user("bob").await;
    scene
        .site
        .bind(
            "developer",
            Subject::User(bob),
            keel.organisation_id,
            Some(keel.id),
        )
        .await;
    (scene, release, deploy, alice, bob)
}

fn user(id: UserId, login: &str) -> Actor {
    Actor::User {
        id,
        login: login.to_owned(),
    }
}

#[tokio::test]
async fn upstream_of_another_project_fires_for_a_user_who_may_dispatch_on_the_subscriber_only() {
    let (scene, release, deploy, alice, bob) = upstream_scene().await;
    scene.follow(&release, &deploy).await;
    let by_alice = scene
        .start(&release, &user(alice, "alice"), None, Conclusion::Success)
        .await;
    let by_bob = scene
        .start(&release, &user(bob, "bob"), None, Conclusion::Success)
        .await;
    let restricted = bosun_db::tokens::create_personal(
        scene.site.db(),
        alice,
        NewToken {
            name: "view".to_owned(),
            sha256: vec![5; 32],
            expires_at: Utc::now() + TimeDelta::days(1),
        },
        &BTreeSet::from([Permission::ProjectView]),
        None,
    )
    .await
    .expect("a token");
    let by_token = scene
        .start(
            &release,
            &Actor::Token {
                id: restricted.id,
                user_id: alice,
                login: "alice".to_owned(),
            },
            None,
            Conclusion::Success,
        )
        .await;
    scene.drain().await;

    let runs = scene.runs(&deploy).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].event, "upstream");
    assert_eq!(runs[0].trigger["actor"]["login"], "alice");
    let event = &runs[0].trigger["payload"];
    assert_eq!(event["id"], by_alice.to_string());
    assert_eq!(event["conclusion"], "success");
    assert_eq!(event["project"]["path"], "globalart/keel");
    assert_eq!(event["workflow"]["name"], "release");
    assert_eq!(
        shape(event),
        shape(&fixture_payload("github/upstream.json"))
    );
    assert_eq!(scene.skips(by_alice).await, Vec::new());
    for refused in [by_bob, by_token] {
        assert_eq!(
            scene.skips(refused).await,
            [(deploy.project_id, SkipReason::NoPermission)]
        );
    }
}

fn fixture_payload(name: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("a fixture");
    let fixture: serde_json::Value = serde_json::from_str(&text).expect("a JSON fixture");
    fixture["payload"].clone()
}

fn shape(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), shape(value)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.first().map(shape).into_iter().collect())
        }
        _ => serde_json::Value::Null,
    }
}

#[tokio::test]
async fn a_run_without_a_bosun_user_fires_no_upstream_of_another_project_and_says_why_once_per_project()
 {
    let (scene, release, deploy, _, _) = upstream_scene().await;
    let app = bosun_db::projects::find(scene.site.db(), deploy.project_id)
        .await
        .expect("a project")
        .expect("the project");
    scene
        .create(
            &app,
            "deploy-again",
            &following("  upstream:\n    project: globalart/keel\n    workflows: [release]\n"),
        )
        .await;
    scene.follow(&release, &deploy).await;
    let scheduled = scene
        .start(&release, &Actor::System, None, Conclusion::Success)
        .await;
    let pushed = scene
        .start(&release, &forge("octocat"), None, Conclusion::Success)
        .await;
    scene.drain().await;
    assert_eq!(scene.runs(&deploy).await, Vec::new());
    for run in [scheduled, pushed] {
        assert_eq!(
            scene.skips(run).await,
            [(deploy.project_id, SkipReason::NoUser)]
        );
    }
    scene.drain().await;
    assert_eq!(
        scene.skips(scheduled).await.len(),
        1,
        "a skip is written once"
    );
    let event = bosun_db::journal::read(scene.site.db(), scheduled, 0)
        .await
        .expect("the journal")
        .into_iter()
        .find(|event| matches!(event.event, Event::TriggerSkipped(_)))
        .expect("a skip");
    assert_eq!(
        serde_json::to_value(&event.event).expect("an event")["data"],
        json!({ "project_id": deploy.project_id, "reason": "no_user" })
    );
}

#[tokio::test]
async fn a_project_the_upstream_does_not_list_follows_nothing_and_leaves_no_trace_in_its_journal() {
    let (scene, release, deploy, alice, _) = upstream_scene().await;
    let by_alice = scene
        .start(&release, &user(alice, "alice"), None, Conclusion::Success)
        .await;
    scene.drain().await;
    assert_eq!(scene.runs(&deploy).await, Vec::new());
    assert_eq!(scene.skips(by_alice).await, Vec::new());

    scene.follow(&release, &deploy).await;
    scene
        .start(&release, &user(alice, "alice"), None, Conclusion::Success)
        .await;
    scene.drain().await;
    assert_eq!(scene.runs(&deploy).await.len(), 1, "a listed project");

    let removed = scene
        .site
        .send(
            Method::DELETE,
            &format!(
                "/api/v1/projects/{}/followers/{}",
                release.project_id, deploy.project_id
            ),
            &scene.owner,
            None,
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{removed:?}");
    scene
        .start(&release, &user(alice, "alice"), None, Conclusion::Success)
        .await;
    scene.drain().await;
    assert_eq!(
        scene.runs(&deploy).await.len(),
        1,
        "a project that was removed"
    );
}

#[tokio::test]
async fn the_followers_of_a_project_are_managed_by_who_manages_it_and_a_project_one_cannot_view_is_missing()
 {
    let (scene, release, deploy, _, _) = upstream_scene().await;
    let (viewer, viewer_credentials) = scene.site.user("viewer").await;
    let (manager, manager_credentials) = scene.site.user("manager").await;
    let keel = release.project_id;
    for (user, role) in [(viewer, "viewer"), (manager, "admin")] {
        let project = bosun_db::projects::find(scene.site.db(), keel)
            .await
            .expect("a project")
            .expect("the project");
        scene
            .site
            .bind(
                role,
                Subject::User(user),
                project.organisation_id,
                Some(keel),
            )
            .await;
    }
    let path = format!("/api/v1/projects/{keel}/followers/{}", deploy.project_id);

    let refused = scene
        .site
        .send(Method::PUT, &path, &viewer_credentials, None)
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{refused:?}");

    let unseen = scene
        .site
        .send(Method::PUT, &path, &manager_credentials, None)
        .await;
    assert_eq!(unseen.status, StatusCode::NOT_FOUND, "{unseen:?}");

    let added = scene
        .site
        .send(Method::PUT, &path, &scene.owner, None)
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{added:?}");
    assert_eq!(added.body["id"], json!(deploy.project_id));
    let again = scene
        .site
        .send(Method::PUT, &path, &scene.owner, None)
        .await;
    assert_eq!(again.status, StatusCode::OK, "{again:?}");
    let list = format!("/api/v1/projects/{keel}/followers");
    let own = scene
        .site
        .send(
            Method::PUT,
            &format!("/api/v1/projects/{keel}/followers/{keel}"),
            &scene.owner,
            None,
        )
        .await;
    assert_eq!(own.status, StatusCode::UNPROCESSABLE_ENTITY, "{own:?}");
    assert_eq!(own.body["error"]["fields"][0]["field"], "follower");

    let hidden = scene.site.get(&list, &viewer_credentials).await;
    assert_eq!(hidden.status, StatusCode::FORBIDDEN, "{hidden:?}");
    let unseen = scene.site.get(&list, &manager_credentials).await;
    assert_eq!(unseen.status, StatusCode::OK, "{unseen:?}");
    assert_eq!(
        unseen.body["items"],
        json!([{ "id": deploy.project_id, "organisation_id": null, "path": null }])
    );

    let app = bosun_db::projects::find(scene.site.db(), deploy.project_id)
        .await
        .expect("a project")
        .expect("the project");
    scene
        .site
        .bind(
            "viewer",
            Subject::User(manager),
            app.organisation_id,
            Some(app.id),
        )
        .await;
    let seen = scene.site.get(&list, &manager_credentials).await;
    assert_eq!(
        seen.body["items"],
        json!([{
            "id": deploy.project_id,
            "organisation_id": app.organisation_id,
            "path": "partner/app"
        }])
    );

    let removed = scene
        .site
        .send(Method::DELETE, &path, &manager_credentials, None)
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{removed:?}");
    let listed = scene.site.get(&list, &manager_credentials).await;
    assert_eq!(listed.body["items"], json!([]));
}

#[tokio::test]
async fn a_revoked_or_expired_token_carries_no_authority_to_the_run_it_started() {
    let (scene, release, deploy, alice, _) = upstream_scene().await;
    scene.follow(&release, &deploy).await;
    let mut tokens = Vec::new();
    for sha in [7_u8, 8, 9] {
        tokens.push(
            bosun_db::tokens::create_personal(
                scene.site.db(),
                alice,
                NewToken {
                    name: format!("token-{sha}"),
                    sha256: vec![sha; 32],
                    expires_at: Utc::now() + TimeDelta::days(1),
                },
                &BTreeSet::from([Permission::ProjectView, Permission::RunDispatch]),
                None,
            )
            .await
            .expect("a token"),
        );
    }
    bosun_db::tokens::revoke(scene.site.db(), alice, tokens[1].id, Utc::now())
        .await
        .expect("a revocation");
    scene
        .site
        .db()
        .execute_unprepared(&format!(
            "UPDATE bosun.api_token SET expires_at = now() - interval '1 hour' WHERE id = '{}'",
            tokens[2].id
        ))
        .await
        .expect("an expiry");
    let mut runs = Vec::new();
    for token in &tokens {
        runs.push(
            scene
                .start(
                    &release,
                    &Actor::Token {
                        id: token.id,
                        user_id: alice,
                        login: "alice".to_owned(),
                    },
                    None,
                    Conclusion::Success,
                )
                .await,
        );
    }
    scene.drain().await;
    assert_eq!(scene.runs(&deploy).await.len(), 1, "only the live token");
    assert_eq!(scene.skips(runs[0]).await, Vec::new());
    for refused in &runs[1..] {
        assert_eq!(
            scene.skips(*refused).await,
            [(deploy.project_id, SkipReason::NoPermission)]
        );
    }
}

#[tokio::test]
async fn a_conclusions_filter_excludes_a_failed_upstream_run() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let release = scene
        .create(
            &project,
            "release",
            &named("release", "  workflow_dispatch:\n"),
        )
        .await;
    let follow = scene
        .create(
            &project,
            "follow",
            &following("  upstream:\n    workflows: [release]\n"),
        )
        .await;
    scene
        .start(&release, &Actor::System, None, Conclusion::Failure)
        .await;
    scene.drain().await;
    assert_eq!(scene.runs(&follow).await, Vec::new(), "a failed run");
    scene
        .start(&release, &Actor::System, None, Conclusion::Success)
        .await;
    scene.drain().await;
    assert_eq!(scene.runs(&follow).await.len(), 1, "a successful run");
}

#[tokio::test]
async fn the_fourth_level_of_a_chain_creates_no_run() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let mut chain = vec![
        scene
            .create(&project, "a", &named("A", "  workflow_dispatch:\n"))
            .await,
    ];
    for (name, after) in [("b", "A"), ("c", "B"), ("d", "C"), ("e", "D")] {
        let types = if name == "e" {
            "requested, in_progress, completed"
        } else {
            "completed"
        };
        let trigger = format!("  workflow_run:\n    workflows: [{after}]\n    types: [{types}]\n");
        let source = named(&name.to_uppercase(), &trigger);
        chain.push(scene.create(&project, name, &source).await);
    }
    let mut last = scene
        .start(&chain[0], &Actor::System, None, Conclusion::Success)
        .await;
    for (level, workflow) in chain.iter().enumerate().take(4).skip(1) {
        scene.drain().await;
        let runs = scene.runs(workflow).await;
        assert_eq!(runs.len(), 1, "level {level}");
        last = run_id(&runs[0]);
        scene
            .site
            .world
            .finish(last, "build", finished(Conclusion::Success))
            .await;
    }
    scene.drain().await;
    assert_eq!(scene.runs(&chain[4]).await, Vec::new(), "the fourth level");
    assert_eq!(
        scene.skips(last).await,
        [(project.id, SkipReason::ChainTooLong)]
    );
}

fn run_id(run: &run::Model) -> RunId {
    RunId::try_from(run.id).expect("a run id")
}

#[tokio::test]
async fn saving_a_server_side_workflow_moves_its_subscriptions_with_its_source() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    scene.create(&project, "follow", &following(AFTER_CI)).await;
    let follow = scene.named(&project, "follow").await;
    let rows = || async {
        upstream_subscription::Entity::find()
            .filter(upstream_subscription::Column::WorkflowId.eq(follow.id.as_uuid()))
            .all(scene.site.db())
            .await
            .expect("the subscriptions")
    };
    let subscribed = rows().await;
    assert_eq!(subscribed.len(), 1);
    assert_eq!(subscribed[0].trigger, "workflow_run");
    assert_eq!(subscribed[0].source_project, None);
    assert_eq!(
        subscribed[0].filter,
        json!({ "workflow_run": {
            "workflows": ["CI"],
            "types": ["completed"],
            "branches": { "include": ["main"] },
        } })
    );

    let saved = scene
        .site
        .send(
            Method::PUT,
            &format!("/api/v1/workflows/{}/source", follow.id),
            &scene.owner,
            Some(json!({ "source": following(
                "  upstream:\n    project: partner/app\n    workflows: [release]\n"
            ) })),
        )
        .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{saved:?}");
    let moved = rows().await;
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].trigger, "upstream");
    assert_eq!(moved[0].source_project.as_deref(), Some("partner/app"));

    let saved = scene
        .site
        .send(
            Method::PUT,
            &format!("/api/v1/workflows/{}/source", follow.id),
            &scene.owner,
            Some(json!({ "source": following("  workflow_dispatch:\n") })),
        )
        .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{saved:?}");
    assert_eq!(rows().await, Vec::new());
}

const AFTER_CI_STARTED: &str = "  workflow_run:\n    workflows: [CI]\n    types: [requested, completed]\n    \
                                branches: [main]\n";

#[tokio::test]
async fn a_run_awaiting_admission_fires_nothing_and_fires_again_once_it_is_admitted() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let ci = scene.create(&project, "main-ci", CI).await;
    let follow = scene
        .create(&project, "follow", &following(AFTER_CI_STARTED))
        .await;

    let fork = scene
        .dispatch(
            &ci,
            &forge("octocat"),
            Some("refs/heads/main"),
            Trust::Untrusted { admitted: false },
        )
        .await;
    scene.drain().await;
    assert_eq!(
        scene.runs(&follow).await,
        Vec::new(),
        "a run awaiting admission"
    );

    scene
        .site
        .world
        .command(
            fork,
            &CommandRequest {
                command: Command::RunAdmit,
                target: Target::default(),
                reason: None,
            },
        )
        .await;
    scene
        .site
        .world
        .finish(fork, "build", finished(Conclusion::Success))
        .await;
    scene.drain().await;
    let runs = scene.runs(&follow).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].trigger["payload"]["action"], "completed");
}

#[tokio::test]
async fn a_subscription_that_cannot_be_read_is_skipped_and_the_others_still_fire() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let ci = scene.create(&project, "main-ci", CI).await;
    let good = scene.create(&project, "follow", &following(AFTER_CI)).await;
    let bad = scene
        .create(&project, "follow-bad", &following(AFTER_CI))
        .await;
    scene
        .site
        .db()
        .execute_unprepared(&format!(
            "UPDATE bosun.upstream_subscription SET filter = '{{\"workflow_run\": 5}}' \
             WHERE workflow_id = '{}'",
            bad.id
        ))
        .await
        .expect("a corrupted row");
    scene
        .start(
            &ci,
            &forge("octocat"),
            Some("refs/heads/main"),
            Conclusion::Success,
        )
        .await;
    scene.drain().await;
    assert_eq!(scene.runs(&good).await.len(), 1);
    assert_eq!(scene.runs(&bad).await, Vec::new());
    let waiting = scene
        .site
        .db()
        .query_one(Statement::from_string(
            scene.site.db().get_database_backend(),
            "SELECT count(*) AS waiting FROM bosun.outbox WHERE kind = 'run_trigger'",
        ))
        .await
        .expect("the outbox")
        .expect("a count");
    assert_eq!(
        waiting.try_get::<i64>("", "waiting").expect("a count"),
        0,
        "every row of the outbox was delivered"
    );
}

#[tokio::test]
async fn subscriptions_are_read_from_the_default_branch_and_forgotten_with_the_repository() {
    let mut pushes = Pushes::new("2026-11-07T09:00:00Z").await;
    let path = ".bosun/workflows/follow.yml";
    let source = following(AFTER_CI);
    let db = pushes.world.db().clone();
    let rows = || async {
        upstream_subscription::Entity::find()
            .all(&db)
            .await
            .expect("the subscriptions")
    };

    pushes.push("feature", &[(path, &source)], ROMAN).await;
    pushes.pull_request("feature", ROMAN).await;
    assert_eq!(rows().await, Vec::new(), "a branch and a pull request");

    pushes.push("main", &[(path, &source)], ROMAN).await;
    let registered = rows().await;
    assert_eq!(registered.len(), 1, "{registered:?}");
    assert_eq!(registered[0].trigger, "workflow_run");
    assert_eq!(registered[0].git_ref.as_deref(), Some("refs/heads/main"));

    pushes
        .push(
            "feature",
            &[(".bosun/workflows/second.yml", &source)],
            ROMAN,
        )
        .await;
    assert_eq!(rows().await.len(), 1, "a push to another branch");

    pushes.remove("main", &[path], ROMAN).await;
    assert_eq!(
        rows().await,
        Vec::new(),
        "a file removed from the default branch"
    );

    pushes.push("main", &[(path, &source)], ROMAN).await;
    assert_eq!(rows().await.len(), 1);
    bosun_db::projects::bind(&db, pushes.world.workflow.project_id, None)
        .await
        .expect("an unbinding");
    assert_eq!(rows().await, Vec::new(), "a repository that was unbound");
}

#[tokio::test]
async fn a_requested_row_that_is_processed_after_the_admission_still_fires_nothing() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let ci = scene.create(&project, "main-ci", CI).await;
    let follow = scene
        .create(&project, "follow", &following(AFTER_CI_STARTED))
        .await;
    let fork = scene
        .dispatch(
            &ci,
            &forge("octocat"),
            Some("refs/heads/main"),
            Trust::Untrusted { admitted: false },
        )
        .await;
    scene
        .site
        .world
        .command(
            fork,
            &CommandRequest {
                command: Command::RunAdmit,
                target: Target::default(),
                reason: None,
            },
        )
        .await;
    scene
        .site
        .world
        .finish(fork, "build", finished(Conclusion::Success))
        .await;
    scene.drain().await;
    let runs = scene.runs(&follow).await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].trigger["payload"]["action"], "completed");
}

#[tokio::test]
async fn a_run_cancelled_while_it_awaits_its_admission_can_never_be_admitted_and_fires_nothing() {
    let scene = Scene::new().await;
    let project = scene.site.project().await;
    let ci = scene.create(&project, "main-ci", CI).await;
    let follow = scene
        .create(&project, "follow", &following(AFTER_CI_STARTED))
        .await;
    let fork = scene
        .dispatch(
            &ci,
            &forge("octocat"),
            Some("refs/heads/main"),
            Trust::Untrusted { admitted: false },
        )
        .await;
    let command = |command| CommandRequest {
        command,
        target: Target::default(),
        reason: None,
    };
    let cancelled = scene
        .site
        .world
        .command(fork, &command(Command::RunCancel))
        .await;
    assert!(matches!(cancelled, Applied::Accepted(_)));
    let admitted = scene
        .site
        .world
        .command(fork, &command(Command::RunAdmit))
        .await;
    assert!(matches!(admitted, Applied::Refused(_)), "{admitted:?}");
    scene.drain().await;
    assert_eq!(scene.runs(&follow).await, Vec::new());
}
