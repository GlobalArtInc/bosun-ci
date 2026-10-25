use std::collections::BTreeMap;

use axum::http::{Method, StatusCode};
use bosun_db::deliveries::Delivery;
use bosun_db::entity::{run, schedule};
use bosun_db::installations::Account;
use bosun_testkit::forge::{APP_ID, APP_KEY, BOT, FakeForge, NO_COMMIT, Sender};
use bosun_types::actor::Actor;
use bosun_types::api::{Repository, Run};
use bosun_types::id::WorkflowId;
use bosun_types::status::{RunConclusion, RunStatus};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::access::{Answer, Credentials, Site, owner_credentials};
use super::{World, site};
use crate::clock::Clock;
use crate::config::GitHub;
use crate::forge::Forge;
use crate::forge_events::{self, Failed, Parses};
use crate::schedules;

const BELGRADE: &str = "name: nightly\non:\n  schedule:\n    - cron: '30 2 * * *'\n      \
                        timezone: Europe/Belgrade\njobs:\n  build:\n    runs-on: [self-hosted]\n    \
                        steps:\n      - run: make\n";

const FULL_NAME: &str = "GlobalArt/keel";

const ROMAN: Sender<'static> = Sender {
    login: "roman",
    id: 7001,
};

const ANNA: Sender<'static> = Sender {
    login: "anna",
    id: 7002,
};

fn at(text: &str) -> DateTime<Utc> {
    text.parse().expect("an RFC 3339 time")
}

fn to(clock: &Clock, instant: DateTime<Utc>) {
    let now = clock.now();
    assert!(
        instant >= now,
        "the clock does not go back to {instant} from {now}"
    );
    clock.advance(instant - now);
}

fn scheduled(cron: &str) -> String {
    format!(
        "name: scheduled\non:\n  schedule:\n    - cron: '{cron}'\njobs:\n  build:\n    runs-on: \
         [self-hosted]\n    steps:\n      - run: make\n"
    )
}

fn source(on: &str) -> String {
    format!(
        "name: scheduled\non:\n{on}jobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      \
         - run: make\n"
    )
}

async fn payload(db: &sea_orm::DatabaseConnection, run: &Run) -> Value {
    run::Entity::find_by_id(run.id.as_uuid())
        .one(db)
        .await
        .expect("a run")
        .expect("the run")
        .trigger["payload"]
        .clone()
}

async fn next_at(db: &sea_orm::DatabaseConnection, workflow: WorkflowId) -> Vec<Option<String>> {
    schedule::Entity::find()
        .filter(schedule::Column::WorkflowId.eq(workflow.as_uuid()))
        .all(db)
        .await
        .expect("the schedules")
        .into_iter()
        .map(|row| row.next_at.map(|at| at.to_rfc3339()))
        .collect()
}

struct Scheduler {
    site: Site,
    clock: Clock,
    owner: Credentials,
}

impl Scheduler {
    async fn new(start: &str) -> Scheduler {
        let clock = Clock::manual(at(start));
        let site = Site::with_clock(clock.clone()).await;
        let owner = owner_credentials(&site).await;
        Scheduler { site, clock, owner }
    }

    async fn create(&self, name: &str, source: &str) -> WorkflowId {
        let project = self.site.project().await;
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
        created.body["id"]
            .as_str()
            .expect("an id")
            .parse()
            .expect("a workflow id")
    }

    async fn save(&self, workflow: WorkflowId, source: String) -> Answer {
        self.site
            .send(
                Method::PUT,
                &format!("/api/v1/workflows/{workflow}/source"),
                &self.owner,
                Some(json!({ "source": source })),
            )
            .await
    }

    async fn enable(&self, workflow: WorkflowId, enabled: bool) -> Answer {
        self.site
            .send(
                Method::PATCH,
                &format!("/api/v1/workflows/{workflow}"),
                &self.owner,
                Some(json!({ "enabled": enabled })),
            )
            .await
    }

    async fn round(&self) -> Vec<Run> {
        schedules::round(self.site.db(), &self.site.state.site, self.clock.now())
            .await
            .unwrap_or_else(|error| panic!("the round failed: {error}"))
    }

