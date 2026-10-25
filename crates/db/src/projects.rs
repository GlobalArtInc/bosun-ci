use std::collections::HashMap;

use bosun_types::api::{Project, ProjectSettings, Repository};
use bosun_types::id::{ExecutionId, JobId, OrganisationId, ProjectId, RunId, WorkflowId};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect,
};

use crate::entity::{forge_repository, organisation, project};
use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "project";

pub struct Reach {
    pub everywhere: bool,
    pub organisations: Vec<OrganisationId>,
    pub projects: Vec<ProjectId>,
}

impl Reach {
    pub fn reaches(&self, project: &Project) -> bool {
        self.everywhere
            || self.projects.contains(&project.id)
            || self.organisations.contains(&project.organisation_id)
    }

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

pub struct Settings {
    pub version: u64,
    pub settings: ProjectSettings,
}

pub struct Binding {
    pub project: Project,
    pub installation: Option<u64>,
    pub github_workflows: bool,
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
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::ProjectTaken(slug.to_owned())))?;
    one(db, Some(created))
        .await?
        .ok_or_else(|| Error::Vanished(format!("project {slug}")))
}

pub async fn bind<C: ConnectionTrait>(
    db: &C,
    id: ProjectId,
    repository: Option<&Repository>,
) -> Result<Option<Project>, Error> {
    let repository_id = repository
        .map(|repository| row::signed(TABLE, repository.id))
        .transpose()?;
    db.execute(statement(
        "UPDATE project SET repository_id = $2 WHERE id = $1",
        [Uuid::from(id).into(), repository_id.into()],
    ))
    .await
    .map_err(|error| {
        row::taken(error, || {
            Error::RepositoryBound(
                repository
                    .map(|repository| format!("{}/{}", repository.owner, repository.name))
                    .unwrap_or_default(),
            )
        })
    })?;
    find(db, id).await
}

pub async fn find<C: ConnectionTrait>(db: &C, id: ProjectId) -> Result<Option<Project>, Error> {
    let found = project::Entity::find_by_id(Uuid::from(id)).one(db).await?;
    one(db, found).await
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
    let found = project::Entity::find()
        .filter(project::Column::OrganisationId.eq(owner.id))
        .filter(project::Column::Slug.eq(project))
        .one(db)
        .await?;
    one(db, found).await
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
    let found = query.all(db).await?;
    present(db, found).await
}

pub async fn bound_to<C: ConnectionTrait>(
    db: &C,
    repository: u64,
) -> Result<Option<Binding>, Error> {
    let found = project::Entity::find()
        .filter(project::Column::RepositoryId.eq(row::signed(TABLE, repository)?))
        .one(db)
        .await?;
    binding_of(db, found).await
}

pub async fn binding<C: ConnectionTrait>(db: &C, id: ProjectId) -> Result<Option<Binding>, Error> {
    let found = project::Entity::find_by_id(Uuid::from(id)).one(db).await?;
    binding_of(db, found).await
}

async fn binding_of<C: ConnectionTrait>(
    db: &C,
    found: Option<project::Model>,
) -> Result<Option<Binding>, Error> {
    let Some(found) = found else {
        return Ok(None);
    };
    let github_workflows = found.github_workflows;
    let installation = match found.repository_id {
        Some(repository) => forge_repository::Entity::find_by_id(repository)
            .one(db)
            .await?
            .and_then(|repository| repository.installation_id)
            .map(|installation| row::unsigned("forge_repository", installation))
            .transpose()?,
        None => None,
    };
    Ok(one(db, Some(found)).await?.map(|project| Binding {
        project,
        installation,
        github_workflows,
    }))
}

pub async fn settings<C: ConnectionTrait>(
    db: &C,
    id: ProjectId,
) -> Result<Option<Settings>, Error> {
    project::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode_settings)
        .transpose()
}

pub async fn change_settings<C: ConnectionTrait>(
    db: &C,
    id: ProjectId,
    version: u64,
    settings: &ProjectSettings,
) -> Result<Option<Settings>, Error> {
    let Ok(version) = i64::try_from(version) else {
        return Ok(None);
    };
    project::Entity::update_many()
        .col_expr(
            project::Column::GithubWorkflows,
            settings.github_workflows.into(),
        )
        .col_expr(
            project::Column::ForkPolicy,
            row::text(TABLE, settings.fork_policy)?.into(),
        )
        .col_expr(
            project::Column::TokenPermissions,
            serde_json::to_value(&settings.token_permissions)?.into(),
        )
        .col_expr(
            project::Column::Report,
            row::text(TABLE, settings.report)?.into(),
        )
        .col_expr(
            project::Column::SettingsVersion,
            Expr::col(project::Column::SettingsVersion).add(1),
        )
        .filter(project::Column::Id.eq(Uuid::from(id)))
        .filter(project::Column::SettingsVersion.eq(version))
        .exec_with_returning(db)
        .await?
        .into_iter()
        .next()
        .map(decode_settings)
        .transpose()
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
    let found = project::Entity::find()
        .from_raw_sql(statement(sql, [id.into()]))
        .one(db)
        .await?;
    one(db, found).await
}

async fn one<C: ConnectionTrait>(
    db: &C,
    found: Option<project::Model>,
) -> Result<Option<Project>, Error> {
    Ok(present(db, found.into_iter().collect()).await?.pop())
}

async fn present<C: ConnectionTrait>(
    db: &C,
    found: Vec<project::Model>,
) -> Result<Vec<Project>, Error> {
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = found
        .iter()
        .filter_map(|project| project.repository_id)
        .collect();
    let mut repositories = HashMap::new();
    if !ids.is_empty() {
        for repository in forge_repository::Entity::find()
            .filter(forge_repository::Column::Id.is_in(ids))
            .all(db)
            .await?
        {
            repositories.insert(repository.id, repository);
        }
    }
    let owners: HashMap<Uuid, String> = organisation::Entity::find()
        .filter(organisation::Column::Id.is_in(found.iter().map(|project| project.organisation_id)))
        .all(db)
        .await?
        .into_iter()
        .map(|owner| (owner.id, owner.slug))
        .collect();
    found
        .into_iter()
        .map(|project| {
            let repository = project
                .repository_id
                .and_then(|id| repositories.remove(&id));
            let owner = owners.get(&project.organisation_id).ok_or_else(|| {
                Error::Vanished(format!("the organisation of project {}", project.id))
            })?;
            let path = format!("{owner}/{}", project.slug);
            decode(project, path, repository)
        })
        .collect()
}

fn decode(
    project: project::Model,
    path: String,
    repository: Option<forge_repository::Model>,
) -> Result<Project, Error> {
    Ok(Project {
        id: row::id(TABLE, project.id)?,
        organisation_id: row::id(TABLE, project.organisation_id)?,
        slug: project.slug,
        path,
        name: project.name,
        repository: repository
            .map(|repository| {
                Ok::<_, Error>(Repository {
                    id: row::unsigned("forge_repository", repository.id)?,
                    owner: repository.owner,
                    name: repository.name,
                    default_branch: repository.default_branch,
                })
            })
            .transpose()?,
        created_at: project.created_at,
    })
}

fn decode_settings(project: project::Model) -> Result<Settings, Error> {
    Ok(Settings {
        version: row::unsigned(TABLE, project.settings_version)?,
        settings: ProjectSettings {
            github_workflows: project.github_workflows,
            fork_policy: row::name(TABLE, project.fork_policy)?,
            token_permissions: row::json(TABLE, project.token_permissions)?,
            report: row::name(TABLE, project.report)?,
        },
    })
}
