use bosun_types::api::{Revision, Workflow, WorkflowSource};
use bosun_types::id::{ProjectId, RevisionId, WorkflowId};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::{LockType, OnConflict};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, TryInsertResult,
};
use serde_json::json;

use crate::entity::{project, workflow, workflow_revision};
use crate::{Error, row};

const WORKFLOWS: &str = "workflow";

const REVISIONS: &str = "workflow_revision";

const SERVER: &str = "server";

pub struct NewWorkflow<'a> {
    pub project_id: ProjectId,
    pub name: &'a str,
    pub source: &'a str,
    pub sha256: Vec<u8>,
}

pub struct Saved {
    pub revision: Revision,
    pub created: bool,
}

pub async fn create<C: ConnectionTrait>(db: &C, new: NewWorkflow<'_>) -> Result<Workflow, Error> {
    let project_id = Uuid::from(new.project_id);
    project::Entity::find_by_id(project_id)
        .lock(LockType::NoKeyUpdate)
        .one(db)
        .await?;
    let taken = workflow::Entity::find()
        .filter(workflow::Column::ProjectId.eq(project_id))
        .filter(workflow::Column::SourceKind.eq(SERVER))
        .filter(workflow::Column::Name.eq(new.name))
        .one(db)
        .await?;
    if taken.is_some() {
        return Err(Error::WorkflowTaken(new.name.to_owned()));
    }
    let created = workflow::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        project_id: ActiveValue::Set(project_id),
        name: ActiveValue::Set(new.name.to_owned()),
        source_kind: ActiveValue::Set(SERVER.to_owned()),
        enabled: ActiveValue::Set(true),
        revision_id: ActiveValue::Set(None),
        created_at: ActiveValue::NotSet,
        last_run_number: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    let (current, _) = store(db, created.id, new.source, new.sha256).await?;
    decode(current)
}

pub async fn save<C: ConnectionTrait>(
    db: &C,
    id: WorkflowId,
    source: &str,
    sha256: Vec<u8>,
) -> Result<Saved, Error> {
    let (_, saved) = store(db, id.into(), source, sha256).await?;
    Ok(saved)
}

pub async fn find<C: ConnectionTrait>(db: &C, id: WorkflowId) -> Result<Option<Workflow>, Error> {
    workflow::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    project_id: ProjectId,
    name: Option<&str>,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Workflow>, Error> {
    let mut query =
        workflow::Entity::find().filter(workflow::Column::ProjectId.eq(Uuid::from(project_id)));
    if let Some(name) = name {
        query = query.filter(workflow::Column::Name.eq(name));
    }
    if let Some(after) = after {
        query = query.filter(workflow::Column::Id.gt(after));
    }
    query
        .order_by_asc(workflow::Column::Id)
        .limit(limit)
        .all(db)
        .await?
        .into_iter()
        .map(decode)
        .collect()
}

pub async fn revision<C: ConnectionTrait>(
    db: &C,
    id: RevisionId,
) -> Result<Option<Revision>, Error> {
    workflow_revision::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode_revision)
        .transpose()
}

pub async fn revisions<C: ConnectionTrait>(
    db: &C,
    workflow_id: WorkflowId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Revision>, Error> {
    let mut query = workflow_revision::Entity::find()
        .filter(workflow_revision::Column::WorkflowId.eq(Uuid::from(workflow_id)));
    if let Some(after) = after {
        query = query.filter(workflow_revision::Column::Id.lt(after));
    }
    query
        .order_by_desc(workflow_revision::Column::Id)
        .limit(limit)
        .all(db)
        .await?
        .into_iter()
        .map(decode_revision)
        .collect()
}

async fn store<C: ConnectionTrait>(
    db: &C,
    workflow_id: Uuid,
    source: &str,
    sha256: Vec<u8>,
) -> Result<(workflow::Model, Saved), Error> {
    let inserted = workflow_revision::Entity::insert(workflow_revision::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        workflow_id: ActiveValue::Set(workflow_id),
        sha256: ActiveValue::Set(sha256.clone()),
        source: ActiveValue::Set(source.to_owned()),
        created_at: ActiveValue::NotSet,
    })
    .on_conflict(
        OnConflict::columns([
            workflow_revision::Column::WorkflowId,
            workflow_revision::Column::Sha256,
        ])
        .do_nothing()
        .to_owned(),
    )
    .do_nothing()
    .exec_without_returning(db)
    .await?;
    let revision = workflow_revision::Entity::find()
        .filter(workflow_revision::Column::WorkflowId.eq(workflow_id))
        .filter(workflow_revision::Column::Sha256.eq(sha256))
        .one(db)
        .await?
        .ok_or_else(|| Error::Vanished(format!("the revision of workflow {workflow_id}")))?;
    let current = workflow::ActiveModel {
        id: ActiveValue::Unchanged(workflow_id),
        revision_id: ActiveValue::Set(Some(revision.id)),
        ..Default::default()
    }
    .update(db)
    .await?;
    Ok((
        current,
        Saved {
            revision: decode_revision(revision)?,
            created: matches!(inserted, TryInsertResult::Inserted(rows) if rows > 0),
        },
    ))
}

fn decode(workflow: workflow::Model) -> Result<Workflow, Error> {
    Ok(Workflow {
        id: row::id(WORKFLOWS, workflow.id)?,
        project_id: row::id(WORKFLOWS, workflow.project_id)?,
        name: workflow.name,
        source: row::json::<WorkflowSource>(WORKFLOWS, json!({ "kind": workflow.source_kind }))?,
        enabled: workflow.enabled,
        revision_id: row::optional_id(WORKFLOWS, workflow.revision_id)?,
        created_at: workflow.created_at,
    })
}

fn decode_revision(revision: workflow_revision::Model) -> Result<Revision, Error> {
    Ok(Revision {
        id: row::id(REVISIONS, revision.id)?,
        workflow_id: row::id(REVISIONS, revision.workflow_id)?,
        sha256: row::hex(&revision.sha256),
        source: revision.source,
        created_at: revision.created_at,
    })
}
