use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use bosun_db::installations::Account;
use bosun_db::runs::{NewRun, Trust};
use bosun_testkit::forge::{APP_ID, APP_KEY, FakeForge, RecordedCheckRun, RecordedStatus};
use bosun_types::api::{Annotation, AnnotationKind, Reporting, Repository};
use bosun_types::event::Event;
use bosun_types::id::{JobId, RunId};
use bosun_types::ir;
use bosun_types::status::Conclusion;
use chrono::{TimeDelta, Utc};
use sea_orm::{ConnectionTrait, Statement, TransactionTrait};
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use tracing_subscriber::fmt::MakeWriter;

use super::{World, finished, job_cancel, operator, site, template, workflow};
use crate::agents::progress::{self, Progress, StepEnd};
use crate::clock::Clock;
use crate::config::GitHub;
use crate::metrics::Meters;
use crate::outbox::{self, Outbox};
use crate::planner;

const FULL_NAME: &str = "GlobalArt/keel";

const PERMISSIONS: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "read"),
    ("metadata", "read"),
    ("statuses", "write"),
];

pub(super) struct Scene {
    pub(super) world: World,
    pub(super) fake: FakeForge,
    pub(super) outbox: Outbox,
    pub(super) clock: Clock,
    sha: String,
    _files: TempDir,
}

impl Scene {
    pub(super) async fn new() -> Scene {
        let world = World::new().await;
        let fake = FakeForge::start().await;
        let installation = fake.install("GlobalArt", PERMISSIONS);
        let repository_id = fake.create_repository(installation, FULL_NAME);
        let sha = fake.commit(FULL_NAME, "main", [("README.md", "# keel\n")]);
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
        let clock = Clock::manual(Utc::now() + TimeDelta::minutes(10));
        Scene {
            outbox: Outbox {
                db: db.clone(),
                store: Arc::clone(world.store()),
                forge: Arc::new(forge),
                keys: Arc::new(super::keys()),
                site: site(),
                clock: clock.clone(),
                meters: Arc::new(Meters::default()),
            },
            world,
            fake,
            clock,
            sha,
            _files: files,
        }
    }

    pub(super) async fn dispatch(&self, workflow: &ir::Workflow) -> (RunId, JobId) {
        let transaction = self.world.db().begin().await.expect("a transaction");
        let payload = json!({});
        let run = planner::dispatch(
            &transaction,
            NewRun {
                workflow: &self.world.workflow,
                revision_id: self.world.workflow.revision_id.expect("a revision"),
                ir: workflow,
                event: "push",
                git_ref: Some("refs/heads/main"),
                sha: Some(&self.sha),
                payload: &payload,
                inputs: &Map::new(),
                actor: &operator(),
                trust: Trust::Trusted,
            },
            &[],
            &site(),
        )
        .await
        .expect("a dispatch");
        transaction.commit().await.expect("a commit");
        let (job, _) = self.world.project(run.id).await.job("build");
        (run.id, job)
    }

    pub(super) fn tick(&self, by: TimeDelta) {
        self.clock.advance(by);
    }

    pub(super) async fn drain(&self) -> usize {
        let mut handled = 0;
        while outbox::next(&self.outbox)
            .await
            .unwrap_or_else(|error| panic!("the outbox failed: {error}"))
        {
            handled += 1;
        }
        handled
    }

    fn reports(&self) -> Vec<(String, u16, Option<Value>)> {
        self.fake
            .requests()
            .into_iter()
            .filter(|request| {
                request.path.contains("/check-runs") || request.path.contains("/statuses/")
            })
            .map(|request| (request.method, request.status, request.body))
            .collect()
    }

    async fn rows(&self) -> Vec<(i64, Option<String>, bool)> {
        let db = self.world.db();
        db.query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT attempts::bigint AS attempts, last_error, dead_at IS NOT NULL AS dead \
             FROM bosun.outbox ORDER BY id",
        ))
        .await
        .expect("the outbox")
        .into_iter()
        .map(|row| {
            (
                row.try_get("", "attempts").expect("attempts"),
                row.try_get("", "last_error").expect("an error"),
                row.try_get("", "dead").expect("a death"),
            )
        })
        .collect()
    }
}

fn details(run: RunId, job: JobId) -> Option<String> {
    Some(format!("https://ci.example.org/runs/{run}/jobs/{job}"))
}

pub(super) fn one_job() -> ir::Workflow {
    workflow(vec![template("build", &[], 1)])
}

