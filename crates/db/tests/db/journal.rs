use std::fs;

use bosun_db::entity::{job, run, run_event};
use bosun_db::journal::{self, NewEvent};
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::event::{AttemptCreated, Change, Event, JobStatusChanged, RunEvent};
use bosun_types::id::{JobId, RunId};
use bosun_types::status::JobStatus;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, TransactionTrait,
};
use serde_json::Value;

use crate::{fixture, repository, rows};

fn run_id(number: u8) -> RunId {
    RunId::try_from(rows::id(number)).expect("a version 7 id")
}

fn new_event(run: RunId, fixture: &RunEvent) -> NewEvent {
    NewEvent {
        run_id: run,
        attempt_id: fixture.attempt_id,
        job_id: fixture.job_id,
        execution_id: fixture.execution_id,
        step_id: fixture.step_id,
        actor: fixture.actor.clone(),
        event: fixture.event.clone(),
    }
}

fn attempt_created(run: RunId, number: u32) -> NewEvent {
    NewEvent {
        run_id: run,
        attempt_id: None,
        job_id: None,
        execution_id: None,
        step_id: None,
        actor: Actor::System,
        event: Event::AttemptCreated(AttemptCreated { number }),
    }
}

fn event_fixtures() -> Vec<(String, Value)> {
    let directory = repository().join("contracts/fixtures/events");
    let mut names: Vec<String> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let value = fixture(&format!("events/{name}"));
            (name, value)
        })
        .collect()
}

async fn second_run(db: &DatabaseConnection) -> RunId {
    let first = run::Entity::find_by_id(rows::id(rows::RUN))
        .one(db)
        .await
        .expect("a query")
        .expect("the first run");
    let second = rows::id(0x17);
    run::ActiveModel {
        id: ActiveValue::Set(second),
        number: ActiveValue::Set(2),
        global_number: ActiveValue::NotSet,
        created_at: ActiveValue::NotSet,
        ..run::ActiveModel::from(first)
    }
    .insert(db)
    .await
    .expect("a second run");
    RunId::try_from(second).expect("a version 7 id")
}

#[tokio::test]
async fn every_event_fixture_round_trips_through_the_journal() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let written = rows::write(db, "journal").await;
    let run = run_id(rows::RUN);
    let after = u64::try_from(written.run_event.seq).expect("a positive sequence number");

    let mut appended = Vec::new();
    for (name, value) in event_fixtures() {
        if value["kind"]
            .as_str()
            .is_some_and(|kind| Change::KINDS.contains(&kind))
        {
            continue;
        }
        let fixture: RunEvent =
            serde_json::from_value(value.clone()).unwrap_or_else(|error| panic!("{name}: {error}"));
        let stored = journal::append(db, new_event(run, &fixture))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            new_event(stored.run_id, &stored),
            new_event(run, &fixture),
            "{name}"
        );

        let row = run_event::Entity::find_by_id(i64::try_from(stored.seq).expect("a sequence"))
            .one(db)
            .await
            .expect("a query")
            .expect("the stored row");
        assert_eq!(Value::String(row.kind), value["kind"], "{name}");
        assert_eq!(row.data, value["data"], "{name}");
        assert_eq!(row.actor, value["actor"], "{name}");
        appended.push(stored);
    }

    assert!(
        appended.windows(2).all(|pair| pair[0].seq < pair[1].seq),
        "sequence numbers do not increase"
    );
    assert_eq!(
        journal::read(db, run, after).await.expect("the journal"),
        appended
    );
}

#[tokio::test]
async fn reading_after_a_sequence_number_returns_the_later_events_of_that_run_only() {
    let database = TestDatabase::migrated().await;
    let db = database.connection().clone();
    let written = rows::write(&db, "reader").await;
    let first = run_id(rows::RUN);
    let second = second_run(&db).await;

    let read = tokio::spawn(async move {
        let one = journal::append(&db, attempt_created(first, 1)).await?;
        let other = journal::append(&db, attempt_created(second, 1)).await?;
        let two = journal::append(&db, attempt_created(first, 2)).await?;
        let everything = journal::read(&db, first, 0).await?;
        let later = journal::read(&db, first, one.seq).await?;
        let others = journal::read(&db, second, 0).await?;
        Ok::<_, bosun_db::Error>((one, other, two, everything, later, others))
    })
    .await
    .expect("the task")
    .expect("the journal");
    let (one, other, two, everything, later, others) = read;

    let seeded = u64::try_from(written.run_event.seq).expect("a sequence");
    assert_eq!(
        everything.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![seeded, one.seq, two.seq]
    );
    assert_eq!(later, vec![two]);
    assert_eq!(others, vec![other]);
}

#[tokio::test]
async fn an_event_rolled_back_with_its_status_change_leaves_neither() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "rollback").await;
    let run = run_id(rows::RUN);
    let test = rows::id(rows::TEST);

    let transaction = db.begin().await.expect("a transaction");
    let pending = job::Entity::find_by_id(test)
        .one(&transaction)
        .await
        .expect("a query")
        .expect("the job");
    let mut queued: job::ActiveModel = pending.clone().into();
    queued.status = ActiveValue::Set("queued".to_owned());
    queued.update(&transaction).await.expect("the status");
    journal::append(
        &transaction,
        NewEvent {
            run_id: run,
            attempt_id: None,
            job_id: Some(JobId::try_from(test).expect("a version 7 id")),
            execution_id: None,
            step_id: None,
            actor: Actor::System,
            event: Event::JobStatusChanged(JobStatusChanged {
                from: JobStatus::Pending,
                to: JobStatus::Queued,
                waiting_reason: None,
                conclusion: None,
                conclusion_source: None,
                reason: None,
            }),
        },
    )
    .await
    .expect("the event");
    transaction.rollback().await.expect("a rollback");

    let kinds: Vec<String> = run_event::Entity::find()
        .filter(run_event::Column::RunId.eq(Uuid::from(run)))
        .order_by_asc(run_event::Column::Seq)
        .all(db)
        .await
        .expect("the journal")
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(kinds, vec!["step.created".to_owned()]);
    let job = job::Entity::find_by_id(test)
        .one(db)
        .await
        .expect("a query")
        .expect("the job");
    assert_eq!(job, pending);
    assert_eq!(job.status, "pending");
}
