use bosun_types::api::Project;
use bosun_types::id::{OrganisationId, ProjectId};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};

use crate::entity::{organisation, project};
use crate::{Error, row};

const TABLE: &str = "project";

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

fn decode(project: project::Model) -> Result<Project, Error> {
    Ok(Project {
        id: row::id(TABLE, project.id)?,
        organisation_id: row::id(TABLE, project.organisation_id)?,
        slug: project.slug,
        name: project.name,
        created_at: project.created_at,
    })
}
