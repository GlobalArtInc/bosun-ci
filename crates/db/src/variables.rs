use std::collections::BTreeMap;

use bosun_types::api::Variable;
use bosun_types::id::{OrganisationId, ProjectId};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QueryResult, QuerySelect,
};

use crate::entity::variable;
use crate::row::statement;
use crate::secrets::Scope;
use crate::{Error, row};

const TABLE: &str = "variable";

const COLUMNS: &str = "id, organisation_id, project_id, name, value, created_at, updated_at";

pub struct Set {
    pub variable: Variable,
    pub created: bool,
}

pub async fn set<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    name: &str,
    value: &str,
) -> Result<Set, Error> {
    let found = db
        .query_one(statement(
            &format!(
                "INSERT INTO variable (id, organisation_id, project_id, name, value) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (organisation_id, project_id, name) DO UPDATE SET \
                 value = EXCLUDED.value, updated_at = now() \
                 RETURNING {COLUMNS}, xmax = 0 AS created"
            ),
            [
                Uuid::now_v7().into(),
                Uuid::from(scope.organisation_id).into(),
                scope.project_id.map(Uuid::from).into(),
                name.to_owned().into(),
                value.to_owned().into(),
            ],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the variable `{name}`")))?;
    Ok(Set {
        created: found.try_get("", "created")?,
        variable: decode(&found)?,
    })
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Variable>, Error> {
    let mut query = variable::Entity::find()
        .filter(scope.condition(
            variable::Column::OrganisationId,
            variable::Column::ProjectId,
        ))
        .order_by_asc(variable::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(variable::Column::Id.gt(after));
    }
    query.all(db).await?.into_iter().map(model).collect()
}

pub async fn delete<C: ConnectionTrait>(db: &C, scope: Scope, name: &str) -> Result<bool, Error> {
    let deleted = variable::Entity::delete_many()
        .filter(scope.condition(
            variable::Column::OrganisationId,
            variable::Column::ProjectId,
        ))
        .filter(variable::Column::Name.eq(name))
        .exec(db)
        .await?;
    Ok(deleted.rows_affected > 0)
}

pub async fn in_scope<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    project_id: ProjectId,
) -> Result<BTreeMap<String, String>, Error> {
    let mut found = variable::Entity::find()
        .filter(Scope::reach(
            organisation_id,
            project_id,
            variable::Column::OrganisationId,
            variable::Column::ProjectId,
        ))
        .all(db)
        .await?;
    found.sort_by_key(|model| model.project_id.is_some());
    Ok(found
        .into_iter()
        .map(|model| (model.name, model.value))
        .collect())
}

fn model(model: variable::Model) -> Result<Variable, Error> {
    Ok(Variable {
        id: row::id(TABLE, model.id)?,
        name: model.name,
        value: model.value,
        organisation_id: row::id("organisation", model.organisation_id)?,
        project_id: row::optional_id("project", model.project_id)?,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn decode(found: &QueryResult) -> Result<Variable, Error> {
    Ok(Variable {
        id: row::id(TABLE, found.try_get("", "id")?)?,
        name: found.try_get("", "name")?,
        value: found.try_get("", "value")?,
        organisation_id: row::id("organisation", found.try_get("", "organisation_id")?)?,
        project_id: row::optional_id("project", found.try_get("", "project_id")?)?,
        created_at: found.try_get("", "created_at")?,
        updated_at: found.try_get("", "updated_at")?,
    })
}
