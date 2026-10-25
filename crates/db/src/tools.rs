use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction};

use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "tool_object";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub key: String,
    pub size: u64,
}

pub struct NewStored<'a> {
    pub tool: &'a str,
    pub version: &'a str,
    pub platform: &'a str,
    pub key: &'a str,
    pub size: u64,
    pub sha256: &'a [u8],
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    tool: &str,
    version: &str,
    platform: &str,
) -> Result<Option<Stored>, Error> {
    let found = db
        .query_one(statement(
            "SELECT key, size FROM tool_object WHERE tool = $1 AND version = $2 AND platform = $3",
            [tool.into(), version.into(), platform.into()],
        ))
        .await?;
    found
        .map(|found| {
            Ok(Stored {
                key: found.try_get("", "key")?,
                size: row::unsigned(TABLE, found.try_get("", "size")?)?,
            })
        })
        .transpose()
}

pub async fn record(transaction: &DatabaseTransaction, new: NewStored<'_>) -> Result<(), Error> {
    transaction
        .execute(statement(
            "INSERT INTO tool_object (id, tool, version, platform, key, size, sha256) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (tool, version, platform) DO NOTHING",
            [
                Uuid::now_v7().into(),
                new.tool.into(),
                new.version.into(),
                new.platform.into(),
                new.key.into(),
                row::signed(TABLE, new.size)?.into(),
                new.sha256.to_vec().into(),
            ],
        ))
        .await?;
    Ok(())
}
