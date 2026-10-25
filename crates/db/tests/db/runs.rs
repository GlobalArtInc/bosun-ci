use bosun_db::entity::{attempt, job, run, run_event};
use bosun_db::journal;
use bosun_db::runs::{self, NewRun};
use bosun_db::workflows::{self, NewWorkflow};
use bosun_db::{organisations, projects};
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::api::Workflow;
use bosun_types::event::{AttemptCreated, Event, JobCreated, RunCreated};
use bosun_types::id::{TokenId, UserId};
use bosun_types::ir;
use bosun_types::status::RunStatus;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    TransactionTrait,
};
use serde_json::json;

use crate::{fixture, rows};

fn actor() -> Actor {
    Actor::Token {
        id: TokenId::try_from(rows::id(rows::TOKEN)).expect("a version 7 id"),
        user_id: UserId::try_from(rows::id(rows::USER)).expect("a version 7 id"),
        login: "roman".to_owned(),
    }
}

fn two_jobs() -> ir::Workflow {
    serde_json::from_value(fixture("ir/two-jobs.json")).expect("the IR of two jobs")
}

async fn workflow(db: &DatabaseConnection, name: &str) -> Workflow {
    let organisation = match organisations::find_by_slug(db, "globalart")
        .await
        .expect("a lookup")
    {
        Some(organisation) => organisation,
        None => organisations::create(db, "globalart", "GlobalArt")
            .await
            .expect("an organisation"),
    };
    let project = match projects::find_by_slugs(db, "globalart", "ops")
        .await
        .expect("a lookup")
    {
        Some(project) => project,
        None => projects::create(db, organisation.id, "ops", "Operations")
            .await
            .expect("a project"),
    };
    workflows::create(
        db,
        NewWorkflow {
            project_id: project.id,
            name,
            source: "name: ci\n",
            sha256: vec![1; 32],
        },
    )
    .await
    .expect("a workflow")
}

#[tokio::test]
async fn a_dispatch_creates_the_run_its_first_attempt_its_pending_jobs_and_their_events() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let workflow = workflow(db, "ci").await;
    let revision_id = workflow.revision_id.expect("a current revision");
    let ir = two_jobs();
    let actor = actor();
    let payload = json!({ "inputs": null, "ref": null, "workflow": "ci" });

    let transaction = db.begin().await.expect("a transaction");
    let created = runs::dispatch(
        &transaction,
        NewRun {
            workflow: &workflow,
            revision_id,
            ir: &ir,
            payload: &payload,
            actor: &actor,
        },
    )
    .await
    .expect("a run");
    transaction.commit().await.expect("a commit");

    assert_eq!(created.project_id, workflow.project_id);
    assert_eq!(created.workflow_id, workflow.id);
    assert_eq!(created.revision_id, revision_id);
    assert_eq!(created.number, 1);
    assert_eq!(created.display_name, "ci");
    assert_eq!(created.notices, []);
    assert_eq!(created.event, "workflow_dispatch");
    assert_eq!(created.status, RunStatus::Queued);
    assert_eq!(created.conclusion, None);
    assert_eq!(created.attempt, 1);
    assert_eq!(created.actor, actor);
    assert_eq!((created.started_at, created.completed_at), (None, None));

    let stored = run::Entity::find_by_id(created.id.as_uuid())
        .one(db)
        .await
        .expect("a query")
        .expect("the run");
    assert_eq!(stored.ir, fixture("ir/two-jobs.json"));
    assert_eq!(stored.status, "queued");
    assert_eq!(
        stored.global_number,
        i64::try_from(created.global_number).expect("a number")
    );
    assert_eq!(stored.inputs, json!({}));
    assert_eq!(
        stored.trigger,
        json!({ "actor": actor, "payload": payload })
    );
    assert_eq!(stored.display_name.as_deref(), Some("ci"));
    assert_eq!((stored.git_ref, stored.sha), (None, None));

    let attempts = attempt::Entity::find()
        .filter(attempt::Column::RunId.eq(stored.id))
        .all(db)
        .await
        .expect("the attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].number, 1);
    assert_eq!(
        attempts[0].actor,
        serde_json::to_value(&actor).expect("JSON")
    );

    let jobs = job::Entity::find()
        .filter(job::Column::AttemptId.eq(attempts[0].id))
        .order_by_asc(job::Column::Id)
        .all(db)
        .await
        .expect("the jobs");
    let shapes: Vec<_> = jobs
        .iter()
        .map(|job| {
            (
                job.key.as_str(),
                job.name.as_str(),
                job.needs.clone(),
                job.status.as_str(),
                job.labels.clone(),
                job.queued_at,
            )
        })
        .collect();
    assert_eq!(
        shapes,
        vec![
            (
                "build",
                "Build",
                Vec::<String>::new(),
                "pending",
                Vec::new(),
                None
            ),
            (
                "test",
                "test",
                vec!["build".to_owned()],
                "pending",
                Vec::new(),
                None
            ),
        ]
    );

    let journal = journal::read(db, created.id, 0).await.expect("the journal");
    let attempt_id = attempts[0].id;
    let concerns: Vec<_> = journal
        .iter()
        .map(|event| {
            (
                event.attempt_id.map(|id| id.as_uuid()),
                event.job_id.map(|id| id.as_uuid()),
                event.actor.clone(),
            )
        })
        .collect();
    assert_eq!(
        concerns,
        vec![
            (None, None, actor.clone()),
            (Some(attempt_id), None, actor.clone()),
            (Some(attempt_id), Some(jobs[0].id), actor.clone()),
            (Some(attempt_id), Some(jobs[1].id), actor.clone()),
        ]
    );
    let events: Vec<_> = journal.into_iter().map(|event| event.event).collect();
    assert_eq!(
        events,
        vec![
            Event::RunCreated(RunCreated {
                workflow_id: workflow.id,
                revision_id,
                number: 1,
                global_number: created.global_number,
                event: "workflow_dispatch".to_owned(),
            }),
            Event::AttemptCreated(AttemptCreated { number: 1 }),
            Event::JobCreated(JobCreated {
                key: "build".parse().expect("a job key"),
                name: "Build".to_owned(),
                needs: Vec::new(),
            }),
            Event::JobCreated(JobCreated {
                key: "test".parse().expect("a job key"),
                name: "test".to_owned(),
                needs: vec!["build".parse().expect("a job key")],
            }),
        ]
    );
}

