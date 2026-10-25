use bosun_db::installations::{self, Account};
use bosun_db::outbox::{self, Claimed, Kind, Report, State};
use bosun_db::pools::Placement;
use bosun_db::runs::{self, NewRun, Trust};
use bosun_db::workflows::{self, NewWorkflow};
use bosun_db::{organisations, projects};
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::api::{Repository, Workflow};
use bosun_types::id::{JobId, RunId};
use bosun_types::ir;
use bosun_types::status::{Conclusion, RunConclusion, StatusReason, WaitingReason};
use chrono::{DateTime, SubsecRound, TimeDelta, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde_json::{Map, Value, json};

use crate::{fixture, strings};

const INSTALLATION: u64 = 52_814_339;

const REPOSITORY: u64 = 871_264_503;

const SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const LEASE: TimeDelta = TimeDelta::minutes(5);

async fn bound(db: &DatabaseConnection) -> Workflow {
    let organisation = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let transaction = db.begin().await.expect("a transaction");
    installations::record(
        &transaction,
        INSTALLATION,
        &Account {
            id: 43_265_880,
            login: "GlobalArtInc",
        },
    )
    .await
    .expect("an installation");
    let repository = Repository {
        id: REPOSITORY,
        owner: "GlobalArtInc".to_owned(),
        name: "keel".to_owned(),
        default_branch: "main".to_owned(),
    };
    installations::sync(
        &transaction,
        INSTALLATION,
        std::slice::from_ref(&repository),
    )
    .await
    .expect("a sync");
    transaction.commit().await.expect("a commit");
    let project = projects::create(db, organisation.id, "keel", "Keel")
        .await
        .expect("a project");
    projects::bind(db, project.id, Some(&repository))
        .await
        .expect("a binding");
    workflows::create(
        db,
        NewWorkflow {
            project_id: project.id,
            name: ".bosun/workflows/ci.yml",
            source: "on: push\n",
            sha256: vec![1; 32],
        },
    )
    .await
    .expect("a workflow")
}

async fn dispatch(db: &DatabaseConnection, workflow: &Workflow, sha: Option<&str>) -> RunId {
    let mut ir: ir::Workflow =
        serde_json::from_value(fixture("ir/two-jobs.json")).expect("the IR of two jobs");
    ir.name = None;
    let payload = json!({});
    let transaction = db.begin().await.expect("a transaction");
    let run = runs::dispatch(
        &transaction,
        NewRun {
            workflow,
            revision_id: workflow.revision_id.expect("a current revision"),
            ir: &ir,
            event: "push",
            git_ref: Some("refs/heads/main"),
            sha,
            payload: &payload,
            inputs: &Map::new(),
            actor: &Actor::System,
            trust: Trust::Trusted,
        },
    )
    .await
    .expect("a run");
    transaction.commit().await.expect("a commit");
    run.id
}

async fn jobs(db: &DatabaseConnection, run: RunId) -> (JobId, JobId) {
    let transaction = db.begin().await.expect("a transaction");
    let locked = runs::lock(&transaction, run)
        .await
        .expect("a lock")
        .expect("the run");
    let jobs = locked.jobs().await.expect("the jobs");
    let find = |key: &str| {
        jobs.iter()
            .find(|job| job.key.as_str() == key)
            .map(|job| job.id)
            .expect("the job")
    };
    (find("build"), find("test"))
}

async fn rows(db: &DatabaseConnection) -> Vec<(String, String, Value)> {
    db.query_all(sea_orm::Statement::from_string(
        db.get_database_backend(),
        "SELECT kind, collapse_key, payload FROM bosun.outbox ORDER BY id",
    ))
    .await
    .expect("the outbox")
    .into_iter()
    .map(|row| {
        (
            row.try_get("", "kind").expect("a kind"),
            row.try_get("", "collapse_key").expect("a key"),
            row.try_get("", "payload").expect("a payload"),
        )
    })
    .collect()
}

fn placement() -> Placement {
    Placement {
        labels: vec!["self-hosted".to_owned()],
        pools: Vec::new(),
        capabilities: Vec::new(),
    }
}

#[tokio::test]
async fn a_report_is_written_in_the_transaction_of_the_journal_event_that_changes_what_it_shows() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let workflow = bound(db).await;
    let run = dispatch(db, &workflow, Some(SHA)).await;
    let (build, test) = jobs(db, run).await;
    assert_eq!(rows(db).await, Vec::new(), "a pending job is reported");

    let transaction = db.begin().await.expect("a transaction");
    let locked = runs::lock(&transaction, run)
        .await
        .expect("a lock")
        .expect("the run");
    locked
        .queue_job(build, &placement())
        .await
        .expect("a queued job");
    transaction.rollback().await.expect("a rollback");
    assert_eq!(rows(db).await, Vec::new(), "a rolled back report stayed");

    let transaction = db.begin().await.expect("a transaction");
    let locked = runs::lock(&transaction, run)
        .await
        .expect("a lock")
        .expect("the run");
    locked
        .queue_job(build, &placement())
        .await
        .expect("a queued job");
    locked
        .hold_job(build, &placement(), WaitingReason::Concurrency)
        .await
        .expect("a held job");
    locked
        .complete_job(
            test,
            Conclusion::Cancelled,
            Some(StatusReason::Command),
            &Actor::System,
        )
        .await
        .expect("a cancelled job");
    transaction.commit().await.expect("a commit");

    let report = |job: JobId, name: &str, state: State| {
        serde_json::to_value(Report {
            repository_id: REPOSITORY,
            sha: SHA.to_owned(),
            run_id: run,
            job_id: Some(job),
            workflow: "ci".to_owned(),
            name: name.to_owned(),
            state,
        })
        .expect("a payload")
    };
    assert_eq!(
        rows(db).await,
        [
            (
                "check_run".to_owned(),
                format!("check_run/job/{build}"),
                report(build, "Build", State::Queued)
            ),
            (
                "check_run".to_owned(),
                format!("check_run/job/{test}"),
                report(test, "test", State::Completed(RunConclusion::Cancelled))
            ),
        ]
    );
    assert_eq!(
        rows(db).await[0].2["state"],
        json!({ "status": "queued" }),
        "{:?}",
        rows(db).await
    );

    let elsewhere = dispatch(db, &workflow, None).await;
    let (build, _) = jobs(db, elsewhere).await;
    let transaction = db.begin().await.expect("a transaction");
    let locked = runs::lock(&transaction, elsewhere)
        .await
        .expect("a lock")
        .expect("the run");
    locked
        .queue_job(build, &placement())
        .await
        .expect("a queued job");
    transaction.commit().await.expect("a commit");
    assert_eq!(
        rows(db).await.len(),
        2,
        "a run without a commit is reported"
    );
}

