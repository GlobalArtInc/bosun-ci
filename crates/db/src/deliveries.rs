use std::time::Duration;

use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde_json::Value;

use crate::row::statement;
use crate::{Error, row};

pub(crate) const CHANNEL: &str = "bosun_webhook";

const TABLE: &str = "webhook_delivery";

pub struct NewDelivery<'a> {
    pub delivery: &'a str,
    pub event: &'a str,
    pub payload: &'a Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub id: Uuid,
    pub delivery: String,
    pub event: String,
    pub payload: Value,
    pub attempts: u32,
}

pub async fn store<C: TransactionTrait>(db: &C, new: NewDelivery<'_>) -> Result<bool, Error> {
    let transaction = db.begin().await?;
    let id = Uuid::now_v7();
    let stored = transaction
        .execute(statement(
            "INSERT INTO webhook_delivery (id, delivery, event, payload) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (delivery) DO NOTHING",
            [
                id.into(),
                new.delivery.into(),
                new.event.into(),
                new.payload.clone().into(),
            ],
        ))
        .await?
        .rows_affected()
        > 0;
    if stored {
        transaction
            .execute(statement(
                "SELECT pg_notify($1, $2)",
                [CHANNEL.into(), id.to_string().into()],
            ))
            .await?;
    }
    transaction.commit().await?;
    Ok(stored)
}

pub async fn claim(transaction: &DatabaseTransaction) -> Result<Option<Delivery>, Error> {
    let Some(found) = transaction
        .query_one(statement(
            "SELECT id, delivery, event, payload, attempts FROM webhook_delivery \
             WHERE processed_at IS NULL AND next_attempt_at <= now() \
             ORDER BY next_attempt_at, id LIMIT 1 FOR UPDATE SKIP LOCKED",
            [],
        ))
        .await?
    else {
        return Ok(None);
    };
    let attempts: i32 = found.try_get("", "attempts")?;
    Ok(Some(Delivery {
        id: found.try_get("", "id")?,
        delivery: found.try_get("", "delivery")?,
        event: found.try_get("", "event")?,
        payload: found.try_get("", "payload")?,
        attempts: u32::try_from(attempts).map_err(|error| Error::Row {
            table: TABLE,
            message: error.to_string(),
        })?,
    }))
}

pub async fn finish(
    transaction: &DatabaseTransaction,
    id: Uuid,
    error: Option<&str>,
) -> Result<(), Error> {
    transaction
        .execute(statement(
            "UPDATE webhook_delivery SET processed_at = now(), attempts = attempts + 1, \
             last_error = $2 WHERE id = $1",
            [id.into(), error.map(str::to_owned).into()],
        ))
        .await?;
    Ok(())
}

pub async fn postpone(
    transaction: &DatabaseTransaction,
    id: Uuid,
    error: &str,
    wait: Duration,
) -> Result<(), Error> {
    transaction
        .execute(statement(
            "UPDATE webhook_delivery SET attempts = attempts + 1, \
             next_attempt_at = now() + make_interval(secs => $3), last_error = $2 WHERE id = $1",
            [id.into(), error.into(), wait.as_secs_f64().into()],
        ))
        .await?;
    Ok(())
}

pub async fn pending<C: ConnectionTrait>(db: &C) -> Result<u64, Error> {
    let found = db
        .query_one(statement(
            "SELECT count(*) AS pending FROM webhook_delivery WHERE processed_at IS NULL",
            [],
        ))
        .await?
        .ok_or(Error::Missing(TABLE))?;
    row::unsigned(TABLE, found.try_get("", "pending")?)
}
