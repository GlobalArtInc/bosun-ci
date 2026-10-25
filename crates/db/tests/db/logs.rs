use std::time::Duration;

use bosun_db::logs::{
    self, Chunk, Compacted, Compaction, Compactor, Ingested, Log, NewObject, Notice, StepLog,
    StoredChunk, StoredObject,
};
use bosun_db::notify::{Listener, Notification};
use bosun_testkit::TestDatabase;
use bosun_types::id::{AgentId, ExecutionId, StepId};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use tokio::time::timeout;

use crate::rows::{self, AGENT, ATTEMPT, EXECUTION, RUN, STEP};
use crate::strings;

const WAIT: Duration = Duration::from_secs(30);

const COMPACTION_LOCK: i64 = i64::from_be_bytes(*b"bosunlog");

async fn execute<C: ConnectionTrait>(db: &C, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn agent() -> AgentId {
    rows::id(AGENT).try_into().expect("an agent id")
}

fn execution() -> ExecutionId {
    rows::id(EXECUTION).try_into().expect("an execution id")
}

fn step() -> StepId {
    rows::id(STEP).try_into().expect("a step id")
}

async fn prepared(db: &DatabaseConnection) {
    rows::write(db, "alone").await;
    execute(
        db,
        "DELETE FROM bosun.log_object; DELETE FROM bosun.log_chunk; \
         UPDATE bosun.execution SET log_bytes = 0",
    )
    .await;
}

async fn complete(db: &DatabaseConnection, ago: &str) {
    execute(
        db,
        &format!(
            "UPDATE bosun.execution SET status = 'completed', conclusion = 'success', \
             completed_at = now() - interval '{ago}' WHERE id = '{}'",
            rows::id(EXECUTION)
        ),
    )
    .await;
}

fn chunk(seq: u64, lines: &[u8]) -> Chunk<'_> {
    Chunk {
        agent: agent(),
        execution: execution(),
        step: 0,
        seq,
        lines,
    }
}

async fn ingest(db: &DatabaseConnection, seq: u64, lines: &[u8], limit: u64) -> Ingested {
    logs::ingest(db, &chunk(seq, lines), limit)
        .await
        .expect("an ingestion")
}

async fn live(db: &DatabaseConnection, after: u64, page: u64) -> Vec<StoredChunk> {
    let window = logs::window(db, execution(), step(), after, page)
        .await
        .expect("a window")
        .expect("the step");
    match window.log {
        Log::Live(chunks) => chunks,
        other => panic!("the log is not live: {other:?}"),
    }
}

async fn log_bytes(db: &DatabaseConnection) -> Vec<String> {
    strings(
        db,
        "SELECT log_bytes::text AS value FROM bosun.execution ORDER BY id",
    )
    .await
}

async fn compaction(db: &DatabaseConnection) -> Result<Compactor, Compacted> {
    match logs::compaction(db, execution())
        .await
        .expect("a compaction")
    {
        Compaction::Started(compactor) => Ok(compactor),
        Compaction::Skipped(skipped) => Err(skipped),
    }
}

async fn started(db: &DatabaseConnection) -> Compactor {
    match compaction(db).await {
        Ok(compactor) => compactor,
        Err(skipped) => panic!("the compaction was skipped: {skipped:?}"),
    }
}

async fn skipped(db: &DatabaseConnection) -> Compacted {
    match compaction(db).await {
        Ok(_) => panic!("a compaction started"),
        Err(skipped) => skipped,
    }
}

fn stored(seq: u64, data: &[u8], dropped: u64) -> StoredChunk {
    StoredChunk {
        seq,
        step_id: step(),
        data: data.to_vec(),
        dropped,
    }
}

