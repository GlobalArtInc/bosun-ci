use std::collections::BTreeMap;

use bosun_types::id::{AgentId, JobId};
use bosun_types::status::ExecutionStatus;
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "action_ref";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub owner: String,
    pub repository: String,
    pub r#ref: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataFile {
    Source(String),
    Oversized(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub key: String,
    pub size: u64,
    pub sha256: Vec<u8>,
    pub metadata: BTreeMap<String, MetadataFile>,
}

pub struct NewStored<'a> {
    pub owner: &'a str,
    pub repository: &'a str,
    pub commit: &'a str,
    pub key: &'a str,
    pub size: u64,
    pub sha256: &'a [u8],
    pub metadata: &'a BTreeMap<String, MetadataFile>,
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    owner: &str,
    repository: &str,
    commit: &str,
) -> Result<Option<Stored>, Error> {
    let found = db
        .query_one(statement(
            "SELECT key, size, sha256, metadata FROM action_ref \
             WHERE owner = $1 AND repository = $2 AND sha = $3",
            [owner.into(), repository.into(), commit.into()],
        ))
        .await?;
    found
        .map(|found| {
            Ok(Stored {
                key: found.try_get("", "key")?,
                size: row::unsigned(TABLE, found.try_get("", "size")?)?,
                sha256: found.try_get("", "sha256")?,
                metadata: row::json(TABLE, found.try_get("", "metadata")?)?,
            })
        })
        .transpose()
}

pub async fn record(transaction: &DatabaseTransaction, new: NewStored<'_>) -> Result<(), Error> {
    transaction
        .execute(statement(
            "INSERT INTO action_ref (id, owner, repository, sha, key, size, sha256, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (owner, repository, sha) DO NOTHING",
            [
                Uuid::now_v7().into(),
                new.owner.into(),
                new.repository.into(),
                new.commit.into(),
                new.key.into(),
                row::signed(TABLE, new.size)?.into(),
                new.sha256.to_vec().into(),
                serde_json::to_value(new.metadata)?.into(),
            ],
        ))
        .await?;
    Ok(())
}

pub async fn pin(transaction: &DatabaseTransaction, job: JobId, pins: &[Pin]) -> Result<(), Error> {
    transaction
        .execute(statement(
            "UPDATE job SET actions = $2 WHERE id = $1 AND actions IS NULL",
            [Uuid::from(job).into(), serde_json::to_value(pins)?.into()],
        ))
        .await?;
    Ok(())
}

pub async fn held<C: ConnectionTrait>(
    db: &C,
    agent: AgentId,
    owner: &str,
    repository: &str,
    commit: &str,
) -> Result<bool, Error> {
    let wanted: Value = json!([{ "owner": owner, "repository": repository, "commit": commit }]);
    let found = db
        .query_one(statement(
            "SELECT EXISTS (SELECT 1 FROM execution e JOIN job j ON j.id = e.job_id \
             WHERE e.agent_id = $1 AND e.status <> $2 AND j.actions @> $3) AS held",
            [
                Uuid::from(agent).into(),
                row::text("execution", ExecutionStatus::Completed)?.into(),
                wanted.into(),
            ],
        ))
        .await?
        .ok_or(Error::Missing("execution"))?;
    Ok(found.try_get("", "held")?)
}
