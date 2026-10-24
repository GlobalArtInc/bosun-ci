use std::time::Duration;

use bosun_db::entity::{workflow, workflow_revision};
use bosun_db::workflows::{self, NewWorkflow};
use bosun_db::{Error, organisations, projects};
use bosun_testkit::TestDatabase;
use bosun_types::api::{Project, WorkflowSource};
use sea_orm::{DatabaseConnection, EntityTrait, PaginatorTrait, TransactionTrait};
use tokio::time::sleep;

const FIRST: &str = "name: ci\non: workflow_dispatch\njobs: {}\n";

const SECOND: &str = "name: ci\non: workflow_dispatch\njobs: {}\n\n";

fn sha256(byte: u8) -> Vec<u8> {
    vec![byte; 32]
}

async fn project(db: &DatabaseConnection) -> Project {
    let organisation = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    projects::create(db, organisation.id, "ops", "Operations")
        .await
        .expect("a project")
}

fn new<'a>(project: &Project, name: &'a str, source: &'a str, sha256: Vec<u8>) -> NewWorkflow<'a> {
    NewWorkflow {
        project_id: project.id,
        name,
        source,
        sha256,
    }
}

#[tokio::test]
async fn a_new_workflow_is_enabled_and_its_source_is_its_current_revision() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    let created = workflows::create(db, new(&project, "ci", FIRST, sha256(1)))
        .await
        .expect("a workflow");
    assert_eq!(created.project_id, project.id);
    assert_eq!(created.name, "ci");
    assert_eq!(created.source, WorkflowSource::Server);
    assert!(created.enabled);
    let revision_id = created.revision_id.expect("a current revision");
    let revision = workflows::revision(db, revision_id)
        .await
        .expect("a lookup")
        .expect("the revision");
    assert_eq!(revision.workflow_id, created.id);
    assert_eq!(revision.source, FIRST);
    assert_eq!(revision.sha256, "01".repeat(32));
    assert_eq!(
        workflows::find(db, created.id).await.expect("a lookup"),
        Some(created)
    );
}

#[tokio::test]
async fn saving_a_source_the_workflow_had_makes_its_revision_current_without_a_new_one() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    let created = workflows::create(db, new(&project, "ci", FIRST, sha256(1)))
        .await
        .expect("a workflow");
    let first = created.revision_id.expect("a current revision");

    let same = workflows::save(db, created.id, FIRST, sha256(1))
        .await
        .expect("a save");
    assert!(!same.created);
    assert_eq!(same.revision.id, first);

    let second = workflows::save(db, created.id, SECOND, sha256(2))
        .await
        .expect("a save");
    assert!(second.created);
    assert_ne!(second.revision.id, first);
    assert_eq!(
        current(db, &created).await,
        Some(second.revision.id),
        "the second source is not current"
    );

    let back = workflows::save(db, created.id, FIRST, sha256(1))
        .await
        .expect("a save");
    assert!(!back.created);
    assert_eq!(back.revision.id, first);
    assert_eq!(current(db, &created).await, Some(first));
    assert_eq!(
        workflow_revision::Entity::find()
            .count(db)
            .await
            .expect("a count"),
        2
    );
}

async fn current(
    db: &DatabaseConnection,
    workflow: &bosun_types::api::Workflow,
) -> Option<bosun_types::id::RevisionId> {
    workflows::find(db, workflow.id)
        .await
        .expect("a lookup")
        .and_then(|found| found.revision_id)
}

#[tokio::test]
async fn a_name_is_taken_once_per_project_and_a_refused_create_leaves_nothing() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    workflows::create(db, new(&project, "ci", FIRST, sha256(1)))
        .await
        .expect("a workflow");
    let transaction = db.begin().await.expect("a transaction");
    let again = workflows::create(&transaction, new(&project, "ci", SECOND, sha256(2))).await;
    assert!(
        matches!(&again, Err(Error::WorkflowTaken(name)) if name == "ci"),
        "{again:?}"
    );
    transaction.rollback().await.expect("a rollback");
    assert_eq!(
        workflow::Entity::find().count(db).await.expect("a count"),
        1
    );
    assert_eq!(
        workflow_revision::Entity::find()
            .count(db)
            .await
            .expect("a count"),
        1
    );
}

#[tokio::test]
async fn workflows_list_oldest_first_and_revisions_newest_first_after_a_cursor() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    let mut created = Vec::new();
    for (byte, name) in [(1, "build"), (2, "deploy"), (3, "nightly")] {
        created.push(
            workflows::create(db, new(&project, name, FIRST, sha256(byte)))
                .await
                .expect("a workflow"),
        );
    }
    let all = workflows::list(db, project.id, None, None, 10)
        .await
        .expect("a list");
    assert_eq!(all, created);
    let after = workflows::list(db, project.id, None, Some(created[0].id.as_uuid()), 1)
        .await
        .expect("a list");
    assert_eq!(after, vec![created[1].clone()]);
    let named = workflows::list(db, project.id, Some("nightly"), None, 10)
        .await
        .expect("a list");
    assert_eq!(named, vec![created[2].clone()]);

    let workflow = &created[0];
    let mut saved = vec![workflow.revision_id.expect("a current revision")];
    for (byte, source) in [
        (4, SECOND),
        (5, "name: ci\non: workflow_dispatch\njobs: {}\n\n\n"),
    ] {
        saved.push(
            workflows::save(db, workflow.id, source, sha256(byte))
                .await
                .expect("a save")
                .revision
                .id,
        );
    }
    let newest_first: Vec<_> = workflows::revisions(db, workflow.id, None, 10)
        .await
        .expect("the revisions")
        .into_iter()
        .map(|revision| revision.id)
        .collect();
    assert_eq!(
        newest_first,
        saved.iter().rev().copied().collect::<Vec<_>>()
    );
    let older: Vec<_> = workflows::revisions(db, workflow.id, Some(saved[2].as_uuid()), 1)
        .await
        .expect("the revisions")
        .into_iter()
        .map(|revision| revision.id)
        .collect();
    assert_eq!(older, vec![saved[1]]);
}

#[tokio::test]
async fn a_create_waits_for_a_create_of_the_same_project_and_then_finds_the_name_taken() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let project = project(db).await;
    let first = db.begin().await.expect("a transaction");
    workflows::create(&first, new(&project, "ci", FIRST, sha256(1)))
        .await
        .expect("a workflow");

    let second = {
        let pool = bosun_db::connect(&database.url(), 2)
            .await
            .expect("a second pool");
        let project_id = project.id;
        tokio::spawn(async move {
            let transaction = pool.begin().await?;
            let created = workflows::create(
                &transaction,
                NewWorkflow {
                    project_id,
                    name: "ci",
                    source: SECOND,
                    sha256: sha256(2),
                },
            )
            .await;
            drop(transaction);
            pool.close().await?;
            created
        })
    };
    sleep(Duration::from_millis(500)).await;
    assert!(
        !second.is_finished(),
        "a second create of the name went ahead while the first was not committed"
    );
    first.commit().await.expect("a commit");
    let second = second.await.expect("the second create");
    assert!(
        matches!(&second, Err(Error::WorkflowTaken(name)) if name == "ci"),
        "{second:?}"
    );
    assert_eq!(
        workflow::Entity::find().count(db).await.expect("a count"),
        1
    );
}
