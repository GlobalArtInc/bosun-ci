use std::collections::BTreeMap;

use bosun_types::api::{ForgeAccess, ForgePermission};
use bosun_types::id::{AgentId, ExecutionId};
use bosun_types::status::ExecutionStatus;
use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, QueryResult};

use crate::row::statement;
use crate::secrets::Sealed;
use crate::{Error, row};

const TABLE: &str = "forge_token";

const COLUMNS: &str =
    "id, execution_id, repository_id, permissions, ciphertext, data_key, kek_id, expires_at";

pub type Grant = BTreeMap<ForgePermission, ForgeAccess>;

pub struct NewForgeToken<'a> {
    pub id: Uuid,
    pub execution_id: ExecutionId,
    pub repository_id: u64,
    pub permissions: &'a Grant,
    pub sealed: &'a Sealed,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct StoredForgeToken {
    pub id: Uuid,
    pub execution_id: ExecutionId,
    pub repository_id: u64,
    pub permissions: Grant,
    pub sealed: Sealed,
    pub expires_at: DateTime<Utc>,
}

pub enum Holding {
    Token(Box<StoredForgeToken>),
    NoToken,
    Ended,
    Elsewhere,
}

pub async fn holding<C: ConnectionTrait>(
    db: &C,
    agent: AgentId,
    execution: ExecutionId,
) -> Result<Holding, Error> {
    let Some(found) = db
        .query_one(statement(
            "SELECT status FROM execution WHERE id = $1 AND agent_id = $2",
            [Uuid::from(execution).into(), Uuid::from(agent).into()],
        ))
        .await?
    else {
        return Ok(Holding::Elsewhere);
    };
    let status: ExecutionStatus = row::name("execution", found.try_get("", "status")?)?;
    if status == ExecutionStatus::Completed {
        return Ok(Holding::Ended);
    }
    Ok(match newest(db, execution).await? {
        Some(token) => Holding::Token(Box::new(token)),
        None => Holding::NoToken,
    })
}

pub async fn store<C: ConnectionTrait>(db: &C, token: &NewForgeToken<'_>) -> Result<(), Error> {
    db.execute(statement(
        "INSERT INTO forge_token (id, execution_id, repository_id, permissions, ciphertext, \
         data_key, kek_id, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        [
            token.id.into(),
            Uuid::from(token.execution_id).into(),
            row::signed(TABLE, token.repository_id)?.into(),
            serde_json::to_value(token.permissions)?.into(),
            token.sealed.ciphertext.clone().into(),
            token.sealed.data_key.clone().into(),
            token.sealed.kek_id.clone().into(),
            token.expires_at.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn newest<C: ConnectionTrait>(
    db: &C,
    execution: ExecutionId,
) -> Result<Option<StoredForgeToken>, Error> {
    db.query_one(statement(
        &format!(
            "SELECT {COLUMNS} FROM forge_token WHERE execution_id = $1 \
             ORDER BY expires_at DESC, id DESC LIMIT 1"
        ),
        [Uuid::from(execution).into()],
    ))
    .await?
    .map(|found| decode(&found))
    .transpose()
}

pub async fn of_execution<C: ConnectionTrait>(
    db: &C,
    execution: ExecutionId,
) -> Result<Vec<StoredForgeToken>, Error> {
    db.query_all(statement(
        &format!("SELECT {COLUMNS} FROM forge_token WHERE execution_id = $1 ORDER BY id"),
        [Uuid::from(execution).into()],
    ))
    .await?
    .iter()
    .map(decode)
    .collect()
}

pub async fn forget<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<(), Error> {
    db.execute(statement(
        "DELETE FROM forge_token WHERE id = $1",
        [id.into()],
    ))
    .await?;
    Ok(())
}

fn decode(found: &QueryResult) -> Result<StoredForgeToken, Error> {
    Ok(StoredForgeToken {
        id: found.try_get("", "id")?,
        execution_id: row::id(TABLE, found.try_get::<Uuid>("", "execution_id")?)?,
        repository_id: row::unsigned(TABLE, found.try_get("", "repository_id")?)?,
        permissions: row::json(TABLE, found.try_get("", "permissions")?)?,
        sealed: Sealed {
            ciphertext: found.try_get("", "ciphertext")?,
            data_key: found.try_get("", "data_key")?,
            kek_id: found.try_get("", "kek_id")?,
        },
        expires_at: found.try_get("", "expires_at")?,
    })
}
