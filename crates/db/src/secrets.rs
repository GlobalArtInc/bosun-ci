use std::collections::{BTreeMap, BTreeSet};

use bosun_types::api::Secret;
use bosun_types::id::{OrganisationId, ProjectId, SecretId};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::Condition;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QueryResult, QuerySelect,
};

use crate::entity::secret;
use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "secret";

const COLUMNS: &str = "id, organisation_id, project_id, name, created_at, updated_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub organisation_id: OrganisationId,
    pub project_id: Option<ProjectId>,
}

impl Scope {
    pub(crate) fn condition(
        &self,
        organisation: impl ColumnTrait,
        project: impl ColumnTrait,
    ) -> Condition {
        let project = match self.project_id {
            Some(id) => project.eq(Uuid::from(id)),
            None => project.is_null(),
        };
        Condition::all()
            .add(organisation.eq(Uuid::from(self.organisation_id)))
            .add(project)
    }

    pub(crate) fn reach(
        organisation_id: OrganisationId,
        project_id: ProjectId,
        organisation: impl ColumnTrait,
        project: impl ColumnTrait,
    ) -> Condition {
        Condition::all()
            .add(organisation.eq(Uuid::from(organisation_id)))
            .add(
                Condition::any()
                    .add(project.is_null())
                    .add(project.eq(Uuid::from(project_id))),
            )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub data_key: Vec<u8>,
    pub kek_id: String,
}

pub struct Set {
    pub secret: Secret,
    pub created: bool,
}

pub struct Stored {
    pub id: SecretId,
    pub name: String,
    pub scope: Scope,
    pub sealed: Sealed,
}

pub async fn set<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    name: &str,
    sealed: &Sealed,
) -> Result<Set, Error> {
    let found = db
        .query_one(statement(
            &format!(
                "INSERT INTO secret (id, organisation_id, project_id, name, ciphertext, data_key, \
                 kek_id) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (organisation_id, project_id, name) DO UPDATE SET \
                 ciphertext = EXCLUDED.ciphertext, data_key = EXCLUDED.data_key, \
                 kek_id = EXCLUDED.kek_id, updated_at = now() \
                 RETURNING {COLUMNS}, xmax = 0 AS created"
            ),
            [
                Uuid::now_v7().into(),
                Uuid::from(scope.organisation_id).into(),
                scope.project_id.map(Uuid::from).into(),
                name.to_owned().into(),
                sealed.ciphertext.clone().into(),
                sealed.data_key.clone().into(),
                sealed.kek_id.clone().into(),
            ],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the secret `{name}`")))?;
    Ok(Set {
        created: found.try_get("", "created")?,
        secret: decode(&found)?,
    })
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Secret>, Error> {
    let mut query = secret::Entity::find()
        .filter(scope.condition(secret::Column::OrganisationId, secret::Column::ProjectId))
        .order_by_asc(secret::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(secret::Column::Id.gt(after));
    }
    query
        .all(db)
        .await?
        .into_iter()
        .map(|model| {
            Ok(Secret {
                id: row::id(TABLE, model.id)?,
                name: model.name,
                organisation_id: row::id("organisation", model.organisation_id)?,
                project_id: row::optional_id("project", model.project_id)?,
                created_at: model.created_at,
                updated_at: model.updated_at,
            })
        })
        .collect()
}

pub async fn delete<C: ConnectionTrait>(db: &C, scope: Scope, name: &str) -> Result<bool, Error> {
    let deleted = secret::Entity::delete_many()
        .filter(scope.condition(secret::Column::OrganisationId, secret::Column::ProjectId))
        .filter(secret::Column::Name.eq(name))
        .exec(db)
        .await?;
    Ok(deleted.rows_affected > 0)
}

pub async fn in_scope<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    project_id: ProjectId,
    names: Option<&BTreeSet<String>>,
) -> Result<Vec<Stored>, Error> {
    let mut query = secret::Entity::find().filter(Scope::reach(
        organisation_id,
        project_id,
        secret::Column::OrganisationId,
        secret::Column::ProjectId,
    ));
    if let Some(names) = names {
        query = query.filter(secret::Column::Name.is_in(names.iter().cloned()));
    }
    let mut found = query
        .all(db)
        .await?
        .into_iter()
        .map(stored)
        .collect::<Result<Vec<Stored>, Error>>()?;
    found.sort_by_key(|stored| stored.scope.project_id.is_some());
    let shadowed: BTreeMap<String, Stored> = found
        .into_iter()
        .map(|stored| (stored.name.clone(), stored))
        .collect();
    Ok(shadowed.into_values().collect())
}

pub async fn under_another_kek<C: ConnectionTrait>(
    db: &C,
    kek_id: &str,
    after: Option<SecretId>,
    limit: u64,
) -> Result<Vec<Stored>, Error> {
    let mut query = secret::Entity::find()
        .filter(secret::Column::KekId.ne(kek_id))
        .order_by_asc(secret::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(secret::Column::Id.gt(Uuid::from(after)));
    }
    query.all(db).await?.into_iter().map(stored).collect()
}

pub async fn rewrap<C: ConnectionTrait>(
    db: &C,
    stored: &Stored,
    data_key: Vec<u8>,
    kek_id: &str,
) -> Result<bool, Error> {
    let changed = db
        .execute(statement(
            "UPDATE secret SET data_key = $2, kek_id = $3 \
             WHERE id = $1 AND kek_id = $4 AND data_key = $5",
            [
                Uuid::from(stored.id).into(),
                data_key.into(),
                kek_id.to_owned().into(),
                stored.sealed.kek_id.clone().into(),
                stored.sealed.data_key.clone().into(),
            ],
        ))
        .await?;
    Ok(changed.rows_affected() == 1)
}

fn stored(model: secret::Model) -> Result<Stored, Error> {
    Ok(Stored {
        id: row::id(TABLE, model.id)?,
        name: model.name,
        scope: Scope {
            organisation_id: row::id("organisation", model.organisation_id)?,
            project_id: row::optional_id("project", model.project_id)?,
        },
        sealed: Sealed {
            ciphertext: model.ciphertext,
            data_key: model.data_key,
            kek_id: model.kek_id,
        },
    })
}

fn decode(found: &QueryResult) -> Result<Secret, Error> {
    Ok(Secret {
        id: row::id(TABLE, found.try_get("", "id")?)?,
        name: found.try_get("", "name")?,
        organisation_id: row::id("organisation", found.try_get("", "organisation_id")?)?,
        project_id: row::optional_id("project", found.try_get("", "project_id")?)?,
        created_at: found.try_get("", "created_at")?,
        updated_at: found.try_get("", "updated_at")?,
    })
}
