use std::sync::Arc;
use std::time::Duration;

use bosun_blob::{Key, Store};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::DatabaseConnection;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::clock::Clock;
use crate::site::Retention;
use crate::store::Backend;

const ROUND: Duration = Duration::from_secs(60);

const PAYLOADS: TimeDelta = TimeDelta::days(30);

const AGENT_EVENTS: TimeDelta = TimeDelta::days(7);

const BATCH: u64 = 100;

const ROWS: u64 = 1000;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error(transparent)]
    Database(#[from] bosun_db::Error),
    #[error(transparent)]
    Store(#[from] bosun_blob::Error),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Removed {
    pub(crate) logs: u64,
    pub(crate) runs: u64,
    pub(crate) payloads: u64,
    pub(crate) agent_events: u64,
    pub(crate) objects: u64,
}

pub(crate) async fn sweep(
    db: DatabaseConnection,
    store: Arc<Backend>,
    clock: Clock,
    retention: Retention,
) {
    let mut ticks = interval(ROUND);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        match round(&db, store.as_ref(), clock.now(), retention).await {
            Ok(Some(removed)) if removed != Removed::default() => info!(
                logs = removed.logs,
                runs = removed.runs,
                payloads = removed.payloads,
                agent_events = removed.agent_events,
                objects = removed.objects,
                "removed what retention expired"
            ),
            Ok(_) => {}
            Err(error) => warn!(%error, "a round of retention failed"),
        }
    }
}

pub(crate) async fn round<S: Store>(
    db: &DatabaseConnection,
    store: &S,
    now: DateTime<Utc>,
    retention: Retention,
) -> Result<Option<Removed>, Failure> {
    let Some(round) = bosun_db::retention::begin(db).await? else {
        return Ok(None);
    };
    let removed = expire(db, store, now, retention).await;
    round.end().await?;
    removed.map(Some)
}

async fn expire<S: Store>(
    db: &DatabaseConnection,
    store: &S,
    now: DateTime<Utc>,
    retention: Retention,
) -> Result<Removed, Failure> {
    let mut removed = Removed::default();
    loop {
        let found = bosun_db::retention::expired_logs(db, now, retention.max_logs, BATCH).await?;
        let mut expired = 0;
        for execution in &found {
            if bosun_db::retention::expire_logs(db, *execution, now, retention.max_logs).await? {
                expired += 1;
            }
        }
        removed.logs += expired;
        if (found.len() as u64) < BATCH || expired == 0 {
            break;
        }
    }
    loop {
        let found = bosun_db::retention::expired_runs(db, now, retention.max_runs, BATCH).await?;
        let mut deleted = 0;
        for run in &found {
            if bosun_db::retention::delete_run(db, *run, now, retention.max_runs).await? {
                deleted += 1;
            }
        }
        removed.runs += deleted;
        if (found.len() as u64) < BATCH || deleted == 0 {
            break;
        }
    }
    if let Some(received_before) = now.checked_sub_signed(PAYLOADS) {
        loop {
            let cleared =
                bosun_db::retention::remove_payloads(db, now, received_before, ROWS).await?;
            removed.payloads += cleared;
            if cleared < ROWS {
                break;
            }
        }
    }
    if let Some(before) = now.checked_sub_signed(AGENT_EVENTS) {
        loop {
            let deleted = bosun_db::retention::remove_agent_events(db, before, ROWS).await?;
            removed.agent_events += deleted;
            if deleted < ROWS {
                break;
            }
        }
    }
    loop {
        let keys = bosun_db::retention::expired_objects(db, BATCH).await?;
        for key in &keys {
            if let Ok(key) = Key::new(key.clone()) {
                store.delete(&key).await?;
            }
        }
        bosun_db::retention::forget_objects(db, &keys).await?;
        removed.objects += keys.len() as u64;
        if (keys.len() as u64) < BATCH {
            break;
        }
    }
    Ok(removed)
}
