use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use bosun_blob::{Directory, Key, Store};
use bosun_db::logs::{Compacted, Compaction, NewObject};
use bosun_types::id::{ExecutionId, StepId};
use chrono::DateTime;
use sea_orm::DatabaseConnection;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use super::notice;
use super::object::Encoder;

const SWEEP: Duration = Duration::from_secs(5);

const SETTLED: Duration = Duration::from_secs(10);

const BATCH: u64 = 100;

const PAGE: u64 = 256;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error(transparent)]
    Database(#[from] bosun_db::Error),
    #[error(transparent)]
    Store(#[from] bosun_blob::Error),
    #[error("a log could not be compressed: {0}")]
    Encode(#[from] io::Error),
    #[error(transparent)]
    Key(#[from] bosun_blob::KeyError),
}

pub(crate) async fn sweep(db: DatabaseConnection, store: Arc<Directory>) {
    let mut ticks = interval(SWEEP);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let found = match bosun_db::logs::compactable(&db, SETTLED, BATCH).await {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(error) => {
                warn!(%error, "the sweep of logs to compact failed");
                continue;
            }
        };
        for execution_id in found {
            match compact(&db, store.as_ref(), execution_id).await {
                Ok(Compacted::Done) => info!(%execution_id, "compacted the log of an execution"),
                Ok(Compacted::Changed) => info!(
                    %execution_id,
                    "the log of an execution changed while it was compacted, and the next sweep \
                     compacts it again"
                ),
                Ok(Compacted::Already | Compacted::Busy) => {}
                Err(error) => warn!(
                    %execution_id,
                    %error,
                    "the log of an execution could not be compacted"
                ),
            }
        }
    }
}

pub(crate) async fn compact<S: Store>(
    db: &DatabaseConnection,
    store: &S,
    execution: ExecutionId,
) -> Result<Compacted, Failure> {
    let mut compactor = match bosun_db::logs::compaction(db, execution).await? {
        Compaction::Started(compactor) => compactor,
        Compaction::Skipped(skipped) => return Ok(skipped),
    };
    let mut steps: HashMap<StepId, (Encoder, u64)> = HashMap::new();
    let mut read: u64 = 0;
    loop {
        let page = compactor.chunks(PAGE).await?;
        let last = (page.len() as u64) < PAGE;
        for chunk in page {
            read += 1;
            let (encoder, dropped) = steps.entry(chunk.step_id).or_default();
            encoder.push(&chunk.data).await?;
            *dropped += chunk.dropped;
        }
        if last {
            break;
        }
    }
    let mut objects = Vec::new();
    for step in &compactor.steps {
        let Some((mut encoder, dropped)) = steps.remove(&step.id) else {
            continue;
        };
        if dropped > 0 {
            let completed_at = step.completed_at.unwrap_or(DateTime::UNIX_EPOCH);
            encoder.push(&notice(completed_at, dropped)).await?;
        }
        let Some(encoded) = encoder.finish().await? else {
            continue;
        };
        let key = format!(
            "logs/{}/{}/{execution}/{read}/{}.log.zst",
            compactor.run_id, compactor.attempt_id, step.id
        );
        store
            .put(&Key::new(key.clone())?, &encoded.bytes[..])
            .await?;
        objects.push(NewObject {
            step_id: step.id,
            key,
            size: encoded.bytes.len() as u64,
            lines: encoded.lines,
            frames: encoded.frames,
        });
    }
    let compacted = compactor.finish(&objects).await?;
    if compacted == Compacted::Changed {
        for object in &objects {
            store.delete(&Key::new(object.key.clone())?).await?;
        }
    }
    Ok(compacted)
}