#[tokio::test]
async fn a_chunk_is_stored_once_announced_on_bosun_log_and_read_back_in_order() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    prepared(db).await;
    let mut listener = Listener::connect(&database.url(), Uuid::now_v7())
        .await
        .expect("a listener");

    assert_eq!(ingest(db, 1, b"a\n", 1024).await, Ingested::Stored);
    assert_eq!(ingest(db, 1, b"a\n", 1024).await, Ingested::Duplicate);
    assert_eq!(ingest(db, 2, b"b\nc\n", 1024).await, Ingested::Stored);
    for seq in [1, 2] {
        let heard = timeout(WAIT, listener.next())
            .await
            .expect("a notification in time")
            .expect("a notification")
            .expect("a notification without a reconnect");
        assert_eq!(
            heard,
            Notification::Log(Notice {
                execution_id: execution(),
                seq,
            })
        );
    }
    assert!(
        timeout(Duration::from_millis(300), listener.next())
            .await
            .is_err(),
        "a duplicate chunk was announced"
    );
    assert_eq!(log_bytes(db).await, ["6"]);

    assert_eq!(
        live(db, 0, 10).await,
        [stored(1, b"a\n", 0), stored(2, b"b\nc\n", 0)]
    );
    assert_eq!(live(db, 1, 10).await, [stored(2, b"b\nc\n", 0)]);
    assert_eq!(live(db, 0, 1).await, [stored(1, b"a\n", 0)]);
    let window = logs::window(db, execution(), step(), 0, 10)
        .await
        .expect("a window")
        .expect("the step");
    assert_eq!(
        window.completed_at,
        Some(rows::at("2026-10-24T18:35:07.512Z"))
    );
    let elsewhere: StepId = Uuid::now_v7().try_into().expect("a step id");
    assert_eq!(
        logs::window(db, execution(), elsewhere, 0, 10)
            .await
            .expect("a window"),
        None
    );
}

#[tokio::test]
async fn chunks_of_another_agent_an_unknown_execution_or_an_unknown_step_are_not_stored() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    prepared(db).await;
    let other: AgentId = Uuid::now_v7().try_into().expect("an agent id");
    let unknown: ExecutionId = Uuid::now_v7().try_into().expect("an execution id");
    for (chunk, expected) in [
        (
            Chunk {
                agent: other,
                ..chunk(1, b"a\n")
            },
            Ingested::OtherAgent,
        ),
        (
            Chunk {
                execution: unknown,
                ..chunk(1, b"a\n")
            },
            Ingested::NoExecution,
        ),
        (
            Chunk {
                step: 7,
                ..chunk(1, b"a\n")
            },
            Ingested::NoStep,
        ),
    ] {
        assert_eq!(
            logs::ingest(db, &chunk, 1024).await.expect("an ingestion"),
            expected
        );
    }
    assert_eq!(live(db, 0, 10).await, []);
    assert_eq!(log_bytes(db).await, ["0"]);
}

#[tokio::test]
async fn a_chunk_that_would_pass_the_limit_is_dropped_and_so_is_every_chunk_after_it() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    prepared(db).await;
    assert_eq!(ingest(db, 1, b"12345\n", 10).await, Ingested::Stored);
    assert_eq!(
        ingest(db, 2, b"67890\n", 10).await,
        Ingested::Dropped { first: true }
    );
    assert_eq!(
        ingest(db, 3, b"\n", 10).await,
        Ingested::Dropped { first: false }
    );
    assert_eq!(ingest(db, 2, b"67890\n", 10).await, Ingested::Duplicate);
    assert_eq!(
        ingest(db, 4, b"", 10).await,
        Ingested::Dropped { first: false }
    );
    assert_eq!(
        live(db, 0, 10).await,
        [
            stored(1, b"12345\n", 0),
            stored(2, b"", 6),
            stored(3, b"", 1),
            stored(4, b"", 0)
        ]
    );
    assert_eq!(log_bytes(db).await, ["13"]);
}

#[tokio::test]
async fn the_first_chunk_dropped_after_a_log_that_fills_its_limit_exactly_is_the_first() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    prepared(db).await;
    assert_eq!(ingest(db, 1, b"123456789\n", 10).await, Ingested::Stored);
    assert_eq!(ingest(db, 2, b"", 10).await, Ingested::Stored);
    assert_eq!(
        ingest(db, 3, b"a\n", 10).await,
        Ingested::Dropped { first: true }
    );
    assert_eq!(
        ingest(db, 4, b"b\n", 10).await,
        Ingested::Dropped { first: false }
    );
    assert_eq!(
        live(db, 0, 10).await,
        [
            stored(1, b"123456789\n", 0),
            stored(2, b"", 0),
            stored(3, b"", 2),
            stored(4, b"", 2)
        ]
    );
    assert_eq!(log_bytes(db).await, ["14"]);
}

