use bosun_db::entity::schedule;
use bosun_db::installations::{self, Account};
use bosun_db::schedules::{self, Entry, Origin, Replaced, Scope};
use bosun_db::workflows::{self, NewWorkflow};
use bosun_db::{organisations, projects};
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::api::{Project, Repository, Workflow};
use bosun_types::ir::{Cron, CronDays};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    TransactionTrait,
};
use serde_json::json;

const INSTALLATION: u64 = 52_814_339;

const NIGHTLY: &str = "30 2 * * *";

const MORNING: &str = "0 7 * * 1-5";

fn cron(expression: &str) -> Cron {
    Cron {
        expression: expression.to_owned(),
        minutes: vec![30],
        hours: vec![2],
        days_of_month: (1..=31).collect(),
        months: (1..=12).collect(),
        days_of_week: (0..=6).collect(),
        days: CronDays::Both,
    }
}

fn at(text: &str) -> DateTime<Utc> {
    text.parse().expect("an RFC 3339 time")
}

fn forge(login: &str, id: u64) -> Actor {
    Actor::Forge {
        id,
        login: login.to_owned(),
    }
}

fn entry<'a>(
    workflow: &Workflow,
    expression: &'a str,
    timezone: &'a str,
    cron: &'a Cron,
    next_at: &str,
) -> Entry<'a> {
    Entry {
        workflow_id: workflow.id,
        revision_id: workflow.revision_id.expect("a current revision"),
        expression,
        timezone,
        cron,
        next_at: Some(at(next_at)),
    }
}

async fn project(db: &DatabaseConnection) -> Project {
    let organisation = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    projects::create(db, organisation.id, "ops", "Operations")
        .await
        .expect("a project")
}

async fn server_side(db: &DatabaseConnection, project: &Project, name: &str) -> Workflow {
    workflows::create(
        db,
        NewWorkflow {
            project_id: project.id,
            name,
            source: "on: workflow_dispatch\njobs: {}\n",
            sha256: vec![name.len().try_into().expect("a short name"); 32],
        },
    )
    .await
    .expect("a workflow")
}

