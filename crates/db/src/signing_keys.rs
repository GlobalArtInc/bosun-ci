use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};

use crate::Error;
use crate::periodic::{self, Task};
use crate::row::statement;
use crate::secrets::Sealed;

const TABLE: &str = "signing_key";

const COLUMNS: &str = "id, public_key, ciphertext, data_key, kek_id, created_at";

pub struct NewSigningKey<'a> {
    pub id: Uuid,
    pub public_key: [u8; 32],
    pub sealed: &'a Sealed,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LiveSigningKey {
    pub id: Uuid,
    pub public_key: [u8; 32],
    pub sealed: Sealed,
    pub created_at: DateTime<Utc>,
}

pub async fn try_lock(transaction: &DatabaseTransaction) -> Result<bool, Error> {
    periodic::try_lock(transaction, Task::SigningKey).await
}

pub async fn live<C: ConnectionTrait>(db: &C) -> Result<Option<LiveSigningKey>, Error> {
    db.query_one(statement(
        &format!("SELECT {COLUMNS} FROM signing_key WHERE retired_at IS NULL"),
        [],
    ))
    .await?
    .map(|found| decode(&found))
    .transpose()
}

pub async fn insert<C: ConnectionTrait>(db: &C, key: &NewSigningKey<'_>) -> Result<(), Error> {
    db.execute(statement(
        "INSERT INTO signing_key (id, public_key, ciphertext, data_key, kek_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        [
            key.id.into(),
            key.public_key.to_vec().into(),
            key.sealed.ciphertext.clone().into(),
            key.sealed.data_key.clone().into(),
            key.sealed.kek_id.clone().into(),
            key.created_at.into(),
        ],
    ))
    .await?;
    Ok(())
}

pub async fn retire<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
    retired_at: DateTime<Utc>,
) -> Result<bool, Error> {
    let retired = db
        .execute(statement(
            "UPDATE signing_key SET retired_at = $2, ciphertext = NULL, data_key = NULL, \
             kek_id = NULL WHERE id = $1 AND retired_at IS NULL",
            [id.into(), retired_at.into()],
        ))
        .await?;
    Ok(retired.rows_affected() == 1)
}

pub async fn purge<C: ConnectionTrait>(
    db: &C,
    retired_before: DateTime<Utc>,
) -> Result<u64, Error> {
    let purged = db
        .execute(statement(
            "DELETE FROM signing_key WHERE retired_at < $1",
            [retired_before.into()],
        ))
        .await?;
    Ok(purged.rows_affected())
}

pub async fn public_keys<C: ConnectionTrait>(db: &C) -> Result<Vec<[u8; 32]>, Error> {
    db.query_all(statement("SELECT public_key FROM signing_key", []))
        .await?
        .iter()
        .map(|found| public_key(&found.try_get::<Vec<u8>>("", "public_key")?))
        .collect()
}

pub async fn live_under_another_kek<C: ConnectionTrait>(
    db: &C,
    kek_id: &str,
) -> Result<Option<LiveSigningKey>, Error> {
    db.query_one(statement(
        &format!("SELECT {COLUMNS} FROM signing_key WHERE retired_at IS NULL AND kek_id <> $1"),
        [kek_id.into()],
    ))
    .await?
    .map(|found| decode(&found))
    .transpose()
}

pub async fn rewrap<C: ConnectionTrait>(
    db: &C,
    key: &LiveSigningKey,
    data_key: Vec<u8>,
    kek_id: &str,
) -> Result<bool, Error> {
    let rewrapped = db
        .execute(statement(
            "UPDATE signing_key SET data_key = $3, kek_id = $4 \
             WHERE id = $1 AND data_key = $2 AND retired_at IS NULL",
            [
                key.id.into(),
                key.sealed.data_key.clone().into(),
                data_key.into(),
                kek_id.into(),
            ],
        ))
        .await?;
    Ok(rewrapped.rows_affected() == 1)
}

fn decode(found: &QueryResult) -> Result<LiveSigningKey, Error> {
    Ok(LiveSigningKey {
        id: found.try_get("", "id")?,
        public_key: public_key(&found.try_get::<Vec<u8>>("", "public_key")?)?,
        sealed: Sealed {
            ciphertext: found.try_get("", "ciphertext")?,
            data_key: found.try_get("", "data_key")?,
            kek_id: found.try_get("", "kek_id")?,
        },
        created_at: found.try_get("", "created_at")?,
    })
}

fn public_key(bytes: &[u8]) -> Result<[u8; 32], Error> {
    bytes.try_into().map_err(|_| Error::Row {
        table: TABLE,
        message: format!(
            "a public key of {} bytes is not an Ed25519 key",
            bytes.len()
        ),
    })
}