    async fn fires_at(&self, instant: &str) -> Run {
        let instant = at(instant);
        to(&self.clock, instant - TimeDelta::seconds(1));
        assert_eq!(self.round().await, Vec::new(), "a run before {instant}");
        to(&self.clock, instant);
        let mut runs = self.round().await;
        assert_eq!(runs.len(), 1, "the runs at {instant}: {runs:?}");
        runs.remove(0)
    }
}

#[tokio::test]
async fn a_nightly_cron_in_belgrade_fires_on_the_settled_instants_of_both_nights_of_daylight_saving_changes()
 {
    let scheduler = Scheduler::new("2026-03-27T12:00:00Z").await;
    let workflow = scheduler.create("nightly", BELGRADE).await;
    assert_eq!(
        next_at(scheduler.site.db(), workflow).await,
        [Some("2026-03-28T01:30:00+00:00".to_owned())]
    );

    let mut runs = Vec::new();
    for instant in [
        "2026-03-28T01:30:00Z",
        "2026-03-29T01:00:00Z",
        "2026-03-30T00:30:00Z",
    ] {
        runs.push(scheduler.fires_at(instant).await);
    }

    to(&scheduler.clock, at("2026-10-23T12:00:00Z"));
    let overdue = scheduler.round().await;
    assert_eq!(overdue.len(), 1, "the missed nights fire once: {overdue:?}");
    runs.extend(overdue);
    assert_eq!(scheduler.round().await, Vec::new());
    for instant in ["2026-10-24T00:30:00Z", "2026-10-25T00:30:00Z"] {
        runs.push(scheduler.fires_at(instant).await);
    }
    to(&scheduler.clock, at("2026-10-25T01:30:00Z"));
    assert_eq!(
        scheduler.round().await,
        Vec::new(),
        "the second 02:30 of the night the clocks go back"
    );
    runs.push(scheduler.fires_at("2026-10-26T01:30:00Z").await);

    let db = scheduler.site.db();
    let owner = &runs[0].actor;
    assert!(
        matches!(owner, Actor::User { login, .. } if login.starts_with("owner-")),
        "{owner:?}"
    );
    for (number, run) in (1..).zip(&runs) {
        assert_eq!(run.workflow_id, workflow);
        assert_eq!(run.number, number);
        assert_eq!(run.event, "schedule");
        assert_eq!(run.display_name, "nightly");
        assert_eq!(&run.actor, owner);
        assert_eq!((&run.r#ref, &run.sha), (&None, &None));
        assert_eq!(run.status, RunStatus::Queued);
        assert_eq!(payload(db, run).await, json!({ "schedule": "30 2 * * *" }));
    }
}

#[tokio::test]
async fn a_server_side_workflow_saved_with_a_schedule_fires_and_one_saved_without_stops() {
    let scheduler = Scheduler::new("2026-10-25T09:58:30Z").await;
    let workflow = scheduler
        .create("hourly", &source("  workflow_dispatch:\n"))
        .await;
    assert_eq!(next_at(scheduler.site.db(), workflow).await, Vec::new());

    let saved = scheduler.save(workflow, scheduled("*/5 * * * *")).await;
    assert_eq!(saved.status, StatusCode::CREATED, "{saved:?}");
    let run = scheduler.fires_at("2026-10-25T10:00:00Z").await;
    assert_eq!(run.workflow_id, workflow);
    assert_eq!(
        run.revision_id.to_string(),
        saved.body["id"].as_str().expect("an id")
    );
    assert_eq!(
        payload(scheduler.site.db(), &run).await,
        json!({ "schedule": "*/5 * * * *" })
    );
    scheduler.fires_at("2026-10-25T10:05:00Z").await;

    let saved = scheduler
        .save(
            workflow,
            source("  schedule:\n    - cron: '0 12 * * *'\n    - cron: '*/5 * * * *'\n"),
        )
        .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{saved:?}");
    assert_eq!(
        next_at(scheduler.site.db(), workflow).await.len(),
        2,
        "the kept cron and the new one"
    );
    scheduler.fires_at("2026-10-25T10:10:00Z").await;

    let saved = scheduler
        .save(workflow, source("  workflow_dispatch:\n"))
        .await;
    assert_eq!(
        saved.status,
        StatusCode::OK,
        "the first source becomes current again: {saved:?}"
    );
    assert_eq!(next_at(scheduler.site.db(), workflow).await, Vec::new());
    to(&scheduler.clock, at("2026-10-26T12:00:00Z"));
    assert_eq!(scheduler.round().await, Vec::new());
}

#[tokio::test]
async fn a_disabled_workflows_schedule_creates_no_run_and_does_not_catch_up_when_enabled() {
    let scheduler = Scheduler::new("2026-10-25T09:58:30Z").await;
    let workflow = scheduler.create("disabled", &scheduled("0 * * * *")).await;
    let disabled = scheduler.enable(workflow, false).await;
    assert_eq!(disabled.status, StatusCode::OK, "{disabled:?}");

    to(&scheduler.clock, at("2026-10-25T10:00:00Z"));
    assert_eq!(scheduler.round().await, Vec::new());
    assert_eq!(
        next_at(scheduler.site.db(), workflow).await,
        [Some("2026-10-25T11:00:00+00:00".to_owned())]
    );
    to(&scheduler.clock, at("2026-10-25T10:30:00Z"));
    let enabled = scheduler.enable(workflow, true).await;
    assert_eq!(enabled.status, StatusCode::OK, "{enabled:?}");
    assert_eq!(scheduler.round().await, Vec::new());
    let run = scheduler.fires_at("2026-10-25T11:00:00Z").await;
    assert_eq!(run.workflow_id, workflow);
}

#[tokio::test]
async fn with_two_replicas_every_schedule_fires_once_a_minute() {
    let scheduler = Scheduler::new("2026-10-25T09:59:30Z").await;
    let mut workflows = Vec::new();
    for number in 0..12 {
        workflows.push(
            scheduler
                .create(&format!("every-minute-{number}"), &scheduled("* * * * *"))
                .await,
        );
    }
    let other = bosun_db::connect(&scheduler.site.world.database.url(), 4)
        .await
        .expect("the pool of a second replica");
    let addresses = &scheduler.site.state.site;
    let mut fired: BTreeMap<WorkflowId, Vec<u64>> = BTreeMap::new();
    for _ in 0..3 {
        to(
            &scheduler.clock,
            scheduler.clock.now() + TimeDelta::minutes(1),
        );
        let now = scheduler.clock.now();
        let (left, right) = tokio::join!(
            schedules::round(scheduler.site.db(), addresses, now),
            schedules::round(&other, addresses, now)
        );
        let left = left.unwrap_or_else(|error| panic!("a round failed: {error}"));
        let right = right.unwrap_or_else(|error| panic!("a round failed: {error}"));
        assert_eq!(
            left.len() + right.len(),
            workflows.len(),
            "{left:?} {right:?}"
        );
        for run in left.into_iter().chain(right) {
            fired.entry(run.workflow_id).or_default().push(run.number);
        }
    }
    let expected: BTreeMap<WorkflowId, Vec<u64>> = workflows
        .iter()
        .map(|workflow| (*workflow, vec![1, 2, 3]))
        .collect();
    for numbers in fired.values_mut() {
        numbers.sort_unstable();
    }
    assert_eq!(fired, expected);
}

struct Pushes {
    world: World,
    installation: u64,
    fake: FakeForge,
    forge: Forge,
    parses: Parses,
    clock: Clock,
    heads: BTreeMap<String, String>,
    _files: TempDir,
}

impl Pushes {
    async fn new(start: &str) -> Pushes {
        let world = World::new().await;
        let fake = FakeForge::start().await;
        let installation = fake.install(
            "GlobalArt",
            &[
                ("contents", "read"),
                ("metadata", "read"),
                ("checks", "write"),
            ],
        );
        let repository_id = fake.create_repository(installation, FULL_NAME);
        let first = fake.commit(FULL_NAME, "main", [("README.md", "# keel\n")]);
        let db = world.db();
        let transaction = db.begin().await.expect("a transaction");
        bosun_db::installations::record(
            &transaction,
            installation,
            &Account {
                id: 43_265_880,
                login: "GlobalArt",
            },
        )
        .await
        .expect("an installation");
        let repository = Repository {
            id: repository_id,
            owner: "GlobalArt".to_owned(),
            name: "keel".to_owned(),
            default_branch: "main".to_owned(),
        };
        bosun_db::installations::sync(
            &transaction,
            installation,
            std::slice::from_ref(&repository),
        )
        .await
        .expect("a sync");
        transaction.commit().await.expect("a commit");
        bosun_db::projects::bind(db, world.workflow.project_id, Some(&repository))
            .await
            .expect("a binding");
        let files = tempfile::tempdir().expect("a directory for the App's files");
        let key = files.path().join("app.pem");
        let secret = files.path().join("webhook-secret");
        std::fs::write(&key, APP_KEY).expect("the App's key");
        std::fs::write(&secret, fake.webhook_secret()).expect("the webhook secret");
        let mut server_url = fake.api_url();
        server_url.set_path("/");
        let forge = crate::forge::load(&GitHub {
            app_id: APP_ID,
            private_key_file: key,
            webhook_secret_file: secret,
            server_url,
            api_url: fake.api_url(),
        })
        .unwrap_or_else(|error| panic!("the App does not load: {error}"));
        Pushes {
            world,
            installation,
            fake,
            forge,
            parses: Parses::default(),
            clock: Clock::manual(at(start)),
            heads: BTreeMap::from([("main".to_owned(), first)]),
            _files: files,
        }
    }

    async fn push(&mut self, branch: &str, files: &[(&str, &str)], sender: Sender<'_>) -> String {
        let before = self
            .heads
            .get(branch)
            .cloned()
            .unwrap_or_else(|| NO_COMMIT.to_owned());
        let after = self.fake.commit(FULL_NAME, branch, files.iter().copied());
        self.heads.insert(branch.to_owned(), after.clone());
        let payload = self.fake.push_event(
            FULL_NAME,
            &format!("refs/heads/{branch}"),
            &before,
            &after,
            sender,
        );
        let transaction = self.world.db().begin().await.expect("a transaction");
        let delivered = forge_events::delivered(
            &transaction,
            &self.forge,
            &self.parses,
            &site(),
            &Delivery {
                id: Uuid::now_v7(),
                delivery: Uuid::now_v7().to_string(),
                event: "push".to_owned(),
                payload,
                attempts: 0,
            },
            self.clock.now(),
        )
        .await;
        match delivered {
            Ok(()) => {}
            Err(Failed::Database(error)) => panic!("the push failed in the database: {error}"),
            Err(Failed::Forge(error)) => panic!("the push failed at the forge: {error}"),
            Err(Failed::Payload(error)) => panic!("the push has a broken payload: {error}"),
            Err(Failed::Unmerged(error)) => panic!("the push waits for a merge: {error}"),
        }
        transaction.commit().await.expect("a commit");
        after
    }

    async fn round(&self, instant: &str) -> Vec<Run> {
        to(&self.clock, at(instant));
        schedules::round(self.world.db(), &site(), self.clock.now())
            .await
            .unwrap_or_else(|error| panic!("the round failed: {error}"))
    }

    async fn rows(&self) -> Vec<(String, Value, Option<String>, Option<String>)> {
        schedule::Entity::find()
            .all(self.world.db())
            .await
            .expect("the schedules")
            .into_iter()
            .map(|row| (row.expression, row.actor, row.git_ref, row.sha))
            .collect()
    }
}

fn forge_actor(sender: Sender<'_>) -> Actor {
    Actor::Forge {
        id: sender.id,
        login: sender.login.to_owned(),
    }
}

#[tokio::test]
async fn pushes_to_the_default_branch_rebuild_its_schedules_and_one_that_removes_a_schedule_stops_it()
 {
    let mut pushes = Pushes::new("2026-10-25T09:00:00Z").await;
    let path = ".bosun/workflows/nightly.yml";
    let first = pushes
        .push("main", &[(path, &scheduled("0 3 * * *"))], ROMAN)
        .await;
    let roman = json!({ "kind": "forge", "id": ROMAN.id, "login": ROMAN.login });
    let main = Some("refs/heads/main".to_owned());
    assert_eq!(
        pushes.rows().await,
        [(
            "0 3 * * *".to_owned(),
            roman.clone(),
            main.clone(),
            Some(first.clone())
        )]
    );
    let runs = pushes.round("2026-10-26T03:00:00Z").await;
    let [run] = runs.as_slice() else {
        panic!("the runs at 03:00: {runs:?}");
    };
    assert_eq!(run.event, "schedule");
    assert_eq!(run.r#ref, main);
    assert_eq!(run.sha.as_deref(), Some(first.as_str()));
    assert_eq!(run.actor, forge_actor(ROMAN));
    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(
        payload(pushes.world.db(), run).await,
        json!({ "schedule": "0 3 * * *" })
    );

    pushes
        .push(
            "feature",
            &[(path, &source("  workflow_dispatch:\n"))],
            ANNA,
        )
        .await;
    let readme = pushes
        .push("main", &[("README.md", "# keel, by the bot\n")], BOT)
        .await;
    assert_eq!(
        pushes.rows().await,
        [(
            "0 3 * * *".to_owned(),
            roman.clone(),
            main.clone(),
            Some(readme.clone())
        )],
        "a push to another branch changes nothing, and one that keeps the cron keeps its actor"
    );
    let runs = pushes.round("2026-10-27T03:00:00Z").await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].sha.as_deref(), Some(readme.as_str()));

    let changed = pushes
        .push("main", &[(path, &scheduled("0 4 * * *"))], ANNA)
        .await;
    let anna = json!({ "kind": "forge", "id": ANNA.id, "login": ANNA.login });
    assert_eq!(
        pushes.rows().await,
        [("0 4 * * *".to_owned(), anna, main.clone(), Some(changed))]
    );
    assert_eq!(pushes.round("2026-10-27T03:59:00Z").await, Vec::new());
    let runs = pushes.round("2026-10-27T04:00:00Z").await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].actor, forge_actor(ANNA));

    pushes
        .push("main", &[(path, &source("  workflow_dispatch:\n"))], ROMAN)
        .await;
    assert_eq!(pushes.rows().await, Vec::new());
    assert_eq!(pushes.round("2026-10-30T12:00:00Z").await, Vec::new());
}

