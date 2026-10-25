use std::collections::HashMap;

use bosun_types::api::{Capability, LabelAlias};
use bosun_types::id::{OrganisationId, PoolId};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, QueryResult};

use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "label_alias";

const COLUMNS: &str =
    "id, organisation_id, label, pool_id, capability, image, created_at, updated_at";

pub struct NewAlias<'a> {
    pub organisation: Option<OrganisationId>,
    pub label: &'a str,
    pub pool: Option<PoolId>,
    pub capability: Option<Capability>,
    pub image: Option<&'a str>,
}

pub struct Set {
    pub alias: LabelAlias,
    pub created: bool,
}

pub(crate) struct Applying {
    pub(crate) pool: Option<Uuid>,
    pub(crate) capability: Option<Capability>,
}

pub async fn set<C: ConnectionTrait>(db: &C, new: NewAlias<'_>) -> Result<Set, Error> {
    let found = db
        .query_one(statement(
            &format!(
                "INSERT INTO label_alias (id, organisation_id, label, pool_id, capability, image) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (organisation_id, label) DO UPDATE SET pool_id = EXCLUDED.pool_id, \
                 capability = EXCLUDED.capability, image = EXCLUDED.image, updated_at = now() \
                 RETURNING {COLUMNS}, xmax = 0 AS created"
            ),
            [
                Uuid::now_v7().into(),
                new.organisation.map(Uuid::from).into(),
                new.label.to_owned().into(),
                new.pool.map(Uuid::from).into(),
                new.capability
                    .map(|capability| row::text(TABLE, capability))
                    .transpose()?
                    .into(),
                new.image.map(str::to_owned).into(),
            ],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the alias of `{}`", new.label)))?;
    Ok(Set {
        created: found.try_get("", "created")?,
        alias: decode(&found)?,
    })
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<LabelAlias>, Error> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.query_all(statement(
        &format!(
            "SELECT {COLUMNS} FROM label_alias WHERE $1::uuid IS NULL OR id > $1 \
             ORDER BY id LIMIT $2"
        ),
        [after.into(), limit.into()],
    ))
    .await?
    .iter()
    .map(decode)
    .collect()
}

pub(crate) async fn applying<C: ConnectionTrait>(
    db: &C,
    organisation: OrganisationId,
    labels: &[String],
) -> Result<HashMap<String, Applying>, Error> {
    let mut applying = HashMap::new();
    if labels.is_empty() {
        return Ok(applying);
    }
    let found = db
        .query_all(statement(
            "SELECT label, pool_id, capability FROM label_alias \
             WHERE label = ANY ($1) AND (organisation_id IS NULL OR organisation_id = $2) \
             ORDER BY organisation_id NULLS FIRST",
            [labels.to_vec().into(), Uuid::from(organisation).into()],
        ))
        .await?;
    for alias in found {
        let label: String = alias.try_get("", "label")?;
        let capability = alias
            .try_get::<Option<String>>("", "capability")?
            .map(|capability| row::name(TABLE, capability))
            .transpose()?;
        applying.insert(
            label,
            Applying {
                pool: alias.try_get("", "pool_id")?,
                capability,
            },
        );
    }
    Ok(applying)
}

fn decode(found: &QueryResult) -> Result<LabelAlias, Error> {
    Ok(LabelAlias {
        id: row::id(TABLE, found.try_get("", "id")?)?,
        label: found.try_get("", "label")?,
        organisation_id: row::optional_id(TABLE, found.try_get("", "organisation_id")?)?,
        pool_id: row::optional_id(TABLE, found.try_get("", "pool_id")?)?,
        capability: found
            .try_get::<Option<String>>("", "capability")?
            .map(|capability| row::name(TABLE, capability))
            .transpose()?,
        image: found.try_get("", "image")?,
        created_at: found.try_get("", "created_at")?,
        updated_at: found.try_get("", "updated_at")?,
    })
}
