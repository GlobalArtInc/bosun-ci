use sea_orm::{ConnectionTrait, DatabaseTransaction};

use crate::Error;
use crate::row::statement;

pub async fn lock(transaction: &DatabaseTransaction, key: &str) -> Result<bool, Error> {
    Ok(transaction
        .query_one(statement(
            "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0)) AS locked",
            [key.into()],
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "locked"))
        .transpose()?
        .unwrap_or(false))
}