async fn rows(db: &DatabaseConnection) -> Vec<(String, String, serde_json::Value, String)> {
    schedule::Entity::find()
        .order_by_asc(schedule::Column::Expression)
        .order_by_asc(schedule::Column::Timezone)
        .all(db)
        .await
        .expect("the schedules")
        .into_iter()
        .map(|row| {
            (
                row.expression,
                row.timezone,
                row.actor,
                row.next_at.map(|at| at.to_rfc3339()).unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_replaced_entry_keeps_its_actor_and_its_instant_and_a_missing_one_goes() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    let workflow = server_side(db, &project, "nightly").await;
    let (nightly, morning) = (cron(NIGHTLY), cron(MORNING));
    let roman = forge("roman", 7001);
    let replaced = schedules::replace(
        db,
        Scope::Workflow(workflow.id),
        &[
            entry(
                &workflow,
                NIGHTLY,
                "Europe/Belgrade",
                &nightly,
                "2026-10-26T01:30:00Z",
            ),
            entry(
                &workflow,
                NIGHTLY,
                "Europe/Belgrade",
                &nightly,
                "2026-10-26T01:30:00Z",
            ),
            entry(&workflow, MORNING, "UTC", &morning, "2026-10-26T07:00:00Z"),
        ],
        Origin {
            git_ref: None,
            sha: None,
            actor: &roman,
        },
    )
    .await
    .expect("the schedules");
    assert_eq!(
        replaced,
        Replaced {
            added: 2,
            kept: 0,
            removed: 0
        }
    );

    let saved = workflows::save(db, workflow.id, "on: push\njobs: {}\n", vec![9; 32])
        .await
        .expect("a save");
    let workflow = workflows::find(db, workflow.id)
        .await
        .expect("a lookup")
        .expect("the workflow");
    let anna = forge("anna", 7002);
    let replaced = schedules::replace(
        db,
        Scope::Workflow(workflow.id),
        &[
            entry(
                &workflow,
                NIGHTLY,
                "Europe/Belgrade",
                &nightly,
                "2026-10-27T01:30:00Z",
            ),
            entry(&workflow, NIGHTLY, "UTC", &nightly, "2026-10-26T02:30:00Z"),
        ],
        Origin {
            git_ref: None,
            sha: None,
            actor: &anna,
        },
    )
    .await
    .expect("the schedules");
    assert_eq!(
        replaced,
        Replaced {
            added: 1,
            kept: 1,
            removed: 1
        }
    );
    assert_eq!(
        rows(db).await,
        [
            (
                NIGHTLY.to_owned(),
                "Europe/Belgrade".to_owned(),
                json!({ "kind": "forge", "id": 7001, "login": "roman" }),
                "2026-10-26T01:30:00+00:00".to_owned()
            ),
            (
                NIGHTLY.to_owned(),
                "UTC".to_owned(),
                json!({ "kind": "forge", "id": 7002, "login": "anna" }),
                "2026-10-26T02:30:00+00:00".to_owned()
            ),
        ]
    );
    let revisions: Vec<_> = schedule::Entity::find()
        .all(db)
        .await
        .expect("the schedules")
        .into_iter()
        .map(|row| row.revision_id)
        .collect();
    assert_eq!(
        revisions,
        vec![saved.revision.id.as_uuid(), saved.revision.id.as_uuid()]
    );
}

#[tokio::test]
async fn the_repository_scope_holds_the_files_of_the_project_and_rebinding_forgets_them() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
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
    let ops = Repository {
        id: 871_264_503,
        owner: "GlobalArtInc".to_owned(),
        name: "ops".to_owned(),
        default_branch: "main".to_owned(),
    };
    let keel = Repository {
        id: 871_264_504,
        name: "keel".to_owned(),
        ..ops.clone()
    };
    installations::sync(&transaction, INSTALLATION, &[ops.clone(), keel.clone()])
        .await
        .expect("a sync");
    transaction.commit().await.expect("a commit");
    projects::bind(db, project.id, Some(&ops))
        .await
        .expect("a binding");
    let files = workflows::repository_files(db, project.id, &[".bosun/workflows/nightly.yml"])
        .await
        .expect("the files");
    let file = &files[0];
    let revision = workflows::add_blob(
        db,
        file.id,
        workflows::Blob {
            sha: "b1",
            source: "on:\n  schedule:\n    - cron: '30 2 * * *'\njobs: {}\n",
            sha256: vec![3; 32],
        },
    )
    .await
    .expect("a revision");
    workflows::make_current(db, file.id, revision.id)
        .await
        .expect("the current revision");
    let file = workflows::find(db, file.id)
        .await
        .expect("a lookup")
        .expect("the file");
    let server = server_side(db, &project, "backup").await;
    let nightly = cron(NIGHTLY);
    let roman = forge("roman", 7001);
    for (scope, workflow) in [
        (Scope::Repository(project.id), &file),
        (Scope::Workflow(server.id), &server),
    ] {
        schedules::replace(
            db,
            scope,
            &[entry(
                workflow,
                NIGHTLY,
                "UTC",
                &nightly,
                "2026-10-26T02:30:00Z",
            )],
            Origin {
                git_ref: Some("refs/heads/main"),
                sha: Some("a1"),
                actor: &roman,
            },
        )
        .await
        .expect("the schedules");
    }
    let of = |workflow: &Workflow| {
        schedule::Entity::find()
            .filter(schedule::Column::WorkflowId.eq(workflow.id.as_uuid()))
            .all(db)
    };
    assert_eq!(of(&file).await.expect("the schedules").len(), 1);

    let replaced = schedules::replace(
        db,
        Scope::Repository(project.id),
        &[],
        Origin {
            git_ref: Some("refs/heads/main"),
            sha: Some("a2"),
            actor: &roman,
        },
    )
    .await
    .expect("the schedules");
    assert_eq!(replaced.removed, 1);
    assert_eq!(of(&server).await.expect("the schedules").len(), 1);

    schedules::replace(
        db,
        Scope::Repository(project.id),
        &[entry(
            &file,
            NIGHTLY,
            "UTC",
            &nightly,
            "2026-10-26T02:30:00Z",
        )],
        Origin {
            git_ref: Some("refs/heads/main"),
            sha: Some("a3"),
            actor: &roman,
        },
    )
    .await
    .expect("the schedules");
    projects::bind(db, project.id, Some(&ops))
        .await
        .expect("the same binding");
    assert_eq!(of(&file).await.expect("the schedules").len(), 1);
    projects::bind(db, project.id, Some(&keel))
        .await
        .expect("another binding");
    assert!(of(&file).await.expect("the schedules").is_empty());
    assert_eq!(of(&server).await.expect("the schedules").len(), 1);
}

#[tokio::test]
async fn due_schedules_are_found_under_the_lock_of_the_round_and_a_locked_one_is_skipped() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    let workflow = server_side(db, &project, "nightly").await;
    let nightly = cron(NIGHTLY);
    let roman = forge("roman", 7001);
    schedules::replace(
        db,
        Scope::Workflow(workflow.id),
        &[
            entry(&workflow, NIGHTLY, "UTC", &nightly, "2026-10-26T02:30:00Z"),
            entry(&workflow, MORNING, "UTC", &nightly, "2026-10-26T07:00:00Z"),
        ],
        Origin {
            git_ref: None,
            sha: None,
            actor: &roman,
        },
    )
    .await
    .expect("the schedules");
    let now = at("2026-10-26T02:30:00Z");
    assert_eq!(
        schedules::due(db, now - TimeDelta::seconds(1))
            .await
            .expect("a round"),
        Some(Vec::new())
    );

    let holder = db.begin().await.expect("a transaction");
    holder
        .execute_unprepared(&format!(
            "SELECT pg_advisory_xact_lock({})",
            i64::from_be_bytes(*b"bosuncrn")
        ))
        .await
        .expect("the lock of the round");
    assert_eq!(schedules::due(db, now).await.expect("a round"), None);
    holder.rollback().await.expect("a rollback");

    let due = schedules::due(db, now)
        .await
        .expect("a round")
        .expect("the lock of the round");
    let [id] = due.as_slice() else {
        panic!("{due:?} are due");
    };
    let first = db.begin().await.expect("a transaction");
    let locked = schedules::lock_due(&first, *id, now)
        .await
        .expect("a lock")
        .expect("the due schedule");
    assert_eq!(locked.expression, NIGHTLY);
    assert_eq!(locked.actor, roman);
    assert_eq!(locked.next_at, now);
    let second = db.begin().await.expect("a transaction");
    assert_eq!(
        schedules::lock_due(&second, *id, now)
            .await
            .expect("a lock"),
        None
    );
    schedules::advance(&first, *id, Some(now + TimeDelta::days(1)))
        .await
        .expect("the next instant");
    first.commit().await.expect("a commit");
    assert_eq!(
        schedules::lock_due(&second, *id, now)
            .await
            .expect("a lock"),
        None
    );
    second.rollback().await.expect("a rollback");
    assert_eq!(
        schedules::due(db, now).await.expect("a round"),
        Some(Vec::new())
    );
}
