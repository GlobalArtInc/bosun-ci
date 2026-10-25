use std::collections::{BTreeMap, BTreeSet};

use bosun_types::actor::Actor;
use bosun_types::id::{ProjectId, RevisionId, ScheduleId, WorkflowId};
use bosun_types::ir::Cron;
use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};

use crate::Error;
use crate::entity::{schedule, workflow};
use crate::periodic::{self, Task};
use crate::row::{self, statement};

const TABLE: &str = "schedule";

const REPOSITORY: &str = "repository";

pub enum Scope {
    Workflow(WorkflowId),
    Repository(ProjectId),
}

pub struct Entry<'a> {
    pub workflow_id: WorkflowId,
    pub revision_id: RevisionId,
    pub expression: &'a str,
    pub timezone: &'a str,
    pub cron: &'a Cron,
    pub next_at: Option<DateTime<Utc>>,
}

pub struct Origin<'a> {
    pub git_ref: Option<&'a str>,
    pub sha: Option<&'a str>,
    pub actor: &'a Actor,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Replaced {
    pub added: u64,
    pub kept: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub id: ScheduleId,
    pub workflow_id: WorkflowId,
    pub revision_id: RevisionId,
    pub expression: String,
    pub timezone: String,
    pub cron: Cron,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    pub actor: Actor,
    pub next_at: DateTime<Utc>,
}

pub async fn replace<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    entries: &[Entry<'_>],
    origin: Origin<'_>,
) -> Result<Replaced, Error> {
    let workflows = match scope {
        Scope::Workflow(id) => {
            db.execute(statement(
                "SELECT id FROM workflow WHERE id = $1 FOR NO KEY UPDATE",
                [Uuid::from(id).into()],
            ))
            .await?;
            vec![Uuid::from(id)]
        }
        Scope::Repository(project) => {
            db.execute(statement(
                "SELECT id FROM project WHERE id = $1 FOR NO KEY UPDATE",
                [Uuid::from(project).into()],
            ))
            .await?;
            workflow::Entity::find()
                .select_only()
                .column(workflow::Column::Id)
                .filter(workflow::Column::ProjectId.eq(Uuid::from(project)))
                .filter(workflow::Column::SourceKind.eq(REPOSITORY))
                .into_tuple::<Uuid>()
                .all(db)
                .await?
        }
    };
    let mut existing: BTreeMap<(Uuid, String, String), Uuid> = schedule::Entity::find()
        .filter(schedule::Column::WorkflowId.is_in(workflows))
        .all(db)
        .await?
        .into_iter()
        .map(|row| ((row.workflow_id, row.expression, row.timezone), row.id))
        .collect();
    let actor = serde_json::to_value(origin.actor)?;
    let mut seen = BTreeSet::new();
    let mut replaced = Replaced::default();
    for entry in entries {
        let key = (
            Uuid::from(entry.workflow_id),
            entry.expression.to_owned(),
            entry.timezone.to_owned(),
        );
        if !seen.insert(key.clone()) {
            continue;
        }
        let cron = serde_json::to_value(entry.cron)?;
        let git_ref = origin.git_ref.map(str::to_owned);
        let sha = origin.sha.map(str::to_owned);
        match existing.remove(&key) {
            Some(id) => {
                schedule::ActiveModel {
                    id: ActiveValue::Unchanged(id),
                    revision_id: ActiveValue::Set(entry.revision_id.into()),
                    cron: ActiveValue::Set(cron),
                    git_ref: ActiveValue::Set(git_ref),
                    sha: ActiveValue::Set(sha),
                    ..Default::default()
                }
                .update(db)
                .await?;
                replaced.kept += 1;
            }
            None => {
                let (workflow_id, expression, timezone) = key;
                schedule::ActiveModel {
                    id: ActiveValue::Set(Uuid::now_v7()),
                    workflow_id: ActiveValue::Set(workflow_id),
                    revision_id: ActiveValue::Set(entry.revision_id.into()),
                    expression: ActiveValue::Set(expression),
                    timezone: ActiveValue::Set(timezone),
                    cron: ActiveValue::Set(cron),
                    git_ref: ActiveValue::Set(git_ref),
                    sha: ActiveValue::Set(sha),
                    actor: ActiveValue::Set(actor.clone()),
                    next_at: ActiveValue::Set(entry.next_at),
                    created_at: ActiveValue::NotSet,
                }
                .insert(db)
                .await?;
                replaced.added += 1;
            }
        }
    }
    let removed: Vec<Uuid> = existing.into_values().collect();
    if !removed.is_empty() {
        replaced.removed = schedule::Entity::delete_many()
            .filter(schedule::Column::Id.is_in(removed))
            .exec(db)
            .await?
            .rows_affected;
    }
    Ok(replaced)
}

pub(crate) async fn forget_repository<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<(), Error> {
    db.execute(statement(
        "DELETE FROM schedule WHERE workflow_id IN \
         (SELECT id FROM workflow WHERE project_id = $1 AND source_kind = $2)",
        [Uuid::from(project).into(), REPOSITORY.into()],
    ))
    .await?;
    Ok(())
}

pub async fn due<C: TransactionTrait>(
    db: &C,
    now: DateTime<Utc>,
) -> Result<Option<Vec<ScheduleId>>, Error> {
    let transaction = db.begin().await?;
    if !periodic::try_lock(&transaction, Task::Schedules).await? {
        return Ok(None);
    }
    let found = schedule::Entity::find()
        .select_only()
        .column(schedule::Column::Id)
        .filter(schedule::Column::NextAt.lte(now))
        .order_by_asc(schedule::Column::NextAt)
        .order_by_asc(schedule::Column::Id)
        .into_tuple::<Uuid>()
        .all(&transaction)
        .await?;
    transaction.commit().await?;
    found
        .into_iter()
        .map(|id| row::id(TABLE, id))
        .collect::<Result<Vec<_>, Error>>()
        .map(Some)
}

pub async fn lock_due(
    transaction: &DatabaseTransaction,
    id: ScheduleId,
    now: DateTime<Utc>,
) -> Result<Option<Due>, Error> {
    let Some(found) = schedule::Entity::find()
        .from_raw_sql(statement(
            "SELECT * FROM schedule WHERE id = $1 AND next_at <= $2 FOR UPDATE SKIP LOCKED",
            [Uuid::from(id).into(), now.into()],
        ))
        .one(transaction)
        .await?
    else {
        return Ok(None);
    };
    let Some(next_at) = found.next_at else {
        return Ok(None);
    };
    Ok(Some(Due {
        id,
        workflow_id: row::id(TABLE, found.workflow_id)?,
        revision_id: row::id(TABLE, found.revision_id)?,
        expression: found.expression,
        timezone: found.timezone,
        cron: row::json(TABLE, found.cron)?,
        git_ref: found.git_ref,
        sha: found.sha,
        actor: row::json(TABLE, found.actor)?,
        next_at,
    }))
}

pub async fn advance<C: ConnectionTrait>(
    db: &C,
    id: ScheduleId,
    next_at: Option<DateTime<Utc>>,
) -> Result<(), Error> {
    db.execute(statement(
        "UPDATE schedule SET next_at = $2 WHERE id = $1",
        [Uuid::from(id).into(), next_at.into()],
    ))
    .await?;
    Ok(())
}
