use std::time::Duration;

use bosun_db::Error;
use bosun_db::control::{self, Kind, Notice};
use bosun_db::notify::{Listener, Notification};
use bosun_testkit::TestDatabase;
use bosun_types::id::{AgentId, ExecutionId};
use sea_orm::ConnectionTrait;
use sea_orm::prelude::Uuid;
use tokio::time::timeout;

use crate::rows::{self, AGENT, EXECUTION, REPLICA};

const WAIT: Duration = Duration::from_secs(30);

fn agent_id() -> AgentId {
    rows::id(AGENT).try_into().expect("an agent id")
}

fn execution_id() -> ExecutionId {
    rows::id(EXECUTION).try_into().expect("an execution id")
}

#[test]
fn the_channel_of_a_replica_is_named_after_its_id() {
    assert_eq!(
        control::channel(rows::id(REPLICA)),
        "bosun_replica_0192f3a45b6c7d8e9f01000000000011"
    );
}

#[tokio::test]
async fn a_control_message_is_announced_on_the_channel_of_the_replica_holding_the_agent() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    let mut listener = Listener::connect(&database.url(), rows::id(REPLICA))
        .await
        .expect("a listener");
    let mut elsewhere = Listener::connect(&database.url(), Uuid::now_v7())
        .await
        .expect("a listener of another replica");

    let published = control::publish(db, execution_id(), Kind::Cancel)
        .await
        .expect("a control message");
    assert_eq!(published.agent_id, agent_id());
    assert_eq!(published.execution_id, execution_id());
    assert_eq!(published.kind, Kind::Cancel);
    let notice = timeout(WAIT, listener.next())
        .await
        .expect("a notice in time")
        .expect("a notice")
        .expect("a notice without a reconnect");
    assert_eq!(
        notice,
        Notification::Control(Notice {
            agent_id: agent_id(),
            control_id: published.id,
        })
    );
    assert!(
        timeout(Duration::from_millis(300), elsewhere.next())
            .await
            .is_err(),
        "another replica heard the notice"
    );

    assert_eq!(
        control::find_pending(db, published.id)
            .await
            .expect("a lookup"),
        Some(published.clone())
    );
    assert_eq!(
        control::pending(db, agent_id()).await.expect("the pending"),
        std::slice::from_ref(&published)
    );
    let other: AgentId = Uuid::now_v7().try_into().expect("an agent id");
    assert!(
        !control::acknowledge(db, other, published.id, true)
            .await
            .expect("an acknowledgement")
    );
    assert!(
        control::acknowledge(db, agent_id(), published.id, true)
            .await
            .expect("an acknowledgement")
    );
    assert!(
        !control::acknowledge(db, agent_id(), published.id, false)
            .await
            .expect("an acknowledgement")
    );
    assert_eq!(
        control::pending(db, agent_id()).await.expect("the pending"),
        []
    );
    assert_eq!(
        control::find_pending(db, published.id)
            .await
            .expect("a lookup"),
        None
    );
}

#[tokio::test]
async fn a_control_message_for_an_agent_without_a_stream_waits_for_it() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    rows::write(db, "alone").await;
    db.execute_unprepared("UPDATE bosun.agent SET replica_id = NULL")
        .await
        .expect("no replica");
    let mut listener = Listener::connect(&database.url(), rows::id(REPLICA))
        .await
        .expect("a listener");

    let published = control::publish(db, execution_id(), Kind::Cancel)
        .await
        .expect("a control message");
    assert!(
        timeout(Duration::from_millis(300), listener.next())
            .await
            .is_err(),
        "a notice went out for an agent without a replica"
    );
    assert_eq!(
        control::pending(db, agent_id()).await.expect("the pending"),
        [published]
    );
}

#[tokio::test]
async fn a_control_message_for_an_execution_that_does_not_exist_is_refused() {
    let database = TestDatabase::migrated().await;
    let missing: ExecutionId = Uuid::now_v7().try_into().expect("an execution id");
    match control::publish(database.connection(), missing, Kind::Cancel).await {
        Err(Error::NoExecution(execution)) => assert_eq!(execution, missing),
        other => panic!("publish returned {other:?}"),
    }
}
