use bosun_types::api::{Installation, Organisation};
use bosun_types::id::OrganisationId;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};

use crate::entity::{forge_installation, organisation};
use crate::row::statement;
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
    decode(created, None)
}

pub async fn create_unless_taken<C: ConnectionTrait>(
    db: &C,
    slug: &str,
    name: &str,
) -> Result<Option<OrganisationId>, Error> {
    db.query_one(statement(
        "INSERT INTO organisation (id, slug, name) VALUES ($1, $2, $3) \
         ON CONFLICT (slug) DO NOTHING RETURNING id",
        [Uuid::now_v7().into(), slug.into(), name.into()],
    ))
    .await?
    .map(|created| row::id(TABLE, created.try_get::<Uuid>("", "id")?))
    .transpose()
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    id: OrganisationId,
) -> Result<Option<Organisation>, Error> {
    let found = organisation::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?;
    present(db, found).await
}

pub async fn find_by_slug<C: ConnectionTrait>(
    db: &C,
    slug: &str,
) -> Result<Option<Organisation>, Error> {
    let found = organisation::Entity::find()
        .filter(organisation::Column::Slug.eq(slug))
        .one(db)
        .await?;
    present(db, found).await
}

async fn present<C: ConnectionTrait>(
    db: &C,
    found: Option<organisation::Model>,
) -> Result<Option<Organisation>, Error> {
    let Some(found) = found else {
        return Ok(None);
    };
    let installation = forge_installation::Entity::find()
        .filter(forge_installation::Column::OrganisationId.eq(found.id))
        .one(db)
        .await?;
    decode(found, installation).map(Some)
}

fn decode(
    organisation: organisation::Model,
    installation: Option<forge_installation::Model>,
) -> Result<Organisation, Error> {
    Ok(Organisation {
        id: row::id(TABLE, organisation.id)?,
        slug: organisation.slug,
        name: organisation.name,
        installation: installation
            .map(|installation| {
                Ok::<_, Error>(Installation {
                    id: row::unsigned("forge_installation", installation.id)?,
                    account: installation.account_login,
                })
            })
            .transpose()?,
        created_at: organisation.created_at,
    })
}