async fn insert(db: &DatabaseConnection, key: &str, label: &str) -> u64 {
    let found = db
        .query_one(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO bosun.outbox (kind, collapse_key, payload) VALUES ('check_run', $1, $2) \
             RETURNING id",
            [key.into(), json!({ "label": label }).into()],
        ))
        .await
        .expect("an outbox row")
        .expect("its id");
    u64::try_from(found.try_get::<i64>("", "id").expect("an id")).expect("a positive id")
}

async fn claim(db: &DatabaseConnection, now: DateTime<Utc>) -> Option<Claimed> {
    let transaction = db.begin().await.expect("a transaction");
    let claimed = outbox::claim(&transaction, now, now + LEASE)
        .await
        .expect("a claim");
    transaction.commit().await.expect("a commit");
    claimed
}

fn later() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(6) + TimeDelta::seconds(1)
}

#[tokio::test]
async fn a_claim_takes_the_oldest_row_of_a_key_and_hands_over_the_newest_once() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    for label in ["queued", "in progress", "completed"] {
        insert(db, "check_run/job/a", label).await;
    }
    let other = insert(db, "check_run/job/b", "queued").await;
    let now = later();

    let first = db.begin().await.expect("a transaction");
    let claimed = outbox::claim(&first, now, now + LEASE)
        .await
        .expect("a claim")
        .expect("a row");
    assert_eq!(
        (
            claimed.kind,
            claimed.collapse_key.as_str(),
            &claimed.payload,
            claimed.attempts,
            claimed.dropped
        ),
        (
            Kind::CheckRun,
            "check_run/job/a",
            &json!({ "label": "completed" }),
            0,
            2
        )
    );
    let second = db.begin().await.expect("a transaction");
    let concurrent = outbox::claim(&second, now, now + LEASE)
        .await
        .expect("a claim")
        .expect("a row");
    assert_eq!(
        concurrent.id, other,
        "a second worker took a row of a held key"
    );
    first.commit().await.expect("a commit");
    second.commit().await.expect("a commit");
    assert_eq!(claim(db, now).await, None, "a leased row was claimed again");

    let newer = insert(db, "check_run/job/a", "completed again").await;
    assert_eq!(
        claim(db, now).await,
        None,
        "a row was claimed while an older one of its key is delivered"
    );
    assert!(outbox::delivered(db, &claimed).await.expect("a delivery"));
    let next = claim(db, later()).await.expect("the newer row");
    assert_eq!((next.id, next.dropped), (newer, 0));
    assert_eq!(outbox::pending(db).await.expect("the backlog"), 2);
}

