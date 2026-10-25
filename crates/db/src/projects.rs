use bosun_types::api::Project;
use bosun_types::id::{ExecutionId, JobId, OrganisationId, ProjectId, RunId, WorkflowId};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::Query;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect,
};

use crate::entity::{organisation, project};
use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "project";

pub struct Reach {
    pub everywhere: bool,
    pub organisations: Vec<OrganisationId>,
    pub projects: Vec<ProjectId>,
}

impl Reach {
    pub(crate) fn condition(&self, project: impl ColumnTrait) -> Option<Condition> {
        (!self.everywhere).then(|| {
            Condition::any()
                .add(project.is_in(self.projects.iter().copied().map(Uuid::from)))
                .add(
                    project.in_subquery(
                        Query::select()
                            .column(project::Column::Id)
                            .from(project::Entity)
                            .and_where(
                                project::Column::OrganisationId
                                    .is_in(self.organisations.iter().copied().map(Uuid::from)),
                            )
                            .to_owned(),
                    ),
                )
        })
    }
}

pub enum Holder {
    Workflow(WorkflowId),
    Run(RunId),
    Job(JobId),
    Execution(ExecutionId),
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    slug: &str,
    name: &str,
) -> Result<Project, Error> {
    let created = project::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        organisation_id: ActiveValue::Set(organisation_id.into()),
        slug: ActiveValue::Set(slug.to_owned()),
        name: ActiveValue::Set(name.to_owned()),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::ProjectTaken(slug.to_owned())))?;
    decode(created)
}

pub async fn find<C: ConnectionTrait>(db: &C, id: ProjectId) -> Result<Option<Project>, Error> {
    project::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn find_by_slugs<C: ConnectionTrait>(
    db: &C,
    organisation: &str,
    project: &str,
) -> Result<Option<Project>, Error> {
    let Some(owner) = organisation::Entity::find()
        .filter(organisation::Column::Slug.eq(organisation))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    project::Entity::find()
        .filter(project::Column::OrganisationId.eq(owner.id))
        .filter(project::Column::Slug.eq(project))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    reach: &Reach,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Project>, Error> {
    let mut query = project::Entity::find()
        .order_by_asc(project::Column::Id)
        .limit(limit);
    if let Some(condition) = reach.condition(project::Column::Id) {
        query = query.filter(condition);
    }
    if let Some(after) = after {
        query = query.filter(project::Column::Id.gt(after));
    }
    query.all(db).await?.into_iter().map(decode).collect()
}

pub async fn holding<C: ConnectionTrait>(db: &C, holder: Holder) -> Result<Option<Project>, Error> {
    let (sql, id) = match holder {
        Holder::Workflow(id) => (
            "SELECT p.* FROM project p JOIN workflow w ON w.project_id = p.id WHERE w.id = $1",
            Uuid::from(id),
        ),
        Holder::Run(id) => (
            "SELECT p.* FROM project p JOIN run r ON r.project_id = p.id WHERE r.id = $1",
            Uuid::from(id),
        ),
        Holder::Job(id) => (
            "SELECT p.* FROM project p JOIN run r ON r.project_id = p.id \
             JOIN attempt a ON a.run_id = r.id JOIN job j ON j.attempt_id = a.id WHERE j.id = $1",
            Uuid::from(id),
        ),
        Holder::Execution(id) => (
            "SELECT p.* FROM project p JOIN run r ON r.project_id = p.id \
             JOIN attempt a ON a.run_id = r.id JOIN job j ON j.attempt_id = a.id \
             JOIN execution e ON e.job_id = j.id WHERE e.id = $1",
            Uuid::from(id),
        ),
    };
    project::Entity::find()
        .from_raw_sql(statement(sql, [id.into()]))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

fn decode(project: project::Model) -> Result<Project, Error> {
    Ok(Project {
        id: row::id(TABLE, project.id)?,
        organisation_id: row::id(TABLE, project.organisation_id)?,
        slug: project.slug,
        name: project.name,
        created_at: project.created_at,
    })
}
