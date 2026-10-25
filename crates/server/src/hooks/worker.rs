use std::sync::Arc;
use std::time::{Duration, Instant};

use bosun_db::deliveries::{self, Delivery};
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use tokio::sync::Notify;
use tracing::{info, warn};

use super::installations;
use crate::clock::Clock;
use crate::forge::Forge;
use crate::forge_events::{self, Failed, Parses};
use crate::retry::{Failure, backoff};
use crate::site::Site;

const POLL: Duration = Duration::from_secs(5);

const PAUSE: Duration = Duration::from_secs(1);

impl From<Failed> for Failure {
    fn from(failed: Failed) -> Failure {
        match failed {
            Failed::Database(error) => error.into(),
            Failed::Forge(error) => error.into(),
            Failed::Payload(error) => Failure::Refused(format!(
                "the payload does not have the shape of its event: {error}"
            )),
            Failed::Unmerged(error) => Failure::Retry {
                error,
                at_least: Duration::ZERO,
            },
        }
    }
}

struct Planning {
    parses: Arc<Parses>,
    site: Site,
    clock: Clock,
}

pub(crate) async fn work(
    db: DatabaseConnection,
    forge: Arc<Forge>,
    wake: Arc<Notify>,
    parses: Arc<Parses>,
    site: Site,
    clock: Clock,
) {
    let planning = Planning {
        parses,
        site,
        clock,
    };
    loop {
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        match next(&db, &forge, &planning).await {
            Ok(true) => {}
            Ok(false) => {
                tokio::select! {
                    () = notified => {}
                    () = tokio::time::sleep(POLL) => {}
                }
            }
            Err(error) => {
                warn!(%error, "the webhook deliveries could not be processed");
                tokio::time::sleep(PAUSE).await;
            }
        }
    }
}

async fn next(
    db: &DatabaseConnection,
    forge: &Forge,
    planning: &Planning,
) -> Result<bool, bosun_db::Error> {
    let transaction = db.begin().await?;
    let Some(delivery) = deliveries::claim(&transaction).await? else {
        return Ok(false);
    };
    let started = Instant::now();
    let nested = transaction.begin().await?;
    match process(&nested, forge, planning, &delivery).await {
        Ok(()) => {
            nested.commit().await?;
            deliveries::finish(&transaction, delivery.id, None).await?;
            transaction.commit().await?;
            info!(
                delivery = delivery.delivery,
                event = delivery.event,
                duration_ms = started.elapsed().as_millis(),
                "processed a webhook delivery"
            );
        }
        Err(Failure::Retry { error, at_least }) => {
            nested.rollback().await?;
            let wait = backoff(delivery.attempts).max(at_least);
            deliveries::postpone(&transaction, delivery.id, &error, wait).await?;
            transaction.commit().await?;
            warn!(
                delivery = delivery.delivery,
                event = delivery.event,
                error,
                wait_ms = wait.as_millis(),
                "a webhook delivery failed and is tried again"
            );
        }
        Err(Failure::Refused(error)) => {
            nested.rollback().await?;
            deliveries::finish(&transaction, delivery.id, Some(&error)).await?;
            transaction.commit().await?;
            warn!(
                delivery = delivery.delivery,
                event = delivery.event,
                error,
                "a webhook delivery cannot be processed and is not tried again"
            );
        }
    }
    Ok(true)
}

async fn process(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    planning: &Planning,
    delivery: &Delivery,
) -> Result<(), Failure> {
    match delivery.event.as_str() {
        "installation" => installations::installation(transaction, forge, &delivery.payload).await,
        "installation_repositories" => {
            installations::installation_repositories(transaction, forge, &delivery.payload).await
        }
        "repository" => installations::repository(transaction, forge, &delivery.payload).await,
        _ => forge_events::delivered(
            transaction,
            forge,
            &planning.parses,
            &planning.site,
            delivery,
            planning.clock.now(),
        )
        .await
        .map_err(Failure::from),
    }
}