#[tokio::test]
async fn a_failed_row_waits_carries_its_failures_to_a_newer_row_and_dies_without_blocking_its_key()
{
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    insert(db, "check_run/job/a", "queued").await;
    let now = later();
    let claimed = claim(db, now).await.expect("a row");
    let retry = now + TimeDelta::seconds(1);
    assert!(
        outbox::postpone(db, &claimed, "the forge failed: 502", now, retry)
            .await
            .expect("a postponed row")
    );
    assert_eq!(
        claim(db, now).await,
        None,
        "a postponed row was claimed early"
    );
    let newer = insert(db, "check_run/job/a", "completed").await;
    let carried = claim(db, retry).await.expect("a row");
    assert_eq!(
        (
            carried.id,
            carried.attempts,
            carried.failing_since,
            carried.dropped
        ),
        (newer, 1, Some(now), 1)
    );

    let stale = carried.clone();
    let after = retry + LEASE + TimeDelta::seconds(1);
    let again = claim(db, after).await.expect("a row whose lease ran out");
    assert_eq!((again.id, again.attempts), (newer, 2));
    assert!(
        !outbox::delivered(db, &stale).await.expect("a delivery"),
        "a delivery under an expired lease settled the row"
    );
    assert!(
        outbox::kill(db, &again, "the forge refused: 422", now, after)
            .await
            .expect("a dead row")
    );
    assert_eq!(outbox::pending(db).await.expect("the backlog"), 0);
    let later_day = after + TimeDelta::days(2);
    assert_eq!(claim(db, later_day).await, None, "a dead row was claimed");
    assert_eq!(
        strings(
            db,
            "SELECT last_error AS value FROM bosun.outbox WHERE dead_at IS NOT NULL"
        )
        .await,
        ["the forge refused: 422"]
    );

    let fresh = insert(db, "check_run/job/a", "completed at last").await;
    let claimed = claim(db, later_day).await.expect("a row behind a dead one");
    assert_eq!(
        (claimed.id, claimed.attempts, claimed.dropped),
        (fresh, 0, 0)
    );
    assert!(outbox::delivered(db, &claimed).await.expect("a delivery"));
    assert_eq!(
        strings(db, "SELECT id::text AS value FROM bosun.outbox").await,
        Vec::<String>::new(),
        "a dead row outlived a newer delivered one"
    );
}
