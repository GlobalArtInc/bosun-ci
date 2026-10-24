use std::time::Duration;

use bosun_db::agents::{self, NewAgent, NewRegistrationToken, Registration};
use bosun_db::journal;
use bosun_testkit::TestDatabase;
use bosun_types::id::{AgentId, ExecutionId, RunId};
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::rows::{self, AGENT, EXECUTION, RUN};
use crate::strings;

const MINUTE: Duration = Duration::from_secs(60);

const SWEEP_LOCK: i64 = i64::from_be_bytes(*b"bosunagt");

async fn execute<C: ConnectionTrait>(db: &C, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn agent_id() -> AgentId {
    rows::id(AGENT).try_into().expect("an agent id")
}

fn execution_id() -> ExecutionId {
    rows::id(EXECUTION).try_into().expect("an execution id")
}

async fn last_seen<C: ConnectionTrait>(db: &C, online: bool, ago: &str) {
    execute(
        db,
        &format!(
            "UPDATE bosun.agent SET online = {online}, last_seen_at = now() - interval '{ago}' \
             WHERE id = '{}'",
            rows::id(AGENT)
        ),
    )
    .await;
}

async fn statuses<C: ConnectionTrait>(db: &C) -> Vec<String> {
    strings(
        db,
        &format!(
            "SELECT concat_ws(' ', e.status, e.conclusion, e.reason, e.completed_at IS NOT NULL, \
             j.status, j.conclusion, j.conclusion_source, j.completed_at IS NOT NULL) AS value \
             FROM bosun.execution e JOIN bosun.job j ON j.id = e.job_id WHERE e.id = '{}'",
            rows::id(EXECUTION)
        ),
    )
    .await
}

#[tokio::test]
async fn a_registration_token_registers_once_unless_it_is_reusable_and_never_after_it_expired() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let pool = bosun_db::pools::default_pool(db).await.expect("a pool");
    let now = rows::at("2026-10-24T18:00:00Z");
    for (byte, reusable) in [(1, false), (2, true)] {
        agents::create_registration_token(
            db,
            NewRegistrationToken {
                pool_id: pool.id,
                sha256: vec![byte; 32],
                reusable,
                expires_at: rows::at("2026-10-25T18:00:00Z"),
            },
        )
        .await
        .expect("a registration token");
    }
    let register = |token: u8, credential: u8, now| async move {
        let agent = NewAgent {
            name: format!("runner{credential}"),
            labels: vec!["rust".to_owned()],
            credential_sha256: vec![credential; 32],
        };
        agents::register(db, &[token; 32], now, agent)
            .await
            .expect("a registration")
    };

    let Registration::Registered(first) = register(1, 11, now).await else {
        panic!("a single-use token did not register an agent");
    };
    assert_eq!(first.name, "runner11");
    assert_eq!(first.pool_id, pool.id);
    assert_eq!(first.labels, ["rust"]);
    assert!(!first.online);
    assert!(matches!(
        register(1, 12, now).await,
        Registration::UsedToken
    ));
    for credential in [13, 14] {
        assert!(matches!(
            register(2, credential, now).await,
            Registration::Registered(_)
        ));
    }
    assert!(matches!(
        register(2, 15, rows::at("2026-10-25T18:00:00Z")).await,
        Registration::ExpiredToken
    ));
    assert!(matches!(
        register(3, 16, now).await,
        Registration::UnknownToken
    ));

    let found = agents::authenticate(db, &[11; 32])
        .await
        .expect("a lookup")
        .expect("the agent");
    assert_eq!(found, first);
    assert_eq!(
        agents::authenticate(db, &[12; 32]).await.expect("a lookup"),
        None
    );
}

#[tokio::test]
async fn the_sweep_marks_an_agent_offline_without_a_recent_heartbeat() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;

    last_seen(db, true, "10 seconds").await;
    let sweep = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert!(sweep.offline.is_empty());

    last_seen(db, true, "2 minutes").await;
    let sweep = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert_eq!(sweep.offline, [(agent_id(), "runner1-1 alone".to_owned())]);
    assert!(
        sweep.abandoned.is_empty(),
        "abandoned {:?} within the grace",
        sweep.abandoned
    );
    let listed = agents::list(db, None, 10).await.expect("the agents");
    assert!(!listed[0].online);
    assert_eq!(listed[0].execution_id, Some(execution_id()));

    agents::heartbeat(db, agent_id())
        .await
        .expect("a heartbeat");
    assert!(agents::list(db, None, 10).await.expect("the agents")[0].online);
}

#[tokio::test]
async fn the_sweep_finds_the_executions_of_an_agent_that_did_not_come_back_within_the_grace() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let run_id: RunId = rows::id(RUN).try_into().expect("a run id");
    let before = journal::read(db, run_id, 0).await.expect("the journal");

    last_seen(db, false, "9 minutes").await;
    let sweep = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert!(sweep.abandoned.is_empty());

    last_seen(db, false, "11 minutes").await;
    let sweep = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert_eq!(sweep.abandoned, [execution_id()]);
    assert_eq!(
        statuses(db).await,
        ["in_progress f in_progress f"],
        "the sweep changed a run, which only the planner of the server does"
    );
    assert_eq!(
        journal::read(db, run_id, 0).await.expect("the journal"),
        before
    );

    execute(
        db,
        &format!(
            "UPDATE bosun.execution SET status = 'completed' WHERE id = '{}'",
            rows::id(EXECUTION)
        ),
    )
    .await;
    let again = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert!(again.abandoned.is_empty());
}

#[tokio::test]
async fn an_agent_that_comes_back_abandons_the_executions_it_does_not_list() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;

    let abandoned = agents::abandoned(db, agent_id(), &[execution_id()])
        .await
        .expect("the abandoned executions");
    assert!(abandoned.is_empty());

    let abandoned = agents::abandoned(db, agent_id(), &[])
        .await
        .expect("the abandoned executions");
    assert_eq!(abandoned, [execution_id()]);
    assert_eq!(statuses(db).await, ["in_progress f in_progress f"]);
}

#[tokio::test]
async fn one_sweep_runs_at_a_time() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let holder = db.begin().await.expect("a transaction");
    execute(
        &holder,
        &format!("SELECT pg_advisory_xact_lock({SWEEP_LOCK})"),
    )
    .await;
    assert!(
        agents::sweep(db, MINUTE, 10 * MINUTE)
            .await
            .expect("a sweep")
            .is_none(),
        "a second sweep ran"
    );
    holder.rollback().await.expect("the lock is released");
    assert!(
        agents::sweep(db, MINUTE, 10 * MINUTE)
            .await
            .expect("a sweep")
            .is_some()
    );
}