#[tokio::test]
async fn run_numbers_count_per_workflow_and_a_rolled_back_dispatch_uses_none() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let ci = workflow(db, "ci").await;
    let nightly = workflow(db, "nightly").await;
    let ir = two_jobs();
    let actor = actor();
    let dispatch = |workflow: &Workflow| {
        let workflow = workflow.clone();
        let ir = ir.clone();
        let actor = actor.clone();
        let db = db.clone();
        async move {
            runs::dispatch(
                &db,
                NewRun {
                    workflow: &workflow,
                    revision_id: workflow.revision_id.expect("a current revision"),
                    ir: &ir,
                    payload: &json!({}),
                    actor: &actor,
                },
            )
            .await
            .expect("a run")
        }
    };

    let first = dispatch(&ci).await;
    let other = dispatch(&nightly).await;
    let transaction = db.begin().await.expect("a transaction");
    runs::dispatch(
        &transaction,
        NewRun {
            workflow: &ci,
            revision_id: ci.revision_id.expect("a current revision"),
            ir: &ir,
            payload: &json!({}),
            actor: &actor,
        },
    )
    .await
    .expect("a run that is rolled back");
    transaction.rollback().await.expect("a rollback");
    let second = dispatch(&ci).await;

    assert_eq!((first.number, other.number, second.number), (1, 1, 2));
    assert!(first.global_number < other.global_number);
    assert!(other.global_number < second.global_number);
    assert_eq!(run::Entity::find().count(db).await.expect("a count"), 3);
    let rolled_back: Vec<Uuid> = run_event::Entity::find()
        .all(db)
        .await
        .expect("the journal")
        .into_iter()
        .map(|event| event.run_id)
        .filter(|run| {
            ![first.id, other.id, second.id]
                .iter()
                .any(|kept| kept.as_uuid() == *run)
        })
        .collect();
    assert!(
        rolled_back.is_empty(),
        "events of a rolled back run: {rolled_back:?}"
    );
}