#[tokio::test]
async fn while_the_forge_fails_a_job_changes_state_five_times_and_is_reported_once_with_its_latest_state()
 {
    let scene = Scene::new().await;
    scene.fake.fail(true);
    let (run, job) = scene.dispatch(&one_job()).await;
    assert_eq!(scene.drain().await, 1);

    let world = &scene.world;
    let execution = world.assign(run, job).await;
    let rejected = progress::rejected(world.db(), world.agent(), execution)
        .await
        .expect("a rejection");
    assert_eq!(rejected, Progress::Applied);
    world.assign(run, job).await;
    assert_eq!(scene.drain().await, 0, "a postponed row was tried early");
    world.command(run, &job_cancel(job)).await;
    let changes = world
        .journal(run)
        .await
        .into_iter()
        .filter(|event| {
            event.job_id == Some(job) && matches!(event.event, Event::JobStatusChanged(_))
        })
        .count();
    assert_eq!(changes, 5);

    scene.tick(TimeDelta::seconds(2));
    assert_eq!(scene.drain().await, 1);
    assert_eq!(
        scene.rows().await,
        [(
            2,
            Some("the forge failed: the forge answered 502: Server Error".to_owned()),
            false
        )],
        "the newest row does not carry the failures of the row it replaced"
    );
    assert_eq!(scene.reports(), Vec::new());

    scene.fake.fail(false);
    scene.tick(TimeDelta::seconds(4));
    assert_eq!(scene.drain().await, 1);
    let reported = scene.reports();
    assert_eq!(reported.len(), 1, "{reported:#?}");
    assert_eq!((reported[0].0.as_str(), reported[0].1), ("POST", 201));
    assert_eq!(
        scene.fake.check_runs(),
        [RecordedCheckRun {
            id: scene.fake.check_runs()[0].id,
            repository: FULL_NAME.to_owned(),
            head_sha: scene.sha.clone(),
            name: "build".to_owned(),
            details_url: details(run, job),
            status: "completed".to_owned(),
            conclusion: Some("cancelled".to_owned()),
            output: None,
        }]
    );
    assert_eq!(scene.rows().await, Vec::new());
    assert_eq!(scene.drain().await, 0);
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

impl Captured {
    fn lines(&self) -> Vec<Value> {
        let bytes = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

#[tokio::test]
async fn a_row_that_keeps_failing_for_24_hours_is_dead_logged_at_error_level_and_not_tried_again() {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(captured.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);
    let scene = Scene::new().await;
    scene.fake.fail(true);
    scene.dispatch(&one_job()).await;

    for hour in 0..24 {
        assert_eq!(scene.drain().await, 1, "hour {hour}");
        let rows = scene.rows().await;
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].2, "the row died after {hour} hours");
        scene.tick(TimeDelta::hours(1));
    }
    assert_eq!(scene.drain().await, 1);
    let rows = scene.rows().await;
    assert_eq!(
        rows,
        [(
            25,
            Some("the forge failed: the forge answered 502: Server Error".to_owned()),
            true
        )]
    );
    let dead: Vec<Value> = captured
        .lines()
        .into_iter()
        .filter(|line| line["fields"]["message"] == "an outbox row is dead and is no longer tried")
        .collect();
    assert_eq!(dead.len(), 1, "{:#?}", captured.lines());
    assert_eq!(dead[0]["level"], "ERROR");
    assert_eq!(dead[0]["fields"]["attempts"], 25);
    assert_eq!(dead[0]["fields"]["kind"], "check_run");

    let attempted = scene.fake.requests().len();
    scene.fake.fail(false);
    scene.tick(TimeDelta::days(2));
    assert_eq!(scene.drain().await, 0);
    assert_eq!(scene.fake.requests().len(), attempted);
    assert_eq!(scene.fake.check_runs(), Vec::new());
}

#[tokio::test]
async fn with_statuses_a_run_reports_commit_statuses_with_bosun_contexts_and_no_check_run() {
    let scene = Scene::new().await;
    let db = scene.world.db();
    let project = scene.world.workflow.project_id;
    let mut settings = bosun_db::projects::settings(db, project)
        .await
        .expect("the settings")
        .expect("the project");
    settings.settings.report = Reporting::Statuses;
    bosun_db::projects::change_settings(db, project, settings.version, &settings.settings)
        .await
        .expect("a change")
        .expect("the changed settings");

    let (run, job) = scene.dispatch(&one_job()).await;
    scene.drain().await;
    let execution = scene.world.start(run, job).await;
    scene.drain().await;
    scene
        .world
        .end(execution, finished(Conclusion::Success))
        .await;
    scene.drain().await;

    let transaction = db.begin().await.expect("a transaction");
    let payload = json!({});
    let broken = workflow(Vec::new());
    let failed = planner::fail_startup(
        &transaction,
        NewRun {
            workflow: &scene.world.workflow,
            revision_id: scene.world.workflow.revision_id.expect("a revision"),
            ir: &broken,
            event: "push",
            git_ref: Some("refs/heads/main"),
            sha: Some(&scene.sha),
            payload: &payload,
            inputs: &Map::new(),
            actor: &operator(),
            trust: Trust::Trusted,
        },
        &[],
    )
    .await
    .expect("a startup failure");
    transaction.commit().await.expect("a commit");
    scene.drain().await;

    let status = |context: &str, state: &str, description: &str, target: String| RecordedStatus {
        repository: FULL_NAME.to_owned(),
        sha: scene.sha.clone(),
        context: context.to_owned(),
        state: state.to_owned(),
        description: Some(description.to_owned()),
        target_url: Some(target),
    };
    let target = details(run, job).expect("an address");
    assert_eq!(
        scene.fake.statuses(),
        [
            status("bosun/ci/build", "pending", "Queued", target.clone()),
            status("bosun/ci/build", "pending", "In progress", target.clone()),
            status("bosun/ci/build", "success", "Succeeded", target),
            status(
                "bosun/ci",
                "failure",
                "The workflow could not start",
                format!("https://ci.example.org/runs/{}", failed.id)
            ),
        ]
    );
    assert_eq!(scene.fake.check_runs(), Vec::new());
}

#[tokio::test]
async fn a_startup_failure_is_reported_as_a_settled_check_run_named_after_its_workflow() {
    let scene = Scene::new().await;
    let transaction = scene.world.db().begin().await.expect("a transaction");
    let payload = json!({});
    let broken = workflow(Vec::new());
    let failed = planner::fail_startup(
        &transaction,
        NewRun {
            workflow: &scene.world.workflow,
            revision_id: scene.world.workflow.revision_id.expect("a revision"),
            ir: &broken,
            event: "push",
            git_ref: Some("refs/heads/main"),
            sha: Some(&scene.sha),
            payload: &payload,
            inputs: &Map::new(),
            actor: &operator(),
            trust: Trust::Trusted,
        },
        &[],
    )
    .await
    .expect("a startup failure");
    transaction.commit().await.expect("a commit");
    assert_eq!(scene.drain().await, 1);
    let recorded = scene.fake.check_runs();
    assert_eq!(
        recorded,
        [RecordedCheckRun {
            id: recorded[0].id,
            repository: FULL_NAME.to_owned(),
            head_sha: scene.sha.clone(),
            name: "ci".to_owned(),
            details_url: Some(format!("https://ci.example.org/runs/{}", failed.id)),
            status: "completed".to_owned(),
            conclusion: Some("failure".to_owned()),
            output: None,
        }]
    );
}

fn noted(step: u32, number: u32) -> Annotation {
    Annotation {
        kind: AnnotationKind::Warning,
        message: format!("warning {number} of step {step}"),
        title: None,
        file: None,
        line: None,
        end_line: None,
        column: None,
        end_column: None,
    }
}

#[tokio::test]
async fn a_completed_job_sends_only_the_annotations_its_check_run_lacks() {
    let scene = Scene::new().await;
    let (run, job) = scene
        .dispatch(&workflow(vec![template("build", &[], 3)]))
        .await;
    assert_eq!(scene.drain().await, 1);
    let world = &scene.world;
    let execution = world.start(run, job).await;
    for step in 0..3 {
        progress::step_started(world.db(), world.agent(), execution, step, None)
            .await
            .expect("a started step");
        let ended = progress::step_finished(
            world.db(),
            world.store(),
            world.agent(),
            execution,
            StepEnd {
                index: step,
                at: None,
                outcome: Conclusion::Success,
                conclusion: Conclusion::Success,
                outputs: BTreeMap::new(),
                annotations: (0..40).map(|number| noted(step, number)).collect(),
                summary: None,
            },
        )
        .await
        .expect("a finished step");
        assert_eq!(ended, Progress::Applied);
    }
    world.end(execution, finished(Conclusion::Success)).await;
    world
        .db()
        .execute_unprepared(&format!(
            "UPDATE bosun.forge_check_run SET annotations = 50 WHERE job_id = '{job}'"
        ))
        .await
        .expect("a check run that holds 50 annotations");
    assert_eq!(scene.drain().await, 1);

    let batches: Vec<usize> = scene
        .reports()
        .into_iter()
        .filter_map(|(_, _, body)| body)
        .filter_map(|body| {
            body.pointer("/output/annotations")
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .collect();
    assert_eq!(batches, [50, 20]);
    let check_runs = scene.fake.check_runs();
    let output = check_runs[0].output.as_ref().expect("an output");
    assert_eq!(
        (output.title.as_str(), output.summary.as_str()),
        ("build", "The steps of this job wrote no summary.")
    );
    let messages: Vec<&str> = output
        .annotations
        .iter()
        .map(|annotation| annotation.message.as_str())
        .collect();
    let expected: Vec<String> = (0..3)
        .flat_map(|step| (0..40).map(move |number| format!("warning {number} of step {step}")))
        .skip(50)
        .collect();
    assert_eq!(messages, expected);
    assert!(
        output
            .annotations
            .iter()
            .all(|annotation| annotation.path == ".github" && annotation.start_line == 1),
        "{:#?}",
        output.annotations
    );
    assert_eq!(
        bosun_db::outbox::check_run(world.db(), job)
            .await
            .expect("a lookup")
            .map(|recorded| recorded.annotations),
        Some(120)
    );
}
