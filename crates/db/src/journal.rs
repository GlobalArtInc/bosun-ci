use bosun_types::actor::Actor;
use bosun_types::event::{Event, RunEvent};
use bosun_types::id::{AttemptId, ExecutionId, JobId, RunId, StepId};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde_json::{Map, Value};

use crate::entity::run_event;
use crate::{Error, row};

const TABLE: &str = "run_event";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub run_id: RunId,
    pub attempt_id: Option<AttemptId>,
    pub job_id: Option<JobId>,
    pub execution_id: Option<ExecutionId>,
    pub step_id: Option<StepId>,
    pub actor: Actor,
    pub event: Event,
}

pub async fn append<C: ConnectionTrait>(db: &C, event: NewEvent) -> Result<RunEvent, Error> {
    let mut encoded = serde_json::to_value(&event.event)?;
    let data = encoded
        .get_mut("data")
        .map(Value::take)
        .unwrap_or(Value::Null);
    let stored = run_event::ActiveModel {
        seq: ActiveValue::NotSet,
        run_id: ActiveValue::Set(event.run_id.into()),
        attempt_id: ActiveValue::Set(event.attempt_id.map(Uuid::from)),
        job_id: ActiveValue::Set(event.job_id.map(Uuid::from)),
        execution_id: ActiveValue::Set(event.execution_id.map(Uuid::from)),
        step_id: ActiveValue::Set(event.step_id.map(Uuid::from)),
        kind: ActiveValue::Set(event.event.kind().to_owned()),
        actor: ActiveValue::Set(serde_json::to_value(&event.actor)?),
        data: ActiveValue::Set(data),
        at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    decode(stored)
}

pub async fn read<C: ConnectionTrait>(
    db: &C,
    run_id: RunId,
    after: u64,
) -> Result<Vec<RunEvent>, Error> {
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    run_event::Entity::find()
        .filter(run_event::Column::RunId.eq(Uuid::from(run_id)))
        .filter(run_event::Column::Seq.gt(after))
        .order_by_asc(run_event::Column::Seq)
        .all(db)
        .await?
        .into_iter()
        .map(decode)
        .collect()
}

fn decode(stored: run_event::Model) -> Result<RunEvent, Error> {
    let mut tagged = Map::new();
    tagged.insert("kind".to_owned(), Value::String(stored.kind));
    tagged.insert("data".to_owned(), stored.data);
    Ok(RunEvent {
        seq: u64::try_from(stored.seq).map_err(|error| Error::Row {
            table: TABLE,
            message: error.to_string(),
        })?,
        run_id: row::id(TABLE, stored.run_id)?,
        attempt_id: row::optional_id(TABLE, stored.attempt_id)?,
        job_id: row::optional_id(TABLE, stored.job_id)?,
        execution_id: row::optional_id(TABLE, stored.execution_id)?,
        step_id: row::optional_id(TABLE, stored.step_id)?,
        actor: row::json(TABLE, stored.actor)?,
        at: stored.at,
        event: row::json(TABLE, Value::Object(tagged))?,
    })
}