#[tokio::test]
async fn a_scheduled_run_of_a_broken_file_ends_as_a_startup_failure() {
    let mut pushes = Pushes::new("2026-10-25T09:00:00Z").await;
    let broken = "on:\n  schedule:\n    - cron: '0 3 * * *'\njobs:\n  build:\n    runs_on: shell\n    \
                  steps:\n      - run: make\n";
    pushes
        .push("main", &[(".bosun/workflows/broken.yml", broken)], ROMAN)
        .await;
    let runs = pushes.round("2026-10-26T03:00:00Z").await;
    let [run] = runs.as_slice() else {
        panic!("the runs at 03:00: {runs:?}");
    };
    assert_eq!(run.event, "schedule");
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.conclusion, Some(RunConclusion::StartupFailure));
    assert!(
        run.notices
            .iter()
            .any(|notice| notice.diagnostic.code == "unknown_key"),
        "{:?}",
        run.notices
    );
}

#[tokio::test]
async fn the_schedule_of_a_repository_no_installation_sees_creates_no_run() {
    let mut pushes = Pushes::new("2026-10-25T09:00:00Z").await;
    pushes
        .push(
            "main",
            &[(".bosun/workflows/nightly.yml", &scheduled("0 3 * * *"))],
            ROMAN,
        )
        .await;
    let transaction = pushes.world.db().begin().await.expect("a transaction");
    bosun_db::installations::sync(&transaction, pushes.installation, &[])
        .await
        .expect("a sync that hides the repository");
    transaction.commit().await.expect("a commit");
    assert_eq!(pushes.round("2026-10-26T03:00:00Z").await, Vec::new());
    let next: Vec<_> = schedule::Entity::find()
        .all(pushes.world.db())
        .await
        .expect("the schedules")
        .into_iter()
        .map(|row| row.next_at)
        .collect();
    assert_eq!(next, [Some(at("2026-10-27T03:00:00Z"))]);
}
