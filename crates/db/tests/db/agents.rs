use std::time::Duration;

use bosun_db::agents::{self, Connection, NewAgent, NewRegistrationToken, Registration, Standing};
use bosun_db::journal;
use bosun_db::notify::{Listener, Notification};
use bosun_testkit::TestDatabase;
use bosun_types::api::Capability;
use bosun_types::id::{AgentId, ExecutionId, RunId};
use sea_orm::{ConnectionTrait, TransactionTrait};
use tokio::time::{Instant, timeout, timeout_at};

use crate::rows::{self, AGENT, EXECUTION, REPLICA, RUN};
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

async fn drains_announced<C: ConnectionTrait>(db: &C) -> Vec<String> {
    strings(
        db,
        &format!(
            "SELECT data->>'drain' AS value FROM bosun.stream_event \
             WHERE subject = '{}' AND kind = 'agent.status' ORDER BY seq",
            rows::id(AGENT)
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

async fn announced(listener: &mut Listener) -> Notification {
    loop {
        let heard = timeout(MINUTE, listener.next())
            .await
            .expect("a notice in time")
            .expect("a notice")
            .expect("a notice without a reconnect");
        if !matches!(heard, Notification::Events(_)) {
            return heard;
        }
    }
}

async fn silent(listener: &mut Listener, what: &str) {
    let deadline = Instant::now() + Duration::from_millis(300);
    while let Ok(heard) = timeout_at(deadline, listener.next()).await {
        match heard {
            Ok(Some(Notification::Events(_))) => {}
            other => panic!("{what} announced {other:?}"),
        }
    }
}

fn connection() -> Connection {
    Connection {
        replica_id: rows::id(REPLICA),
        version: "0.1.0".to_owned(),
        labels: vec!["self-hosted".to_owned()],
        capabilities: vec![Capability::Shell],
    }
}

#[tokio::test]
async fn a_drain_and_its_lifting_are_announced_to_the_replica_of_the_agent_once_each() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let mut listener = Listener::connect(&database.url(), rows::id(REPLICA))
        .await
        .expect("a listener");
    let standing = |drain| {
        Some(Standing {
            drain,
            deleted: false,
        })
    };
    assert_eq!(
        agents::standing(db, agent_id()).await.expect("a read"),
        standing(false)
    );

    let drained = agents::drain(db, agent_id(), true)
        .await
        .expect("a drain")
        .expect("the agent");
    assert!(drained.drain);
    assert_eq!(drained.execution_id, Some(execution_id()));
    assert_eq!(
        announced(&mut listener).await,
        Notification::Agent(agent_id())
    );
    assert_eq!(
        agents::standing(db, agent_id()).await.expect("a read"),
        standing(true)
    );
    let again = agents::drain(db, agent_id(), true)
        .await
        .expect("a drain")
        .expect("the agent");
    assert_eq!(again, drained);
    silent(&mut listener, "a second drain").await;
    assert_eq!(drains_announced(db).await, ["false", "true"]);
    assert_eq!(
        agents::connect(db, agent_id(), connection())
            .await
            .expect("a connection"),
        Some(true)
    );
    assert_eq!(drains_announced(db).await, ["false", "true", "true"]);

    let lifted = agents::drain(db, agent_id(), false)
        .await
        .expect("a lifting")
        .expect("the agent");
    assert!(!lifted.drain);
    assert_eq!(
        announced(&mut listener).await,
        Notification::Agent(agent_id())
    );
    assert_eq!(
        agents::connect(db, agent_id(), connection())
            .await
            .expect("a connection"),
        Some(false)
    );
    assert_eq!(
        drains_announced(db).await,
        ["false", "true", "true", "false", "false"]
    );

    execute(db, "UPDATE bosun.agent SET replica_id = NULL").await;
    agents::drain(db, agent_id(), true)
        .await
        .expect("a drain")
        .expect("the agent");
    silent(&mut listener, "the drain of an agent without a stream").await;
    assert_eq!(
        drains_announced(db).await,
        ["false", "true", "true", "false", "false", "true"]
    );
}

#[tokio::test]
async fn a_deleted_agent_keeps_its_row_and_is_refused_everywhere() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let mut listener = Listener::connect(&database.url(), rows::id(REPLICA))
        .await
        .expect("a listener");
    let credential = [0x33; 32];
    assert!(
        agents::authenticate(db, &credential)
            .await
            .expect("a lookup")
            .is_some()
    );
    assert!(!agents::revoked(db, &credential).await.expect("a lookup"));

    assert_eq!(drains_announced(db).await, ["false"]);
    assert!(agents::delete(db, agent_id()).await.expect("a deletion"));
    assert!(
        !agents::delete(db, agent_id()).await.expect("a deletion"),
        "an agent was deleted twice"
    );
    assert_eq!(
        drains_announced(db).await,
        ["false", "false"],
        "the deletion was not announced once"
    );
    assert_eq!(
        agents::standing(db, agent_id()).await.expect("a read"),
        Some(Standing {
            drain: false,
            deleted: true
        })
    );
    assert_eq!(
        agents::authenticate(db, &credential)
            .await
            .expect("a lookup"),
        None
    );
    assert!(agents::revoked(db, &credential).await.expect("a lookup"));
    assert!(!agents::revoked(db, &[0x99; 32]).await.expect("a lookup"));
    assert_eq!(agents::find(db, agent_id()).await.expect("a read"), None);
    assert_eq!(agents::list(db, None, 10).await.expect("the agents"), []);
    assert_eq!(
        agents::drain(db, agent_id(), true).await.expect("a drain"),
        None
    );
    assert!(
        !agents::heartbeat(db, agent_id())
            .await
            .expect("a heartbeat")
    );
    assert_eq!(
        agents::connect(db, agent_id(), connection())
            .await
            .expect("a connection"),
        None
    );
    assert_eq!(
        strings(
            db,
            "SELECT concat_ws(' ', name, online, replica_id IS NOT NULL) AS value FROM bosun.agent"
        )
        .await,
        ["runner1-1 alone f t"]
    );
    assert_eq!(
        agents::held(db, agent_id()).await.expect("the executions"),
        [execution_id()]
    );

    agents::announce(db, agent_id())
        .await
        .expect("an announcement");
    assert_eq!(
        announced(&mut listener).await,
        Notification::Agent(agent_id())
    );
    last_seen(db, false, "11 minutes").await;
    let sweep = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert!(
        sweep.removed.is_empty(),
        "the sweep took on a deletion that its replica may still be finishing"
    );
    execute(
        db,
        "UPDATE bosun.agent SET deleted_at = now() - interval '2 minutes'",
    )
    .await;
    let sweep = agents::sweep(db, MINUTE, 10 * MINUTE)
        .await
        .expect("a sweep")
        .expect("the sweep lock");
    assert_eq!(sweep.removed, [(agent_id(), execution_id())]);
    assert!(
        sweep.abandoned.is_empty(),
        "the execution of a deleted agent was found lost too"
    );
    assert_eq!(statuses(db).await, ["in_progress f in_progress f"]);
}
