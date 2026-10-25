use std::collections::HashMap;

use bosun_types::api::Annotation;
use bosun_types::id::{AgentId, ExecutionId, JobId, StepId};
use bosun_types::status::StepStatus;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QueryResult,
};

use crate::entity::{annotation, step_summary};
use crate::row::statement;
use crate::{Error, row};

const ANNOTATIONS: &str = "annotation";

const SUMMARIES: &str = "step_summary";

const INSERTED: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub key: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredSummary {
    Kept(Summary),
    Expired,
    Absent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobResults {
    pub summaries: Vec<Summary>,
    pub annotations: Vec<Annotation>,
}

pub async fn summary_target<C: ConnectionTrait>(
    db: &C,
    agent: AgentId,
    execution: ExecutionId,
    index: u32,
) -> Result<Option<StepId>, Error> {
    db.query_one(statement(
        "SELECT s.id FROM step s JOIN execution e ON e.id = s.execution_id \
         WHERE e.id = $1 AND e.agent_id = $2 AND s.index = $3 AND s.status <> $4 \
         AND NOT EXISTS (SELECT 1 FROM step_summary m WHERE m.step_id = s.id)",
        [
            Uuid::from(execution).into(),
            Uuid::from(agent).into(),
            i64::from(index).into(),
            row::text("step", StepStatus::Completed)?.into(),
        ],
    ))
    .await?
    .map(|found| row::id("step", found.try_get::<Uuid>("", "id")?))
    .transpose()
}

pub(crate) async fn record<C: ConnectionTrait>(
    db: &C,
    step: StepId,
    annotations: &[Annotation],
    summary: Option<&Summary>,
) -> Result<(), Error> {
    let rows = annotations
        .iter()
        .enumerate()
        .map(|(ordinal, annotation)| encode(step, ordinal, annotation))
        .collect::<Result<Vec<annotation::ActiveModel>, Error>>()?;
    for batch in rows.chunks(INSERTED) {
        annotation::Entity::insert_many(batch.to_vec())
            .exec(db)
            .await?;
    }
    if let Some(summary) = summary {
        db.execute(statement(
            "INSERT INTO step_summary (step_id, key, size) VALUES ($1, $2, $3) \
             ON CONFLICT (step_id) DO NOTHING",
            [
                Uuid::from(step).into(),
                summary.key.as_str().into(),
                row::signed(SUMMARIES, summary.size)?.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

pub async fn summary<C: ConnectionTrait>(
    db: &C,
    execution: ExecutionId,
    step: StepId,
) -> Result<Option<StoredSummary>, Error> {
    let Some(found) = db
        .query_one(statement(
            "SELECT e.logs_expired_at IS NOT NULL AS expired, m.key, m.size FROM step s \
             JOIN execution e ON e.id = s.execution_id \
             LEFT JOIN step_summary m ON m.step_id = s.id \
             WHERE s.id = $1 AND s.execution_id = $2",
            [Uuid::from(step).into(), Uuid::from(execution).into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    if found.try_get::<bool>("", "expired")? {
        return Ok(Some(StoredSummary::Expired));
    }
    let Some(key) = found.try_get::<Option<String>>("", "key")? else {
        return Ok(Some(StoredSummary::Absent));
    };
    Ok(Some(StoredSummary::Kept(Summary {
        key,
        size: row::unsigned(SUMMARIES, found.try_get("", "size")?)?,
    })))
}

pub async fn of_job<C: ConnectionTrait>(db: &C, job: JobId) -> Result<JobResults, Error> {
    let Some(latest) = db
        .query_one(statement(
            "SELECT id FROM execution WHERE job_id = $1 ORDER BY number DESC LIMIT 1",
            [Uuid::from(job).into()],
        ))
        .await?
    else {
        return Ok(JobResults::default());
    };
    let execution: Uuid = latest.try_get("", "id")?;
    let summaries = db
        .query_all(statement(
            "SELECT m.key, m.size FROM step_summary m JOIN step s ON s.id = m.step_id \
             WHERE s.execution_id = $1 ORDER BY s.index",
            [execution.into()],
        ))
        .await?
        .iter()
        .map(stored)
        .collect::<Result<Vec<Summary>, Error>>()?;
    let annotations = annotation::Entity::find()
        .from_raw_sql(statement(
            "SELECT a.* FROM annotation a JOIN step s ON s.id = a.step_id \
             WHERE s.execution_id = $1 ORDER BY s.index, a.ordinal",
            [execution.into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(decode)
        .collect::<Result<Vec<Annotation>, Error>>()?;
    Ok(JobResults {
        summaries,
        annotations,
    })
}

pub(crate) async fn of_steps<C: ConnectionTrait>(
    db: &C,
    steps: &[Uuid],
) -> Result<HashMap<Uuid, (Vec<Annotation>, Option<u64>)>, Error> {
    let mut found: HashMap<Uuid, (Vec<Annotation>, Option<u64>)> = HashMap::new();
    if steps.is_empty() {
        return Ok(found);
    }
    for stored in annotation::Entity::find()
        .filter(annotation::Column::StepId.is_in(steps.iter().copied()))
        .order_by_asc(annotation::Column::StepId)
        .order_by_asc(annotation::Column::Ordinal)
        .all(db)
        .await?
    {
        let step = stored.step_id;
        found.entry(step).or_default().0.push(decode(stored)?);
    }
    for stored in step_summary::Entity::find()
        .filter(step_summary::Column::StepId.is_in(steps.iter().copied()))
        .all(db)
        .await?
    {
        found.entry(stored.step_id).or_default().1 = Some(row::unsigned(SUMMARIES, stored.size)?);
    }
    Ok(found)
}

fn stored(found: &QueryResult) -> Result<Summary, Error> {
    Ok(Summary {
        key: found.try_get("", "key")?,
        size: row::unsigned(SUMMARIES, found.try_get("", "size")?)?,
    })
}

fn encode(
    step: StepId,
    ordinal: usize,
    annotation: &Annotation,
) -> Result<annotation::ActiveModel, Error> {
    let ordinal = i32::try_from(ordinal).map_err(|error| Error::Row {
        table: ANNOTATIONS,
        message: error.to_string(),
    })?;
    let number = |value: Option<u32>| ActiveValue::Set(value.map(i64::from));
    Ok(annotation::ActiveModel {
        step_id: ActiveValue::Set(Uuid::from(step)),
        ordinal: ActiveValue::Set(ordinal),
        kind: ActiveValue::Set(row::text(ANNOTATIONS, annotation.kind)?),
        message: ActiveValue::Set(annotation.message.clone()),
        title: ActiveValue::Set(annotation.title.clone()),
        file: ActiveValue::Set(annotation.file.clone()),
        line: number(annotation.line),
        end_line: number(annotation.end_line),
        start_column: number(annotation.column),
        end_column: number(annotation.end_column),
    })
}

fn decode(stored: annotation::Model) -> Result<Annotation, Error> {
    let number = |value: Option<i64>| {
        value
            .map(|value| {
                u32::try_from(value).map_err(|error| Error::Row {
                    table: ANNOTATIONS,
                    message: error.to_string(),
                })
            })
            .transpose()
    };
    Ok(Annotation {
        kind: row::name(ANNOTATIONS, stored.kind)?,
        message: stored.message,
        title: stored.title,
        file: stored.file,
        line: number(stored.line)?,
        end_line: number(stored.end_line)?,
        column: number(stored.start_column)?,
        end_column: number(stored.end_column)?,
    })
}
