use bosun_types::api::Organisation;
use bosun_types::id::OrganisationId;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};

use crate::entity::organisation;
use crate::{Error, row};

const TABLE: &str = "organisation";

pub async fn create<C: ConnectionTrait>(
    db: &C,
    slug: &str,
    name: &str,
) -> Result<Organisation, Error> {
    let created = organisation::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        slug: ActiveValue::Set(slug.to_owned()),
        name: ActiveValue::Set(name.to_owned()),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::OrganisationTaken(slug.to_owned())))?;
    decode(created)
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    id: OrganisationId,
) -> Result<Option<Organisation>, Error> {
    organisation::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn find_by_slug<C: ConnectionTrait>(
    db: &C,
    slug: &str,
) -> Result<Option<Organisation>, Error> {
    organisation::Entity::find()
        .filter(organisation::Column::Slug.eq(slug))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

fn decode(organisation: organisation::Model) -> Result<Organisation, Error> {
    Ok(Organisation {
        id: row::id(TABLE, organisation.id)?,
        slug: organisation.slug,
        name: organisation.name,
        created_at: organisation.created_at,
    })
}
