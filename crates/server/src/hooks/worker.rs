use std::sync::Arc;
use std::time::{Duration, Instant};

use bosun_db::deliveries::{self, Delivery};
use chrono::Utc;
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use tokio::sync::Notify;
use tracing::{info, warn};

use super::installations;
use crate::forge::Forge;
use crate::forge_events::{self, Failed, Parses};
use crate::site::Site;

const POLL: Duration = Duration::from_secs(5);

const PAUSE: Duration = Duration::from_secs(1);

const FIRST_RETRY: Duration = Duration::from_secs(1);

const LAST_RETRY: Duration = Duration::from_secs(3600);

pub(super) enum Failure {
    Retry { error: String, at_least: Duration },
    Refused(String),
}

impl From<bosun_db::Error> for Failure {
    fn from(error: bosun_db::Error) -> Failure {
        Failure::Retry {
            error: error.to_string(),
            at_least: Duration::ZERO,
        }
    }
}

impl From<Failed> for Failure {
    fn from(failed: Failed) -> Failure {
        match failed {
            Failed::Database(error) => error.into(),
            Failed::Forge(error) => error.into(),
            Failed::Payload(error) => Failure::Refused(format!(
                "the payload does not have the shape of its event: {error}"
            )),
        }
    }
}

impl From<bosun_forge::Error> for Failure {
    fn from(error: bosun_forge::Error) -> Failure {
        let at_least = match &error {
            bosun_forge::Error::RateLimited { reset } => {
                (*reset - Utc::now()).to_std().unwrap_or(Duration::ZERO)
            }
            bosun_forge::Error::Transport(_) => Duration::ZERO,
            bosun_forge::Error::Status { status, .. } if *status >= 500 => Duration::ZERO,
            _ => return Failure::Refused(format!("the forge refused: {error}")),
        };
        Failure::Retry {
            error: format!("the forge failed: {error}"),
            at_least,
        }
    }
}

struct Planning {
    parses: Arc<Parses>,
    site: Site,
}

pub(crate) async fn work(
    db: DatabaseConnection,
    forge: Arc<Forge>,
    wake: Arc<Notify>,
    parses: Arc<Parses>,
    site: Site,
) {
    let planning = Planning { parses, site };
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
        event => forge_events::delivered(
            transaction,
            forge,
            &planning.parses,
            &planning.site,
            event,
            &delivery.payload,
        )
        .await
        .map_err(Failure::from),
    }
}

fn backoff(attempts: u32) -> Duration {
    FIRST_RETRY
        .checked_mul(1 << attempts.min(12))
        .unwrap_or(LAST_RETRY)
        .min(LAST_RETRY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::backoff;

    #[test]
    fn a_failed_delivery_waits_twice_as_long_each_time_up_to_an_hour() {
        let waits: Vec<u64> = (0..14)
            .map(|attempts| backoff(attempts).as_secs())
            .collect();
        assert_eq!(
            waits,
            [
                1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 3600, 3600
            ]
        );
        assert_eq!(backoff(u32::MAX), Duration::from_secs(3600));
    }
}
