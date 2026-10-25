use bosun_types::actor::Actor;
use bosun_types::event::{Event, RunEvent};
use bosun_types::id::{AttemptId, ExecutionId, JobId, RunId, StepId};
use bosun_types::status::JobStatus;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
};
use serde_json::{Map, Value};

use crate::entity::run_event;
use crate::row::statement;
use crate::stream::{self, XACT};
use crate::{Error, outbox, row};

const TABLE: &str = "run_event";

const QUEUE: [JobStatus; 2] = [JobStatus::Queued, JobStatus::Waiting];

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
    let found = db
        .query_one(statement(
            &format!(
                "WITH stored AS (INSERT INTO run_event \
                 (run_id, attempt_id, job_id, execution_id, step_id, kind, actor, data, xact) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, greatest({XACT}, coalesce((SELECT xact \
                 FROM run_event WHERE run_id = $1 ORDER BY seq DESC LIMIT 1), 0))) RETURNING *) \
                 SELECT stored.* FROM stored CROSS JOIN (SELECT pg_notify($9, $10)) announced"
            ),
            [
                Uuid::from(event.run_id).into(),
                event.attempt_id.map(Uuid::from).into(),
                event.job_id.map(Uuid::from).into(),
                event.execution_id.map(Uuid::from).into(),
                event.step_id.map(Uuid::from).into(),
                event.event.kind().into(),
                serde_json::to_value(&event.actor)?.into(),
                data.into(),
                stream::CHANNEL.into(),
                stream::announcement(event.run_id).into(),
            ],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the journal of run {}", event.run_id)))?;
    let stored = run_event::Model::from_query_result(&found, "")?;
    if let (Event::JobStatusChanged(changed), Some(job)) = (&event.event, event.job_id)
        && (QUEUE.contains(&changed.from) || QUEUE.contains(&changed.to))
    {
        stream::queue_changed(db, job).await?;
    }
    outbox::record(
        db,
        event.run_id,
        event.job_id,
        event.execution_id,
        &event.event,
    )
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

pub(crate) fn decode(stored: run_event::Model) -> Result<RunEvent, Error> {
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