#[tokio::test]
async fn a_compaction_replaces_the_chunks_with_objects_once_and_closes_the_log() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    prepared(db).await;
    assert_eq!(ingest(db, 1, b"a\n", 1024).await, Ingested::Stored);
    assert_eq!(ingest(db, 2, b"b\n", 1024).await, Ingested::Stored);
    assert_eq!(
        skipped(db).await,
        Compacted::Already,
        "an execution in progress was taken for compaction"
    );
    complete(db, "0 seconds").await;
    let mut compactor = started(db).await;
    assert_eq!(
        (
            compactor.run_id,
            compactor.attempt_id,
            compactor.steps.clone()
        ),
        (
            rows::id(RUN).try_into().expect("a run id"),
            rows::id(ATTEMPT).try_into().expect("an attempt id"),
            vec![StepLog {
                id: step(),
                completed_at: Some(rows::at("2026-10-24T18:35:07.512Z")),
            }],
        )
    );
    assert_eq!(
        skipped(db).await,
        Compacted::Busy,
        "a second compaction started while the first held the execution"
    );
    assert_eq!(
        compactor.chunks(1).await.expect("the chunks"),
        [stored(1, b"a\n", 0)]
    );
    assert_eq!(ingest(db, 3, b"c\n", 1024).await, Ingested::Stored);
    assert_eq!(
        compactor.chunks(10).await.expect("the chunks"),
        [stored(2, b"b\n", 0)],
        "a compactor saw a chunk that arrived after it began to read"
    );

    let object = NewObject {
        step_id: step(),
        key: format!(
            "logs/{}/{}/{}/3/{}.log.zst",
            rows::id(RUN),
            rows::id(ATTEMPT),
            execution(),
            step()
        ),
        size: 30,
        lines: 3,
        frames: vec![0],
    };
    assert_eq!(
        compactor
            .finish(std::slice::from_ref(&object))
            .await
            .expect("a refused compaction"),
        Compacted::Changed
    );
    assert_eq!(
        live(db, 0, 10).await.len(),
        3,
        "a refused compaction changed the log"
    );

    let mut compactor = started(db).await;
    assert_eq!(
        compactor.chunks(10).await.expect("the chunks"),
        [
            stored(1, b"a\n", 0),
            stored(2, b"b\n", 0),
            stored(3, b"c\n", 0)
        ]
    );
    assert_eq!(compactor.chunks(10).await.expect("the chunks"), []);
    assert_eq!(
        compactor
            .finish(std::slice::from_ref(&object))
            .await
            .expect("a compaction"),
        Compacted::Done
    );
    assert_eq!(skipped(db).await, Compacted::Already);
    let window = logs::window(db, execution(), step(), 0, 10)
        .await
        .expect("a window")
        .expect("the step");
    assert_eq!(
        window.log,
        Log::Compacted(Some(StoredObject {
            key: object.key.clone(),
            size: 30,
            lines: 3,
            frames: vec![0],
        }))
    );
    assert_eq!(ingest(db, 4, b"d\n", 1024).await, Ingested::Closed);
    assert_eq!(
        strings(
            db,
            &format!(
                "SELECT count(*)::text AS value FROM bosun.log_chunk WHERE execution_id = '{}'",
                execution()
            )
        )
        .await,
        ["0"]
    );
}

#[tokio::test]
async fn the_compaction_sweep_takes_settled_completed_executions_under_its_own_lock() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    prepared(db).await;
    let settled = Duration::from_secs(60);
    assert_eq!(
        logs::compactable(db, Duration::ZERO, 10)
            .await
            .expect("a sweep"),
        Some(Vec::new()),
        "an execution in progress was taken"
    );
    complete(db, "1 second").await;
    assert_eq!(
        logs::compactable(db, settled, 10).await.expect("a sweep"),
        Some(Vec::new()),
        "an execution that completed a second ago was taken"
    );
    complete(db, "2 minutes").await;
    assert_eq!(
        logs::compactable(db, settled, 10).await.expect("a sweep"),
        Some(vec![execution()])
    );

    let holder = db.begin().await.expect("a transaction");
    execute(
        &holder,
        &format!("SELECT pg_advisory_xact_lock({COMPACTION_LOCK})"),
    )
    .await;
    assert_eq!(
        logs::compactable(db, settled, 10).await.expect("a sweep"),
        None,
        "a sweep ran while another held its lock"
    );
    holder.rollback().await.expect("the lock is released");

    execute(
        db,
        &format!(
            "UPDATE bosun.execution SET log_compacted_at = now() WHERE id = '{}'",
            rows::id(EXECUTION)
        ),
    )
    .await;
    assert_eq!(
        logs::compactable(db, settled, 10).await.expect("a sweep"),
        Some(Vec::new())
    );
}
