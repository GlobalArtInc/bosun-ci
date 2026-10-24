use std::time::Duration;

use bosun_db::runs::{self, JobState};
use bosun_db::{Error, journal};
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::event::{AttemptCreated, Event, RunEvent};
use bosun_types::id::{AgentId, ExecutionId, JobId, PoolId, RunId};
use bosun_types::status::{Conclusion, ExecutionStatus, JobStatus, StatusReason};
use sea_orm::{DatabaseConnection, TransactionTrait};

use crate::rows::{self, AGENT, BUILD, EXECUTION, RUN, TEST};
use crate::strings;

fn run_id() -> RunId {
    rows::id(RUN).try_into().expect("a run id")
}

fn job(number: u8) -> JobId {
    rows::id(number).try_into().expect("a job id")
}

fn marker(number: u32) -> Event {
    Event::AttemptCreated(AttemptCreated { number })
}

async fn append_under_lock(db: DatabaseConnection, number: u32) -> RunEvent {
    let transaction = db.begin().await.expect("a transaction");
    let run = runs::lock(&transaction, run_id())
        .await
        .expect("a lock")
        .expect("the run");
    let appended = run
        .record(None, &Actor::System, marker(number))
        .await
        .expect("an event");
    transaction.commit().await.expect("a commit");
    appended
}

#[tokio::test]
async fn an_append_waits_for_the_lock_of_its_run_so_sequence_numbers_follow_commits() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let before = journal::read(db, run_id(), 0).await.expect("the journal");
    let seen = before.last().map_or(0, |event| event.seq);

    let first = db.begin().await.expect("a transaction");
    let holder = runs::lock(&first, run_id())
        .await
        .expect("a lock")
        .expect("the run");
    let waiting = tokio::spawn(append_under_lock(db.clone(), 2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !waiting.is_finished(),
        "an append to a locked run did not wait"
    );
    let earlier = holder
        .record(None, &Actor::System, marker(1))
        .await
        .expect("an event");
    first.commit().await.expect("a commit");
    let later = waiting.await.expect("the waiting append");

    assert!(
        earlier.seq < later.seq,
        "{} committed first but was numbered after {}",
        earlier.seq,
        later.seq
    );
    let resumed = journal::read(db, run_id(), seen)
        .await
        .expect("the journal");
    assert_eq!(resumed, [earlier.clone(), later.clone()]);
    assert_eq!(
        journal::read(db, run_id(), earlier.seq)
            .await
            .expect("the journal"),
        [later]
    );
}

