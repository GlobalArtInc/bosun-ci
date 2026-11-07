use std::sync::Arc;
use std::time::Duration;

use bosun_db::signing_keys::{self, LiveSigningKey, NewSigningKey};
use bosun_services::{SigningKey, TokenError};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep};
use tracing::{info, warn};

use crate::Error;
use crate::clock::Clock;
use crate::job_token::MAX_LIFETIME;
use crate::keys::{JOB_TOKEN_KEY, Keyring, Unreadable};

const SWEEP: Duration = Duration::from_secs(60);

const LOCK_WAIT: Duration = Duration::from_secs(30);

const LOCK_RETRY: Duration = Duration::from_millis(50);

const MARGIN: TimeDelta = TimeDelta::hours(1);

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error(transparent)]
    Database(#[from] bosun_db::Error),
    #[error("the key that signs job tokens cannot be made: {0}")]
    Generate(#[source] TokenError),
    #[error("the key that signs job tokens cannot be encrypted: {0}")]
    Seal(#[source] Error),
    #[error("the key that signs job tokens cannot be decrypted: {0}")]
    Unreadable(Unreadable),
    #[error("the key that signs job tokens is not usable: {0}")]
    Corrupt(#[source] TokenError),
    #[error("the key that signs job tokens does not match the public key stored with it")]
    Mismatch,
    #[error("another replica held the lock of the key that signs job tokens for {0:?}")]
    Busy(Duration),
    #[error("the job token cannot be signed: {0}")]
    Sign(#[source] TokenError),
}

impl From<sea_orm::DbErr> for Failure {
    fn from(error: sea_orm::DbErr) -> Failure {
        Failure::Database(error.into())
    }
}

pub(crate) struct Swept {
    pub(crate) rotated: bool,
    pub(crate) purged: u64,
}

fn binding(id: Uuid) -> [u8; 16] {
    *id.as_bytes()
}

pub(crate) async fn signer(
    db: &DatabaseConnection,
    keys: &Keyring,
    now: DateTime<Utc>,
) -> Result<SigningKey, Failure> {
    let live = match signing_keys::live(db).await? {
        Some(live) => live,
        None => generate(db, keys, now).await?,
    };
    open(keys, &live)
}

fn open(keys: &Keyring, stored: &LiveSigningKey) -> Result<SigningKey, Failure> {
    let pkcs8 = keys
        .open_as(&JOB_TOKEN_KEY, &binding(stored.id), &stored.sealed)
        .map_err(Failure::Unreadable)?;
    let key = SigningKey::from_pkcs8(&pkcs8).map_err(Failure::Corrupt)?;
    if key.public_key() != stored.public_key {
        return Err(Failure::Mismatch);
    }
    Ok(key)
}

async fn generate(
    db: &DatabaseConnection,
    keys: &Keyring,
    now: DateTime<Utc>,
) -> Result<LiveSigningKey, Failure> {
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        let transaction = db.begin().await?;
        if signing_keys::try_lock(&transaction).await? {
            if let Some(live) = signing_keys::live(&transaction).await? {
                transaction.rollback().await?;
                return Ok(live);
            }
            let made = make(&transaction, keys, now).await?;
            transaction.commit().await?;
            info!(
                created_at = %made.created_at,
                "generated the key that signs job tokens"
            );
            return Ok(made);
        }
        transaction.rollback().await?;
        if let Some(live) = signing_keys::live(db).await? {
            return Ok(live);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Busy(LOCK_WAIT));
        }
        sleep(LOCK_RETRY).await;
    }
}

async fn make(
    transaction: &DatabaseTransaction,
    keys: &Keyring,
    now: DateTime<Utc>,
) -> Result<LiveSigningKey, Failure> {
    let (key, pkcs8) = SigningKey::generate().map_err(Failure::Generate)?;
    let id = Uuid::now_v7();
    let sealed = keys
        .seal_as(&JOB_TOKEN_KEY, &binding(id), &pkcs8)
        .map_err(Failure::Seal)?;
    let public_key = key.public_key();
    signing_keys::insert(
        transaction,
        &NewSigningKey {
            id,
            public_key,
            sealed: &sealed,
            created_at: now,
        },
    )
    .await?;
    Ok(LiveSigningKey {
        id,
        public_key,
        sealed,
        created_at: now,
    })
}

pub(crate) async fn sweep_once(
    db: &DatabaseConnection,
    keys: &Keyring,
    now: DateTime<Utc>,
    rotation: Duration,
) -> Result<Option<Swept>, Failure> {
    let transaction = db.begin().await?;
    if !signing_keys::try_lock(&transaction).await? {
        transaction.rollback().await?;
        return Ok(None);
    }
    let period = TimeDelta::from_std(rotation).unwrap_or(TimeDelta::MAX);
    let mut rotated = false;
    if let Some(live) = signing_keys::live(&transaction).await?
        && now - live.created_at >= period
    {
        if keys.holds(&live.sealed.kek_id) {
            signing_keys::retire(&transaction, live.id, now).await?;
            make(&transaction, keys, now).await?;
            rotated = true;
        } else {
            warn!(
                kek_id = live.sealed.kek_id,
                "the key that signs job tokens is due for rotation, but it is encrypted under a \
                 KEK that this server does not hold, so this replica leaves it to a replica that \
                 does; name that KEK in keys.previous_kek_files"
            );
        }
    }
    let purged = signing_keys::purge(&transaction, now - MAX_LIFETIME - MARGIN).await?;
    transaction.commit().await?;
    Ok(Some(Swept { rotated, purged }))
}

pub(crate) async fn sweep(
    db: DatabaseConnection,
    keys: Arc<Keyring>,
    clock: Clock,
    rotation: Duration,
) {
    let mut ticks = interval(SWEEP);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        match sweep_once(&db, &keys, clock.now(), rotation).await {
            Ok(Some(swept)) => {
                if swept.rotated {
                    info!("rotated the key that signs job tokens");
                }
                if swept.purged > 0 {
                    info!(
                        purged = swept.purged,
                        "removed retired keys whose job tokens have all expired"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "the rotation of the key that signs job tokens failed"),
        }
    }
}

pub(crate) async fn rewrap(
    db: &DatabaseConnection,
    keys: &Keyring,
    failures: &mut Vec<String>,
) -> Result<(), Error> {
    let Some(live) = signing_keys::live_under_another_kek(db, keys.current())
        .await
        .map_err(Error::Rewrap)?
    else {
        return Ok(());
    };
    let described = format!("the key that signs job tokens ({})", live.id);
    match keys.rewrap_as(&JOB_TOKEN_KEY, &binding(live.id), &live.sealed) {
        Ok(data_key) => {
            if signing_keys::rewrap(db, &live, data_key, keys.current())
                .await
                .map_err(Error::Rewrap)?
            {
                info!(key = %live.id, kek_id = keys.current(), "re-encrypted the key that signs job tokens");
            } else {
                failures.push(format!(
                    "{described} changed while its data key was re-encrypted; run the command again"
                ));
            }
        }
        Err(reason) => failures.push(format!("{described}: {reason}")),
    }
    Ok(())
}
