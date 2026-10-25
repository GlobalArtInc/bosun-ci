use bosun_types::id::{AgentId, ExecutionId};
use sea_orm::prelude::Uuid;
use sea_orm::{ActiveModelTrait, ActiveValue, ConnectionTrait, QueryResult, TransactionTrait};

use crate::entity::control_message;
use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "control_message";

const CHANNEL: &str = "bosun_replica_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cancel,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Cancel => "cancel",
        }
    }

    fn parse(name: &str) -> Result<Kind, Error> {
        match name {
            "cancel" => Ok(Kind::Cancel),
            other => Err(Error::Row {
                table: TABLE,
                message: format!("`{other}` is not a kind of control message"),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub id: Uuid,
    pub agent_id: AgentId,
    pub execution_id: ExecutionId,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub agent_id: AgentId,
    pub control_id: Uuid,
}

pub fn channel(replica_id: Uuid) -> String {
    format!("{CHANNEL}{}", replica_id.simple())
}

pub async fn publish<C: TransactionTrait>(
    db: &C,
    execution: ExecutionId,
    kind: Kind,
) -> Result<Control, Error> {
    let transaction = db.begin().await?;
    let target = transaction
        .query_one(statement(
            "SELECT a.id AS agent_id, a.replica_id FROM execution e \
             JOIN agent a ON a.id = e.agent_id WHERE e.id = $1 FOR SHARE OF a",
            [Uuid::from(execution).into()],
        ))
        .await?
        .ok_or(Error::NoExecution(execution))?;
    let agent_id: Uuid = target.try_get("", "agent_id")?;
    let replica_id: Option<Uuid> = target.try_get("", "replica_id")?;
    let stored = control_message::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        execution_id: ActiveValue::Set(execution.into()),
        kind: ActiveValue::Set(kind.name().to_owned()),
        created_at: ActiveValue::NotSet,
        acknowledged_at: ActiveValue::Set(None),
        took_effect: ActiveValue::Set(None),
    }
    .insert(&transaction)
    .await?;
    if let Some(replica_id) = replica_id {
        transaction
            .execute(statement(
                "SELECT pg_notify($1, $2)",
                [
                    channel(replica_id).into(),
                    format!("{agent_id} {}", stored.id).into(),
                ],
            ))
            .await?;
    }
    transaction.commit().await?;
    Ok(Control {
        id: stored.id,
        agent_id: row::id("agent", agent_id)?,
        execution_id: execution,
        kind,
    })
}

pub async fn find_pending<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Control>, Error> {
    db.query_one(statement(
        "SELECT c.id, e.agent_id, c.execution_id, c.kind FROM control_message c \
         JOIN execution e ON e.id = c.execution_id \
         WHERE c.id = $1 AND c.acknowledged_at IS NULL",
        [id.into()],
    ))
    .await?
    .map(decode)
    .transpose()
}

pub async fn pending<C: ConnectionTrait>(db: &C, agent: AgentId) -> Result<Vec<Control>, Error> {
    db.query_all(statement(
        "SELECT c.id, e.agent_id, c.execution_id, c.kind FROM control_message c \
         JOIN execution e ON e.id = c.execution_id \
         WHERE e.agent_id = $1 AND c.acknowledged_at IS NULL ORDER BY c.id",
        [Uuid::from(agent).into()],
    ))
    .await?
    .into_iter()
    .map(decode)
    .collect()
}

pub async fn acknowledge<C: ConnectionTrait>(
    db: &C,
    agent: AgentId,
    id: Uuid,
    took_effect: bool,
) -> Result<bool, Error> {
    let updated = db
        .execute(statement(
            "UPDATE control_message c SET acknowledged_at = now(), took_effect = $3 \
             FROM execution e WHERE c.id = $1 AND e.id = c.execution_id AND e.agent_id = $2 \
             AND c.acknowledged_at IS NULL",
            [id.into(), Uuid::from(agent).into(), took_effect.into()],
        ))
        .await?;
    Ok(updated.rows_affected() == 1)
}

pub(crate) fn notice(payload: &str) -> Option<Notice> {
    let (agent, control) = payload.split_once(' ')?;
    Some(Notice {
        agent_id: agent.parse().ok()?,
        control_id: control.parse().ok()?,
    })
}

fn decode(found: QueryResult) -> Result<Control, Error> {
    Ok(Control {
        id: found.try_get("", "id")?,
        agent_id: row::id("execution", found.try_get::<Uuid>("", "agent_id")?)?,
        execution_id: row::id(TABLE, found.try_get::<Uuid>("", "execution_id")?)?,
        kind: Kind::parse(&found.try_get::<String>("", "kind")?)?,
    })
}