#[tokio::test]
async fn the_transitions_write_their_columns_and_events_and_refuse_what_the_tables_do_not_allow() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let written = rows::write(db, "alone").await;
    let seen = u64::try_from(written.run_event.seq).expect("a sequence number");
    let agent: AgentId = rows::id(AGENT).try_into().expect("an agent id");
    let pool: PoolId = written.agent.pool_id.try_into().expect("a pool id");
    let running: ExecutionId = rows::id(EXECUTION).try_into().expect("an execution id");

    let transaction = db.begin().await.expect("a transaction");
    let run = runs::lock(&transaction, run_id())
        .await
        .expect("a lock")
        .expect("the run");
    assert_eq!(run.state().attempt, 1);
    assert!(!run.state().cancel_requested);
    let jobs: Vec<JobState> = run.jobs().await.expect("the jobs");
    assert_eq!(
        jobs.iter()
            .map(|job| (
                job.key.as_str(),
                job.status,
                job.execution.as_ref().map(|execution| execution.status)
            ))
            .collect::<Vec<_>>(),
        [
            (
                "build",
                JobStatus::InProgress,
                Some(ExecutionStatus::InProgress)
            ),
            ("test", JobStatus::Pending, None),
        ]
    );

    match run
        .complete_job(job(BUILD), Conclusion::Cancelled, None, &Actor::System)
        .await
    {
        Err(Error::Transition {
            entity, from, to, ..
        }) => {
            assert_eq!(
                (entity, from.as_str(), to.as_str()),
                ("job", "in_progress", "completed")
            );
        }
        other => panic!("a job on an agent was completed without its execution: {other:?}"),
    }
    match run.assign_job(job(TEST), agent).await {
        Err(Error::Transition { from, to, .. }) => {
            assert_eq!((from.as_str(), to.as_str()), ("pending", "assigned"));
        }
        other => panic!("a pending job was assigned: {other:?}"),
    }

    run.queue_job(
        job(TEST),
        &[
            "Self-Hosted".to_owned(),
            "self-hosted".to_owned(),
            "X64".to_owned(),
        ],
        &[pool],
    )
    .await
    .expect("the test job queued");
    let execution = run
        .assign_job(job(TEST), agent)
        .await
        .expect("the test job assigned");
    match run.start_step(execution, 0, None, &Actor::System).await {
        Err(Error::NotInProgress { status, .. }) => assert_eq!(status, "assigned"),
        other => panic!("a step of an assigned execution started: {other:?}"),
    }
    run.start_execution(execution, &Actor::System)
        .await
        .expect("the execution started");
    match run.start_step(execution, 5, None, &Actor::System).await {
        Err(Error::NoStep { index, .. }) => assert_eq!(index, 5),
        other => panic!("a step that does not exist started: {other:?}"),
    }
    run.start_step(execution, 0, None, &Actor::System)
        .await
        .expect("the step started");
    run.finish_step(
        execution,
        0,
        None,
        Conclusion::Failure,
        Conclusion::Failure,
        &Actor::System,
    )
    .await
    .expect("the step finished");
    run.finish_execution(execution, Conclusion::Failure, None, &Actor::System)
        .await
        .expect("the execution finished");
    run.finish_execution(
        running,
        Conclusion::Failure,
        Some(StatusReason::AgentLost),
        &Actor::System,
    )
    .await
    .expect("the running execution was lost");
    match run
        .finish_execution(execution, Conclusion::Success, None, &Actor::System)
        .await
    {
        Err(Error::Transition { from, .. }) => assert_eq!(from, "completed"),
        other => panic!("a completed execution finished again: {other:?}"),
    }
    transaction.commit().await.expect("a commit");

    assert_eq!(
        strings(
            db,
            &format!(
                "SELECT concat_ws(' ', key, status, conclusion, conclusion_source, \
                 array_to_string(labels, ','), queued_at IS NOT NULL, started_at IS NOT NULL, \
                 completed_at IS NOT NULL) AS value FROM bosun.job WHERE id = '{}'",
                rows::id(TEST)
            ),
        )
        .await,
        ["test completed failure natural self-hosted,x64 t t t"]
    );
    assert_eq!(
        strings(
            db,
            &format!(
                "SELECT concat_ws(' ', s.index, s.name, s.status, s.outcome, s.conclusion) \
                 AS value FROM bosun.step s JOIN bosun.execution e ON e.id = s.execution_id \
                 WHERE e.job_id = '{}'",
                rows::id(TEST)
            ),
        )
        .await,
        ["0 Run perl -e 'exit 0' completed failure failure"]
    );
    assert_eq!(
        strings(
            db,
            &format!(
                "SELECT concat_ws(' ', s.status, s.outcome, s.conclusion) AS value \
                 FROM bosun.step s WHERE s.execution_id = '{}'",
                rows::id(EXECUTION)
            ),
        )
        .await,
        ["completed success success"]
    );
    let kinds: Vec<(String, Option<JobId>)> = journal::read(db, run_id(), seen)
        .await
        .expect("the journal")
        .into_iter()
        .map(|event| (event.event.kind().to_owned(), event.job_id))
        .collect();
    let test = Some(job(TEST));
    let build = Some(job(BUILD));
    assert_eq!(
        kinds,
        [
            ("job.status".to_owned(), test),
            ("execution.created".to_owned(), test),
            ("job.status".to_owned(), test),
            ("execution.status".to_owned(), test),
            ("job.status".to_owned(), test),
            ("step.created".to_owned(), test),
            ("step.status".to_owned(), test),
            ("step.status".to_owned(), test),
            ("execution.status".to_owned(), test),
            ("job.status".to_owned(), test),
            ("execution.status".to_owned(), build),
            ("job.status".to_owned(), build),
        ]
    );
}
