use std::collections::BTreeMap;

use bosun_types::api::{Capability, Pool, PoolAccess, PoolOrganisation, PoolProject, TrustLevel};
use bosun_types::id::{OrganisationId, PoolId, ProjectId};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

use crate::aliases;
use crate::entity::pool;
use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "pool";

const ACCESS: &str = "pool_access";

const DEFAULT: &str = "default";

pub enum Access {
    AllProjects,
    Organisation(OrganisationId),
    Project(OrganisationId, ProjectId),
}

pub struct NewPool<'a> {
    pub name: &'a str,
    pub trust: TrustLevel,
    pub access: &'a [Access],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub labels: Vec<String>,
    pub pools: Vec<PoolId>,
    pub capabilities: Vec<Capability>,
}

pub(crate) fn admits(pool: &str, project: &str, organisation: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM pool_access pa WHERE pa.pool_id = {pool} \
         AND (pa.project_id = {project} OR (pa.project_id IS NULL \
         AND (pa.organisation_id IS NULL OR pa.organisation_id = {organisation}))))"
    )
}

pub async fn create<C: ConnectionTrait>(db: &C, new: NewPool<'_>) -> Result<Pool, Error> {
    let created = pool::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        name: ActiveValue::Set(new.name.to_owned()),
        trust: ActiveValue::Set(row::text(TABLE, new.trust)?),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::PoolTaken(new.name.to_owned())))?;
    for access in new.access {
        let (organisation, project) = match access {
            Access::AllProjects => (None, None),
            Access::Organisation(organisation) => (Some(Uuid::from(*organisation)), None),
            Access::Project(organisation, project) => {
                (Some(Uuid::from(*organisation)), Some(Uuid::from(*project)))
            }
        };
        db.execute(statement(
            "INSERT INTO pool_access (id, pool_id, organisation_id, project_id) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            [
                Uuid::now_v7().into(),
                created.id.into(),
                organisation.into(),
                project.into(),
            ],
        ))
        .await?;
    }
    one(db, created).await
}

pub async fn default_pool<C: ConnectionTrait>(db: &C) -> Result<Pool, Error> {
    named(db, DEFAULT)
        .await?
        .ok_or(Error::Missing("the pool `default`"))
}

pub async fn named<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<Pool>, Error> {
    match pool::Entity::find()
        .filter(pool::Column::Name.eq(name))
        .one(db)
        .await?
    {
        Some(found) => one(db, found).await.map(Some),
        None => Ok(None),
    }
}

pub async fn find<C: ConnectionTrait>(db: &C, id: PoolId) -> Result<Option<Pool>, Error> {
    match pool::Entity::find_by_id(Uuid::from(id)).one(db).await? {
        Some(found) => one(db, found).await.map(Some),
        None => Ok(None),
    }
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Pool>, Error> {
    let mut query = pool::Entity::find()
        .order_by_asc(pool::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(pool::Column::Id.gt(after));
    }
    decode(db, query.all(db).await?).await
}

pub async fn place<C: ConnectionTrait>(
    db: &C,
    organisation: OrganisationId,
    project: ProjectId,
    group: Option<&str>,
    labels: &[String],
) -> Result<Placement, Error> {
    let mut lowered: Vec<String> = Vec::with_capacity(labels.len());
    for label in labels {
        let label = label.to_lowercase();
        if !lowered.contains(&label) {
            lowered.push(label);
        }
    }
    let aliases = aliases::applying(db, organisation, &lowered).await?;
    let mut kept = Vec::new();
    let mut named: Vec<Uuid> = Vec::new();
    let mut capabilities = Vec::new();
    for label in lowered {
        let Some(alias) = aliases.get(&label) else {
            kept.push(label);
            continue;
        };
        if let Some(pool) = alias.pool
            && !named.contains(&pool)
        {
            named.push(pool);
        }
        if let Some(capability) = alias.capability
            && !capabilities.contains(&capability)
        {
            capabilities.push(capability);
        }
    }
    let open = db
        .query_all(statement(
            &format!(
                "SELECT p.id, p.name FROM pool p WHERE {} ORDER BY p.id",
                admits("p.id", "$2", "$1")
            ),
            [Uuid::from(organisation).into(), Uuid::from(project).into()],
        ))
        .await?;
    let mut pools = Vec::new();
    for found in open {
        let id: Uuid = found.try_get("", "id")?;
        let name: String = found.try_get("", "name")?;
        if group.is_none_or(|group| group == name) && named.iter().all(|pool| *pool == id) {
            pools.push(row::id(TABLE, id)?);
        }
    }
    Ok(Placement {
        labels: kept,
        pools,
        capabilities,
    })
}

async fn one<C: ConnectionTrait>(db: &C, found: pool::Model) -> Result<Pool, Error> {
    decode(db, vec![found])
        .await?
        .pop()
        .ok_or_else(|| Error::Vanished("a pool".to_owned()))
}

async fn decode<C: ConnectionTrait>(db: &C, pools: Vec<pool::Model>) -> Result<Vec<Pool>, Error> {
    let ids: Vec<Uuid> = pools.iter().map(|pool| pool.id).collect();
    let mut access: BTreeMap<Uuid, PoolAccess> = ids
        .iter()
        .map(|id| {
            (
                *id,
                PoolAccess {
                    all_projects: false,
                    organisations: Vec::new(),
                    projects: Vec::new(),
                },
            )
        })
        .collect();
    if !ids.is_empty() {
        let entries = db
            .query_all(statement(
                "SELECT pa.pool_id, pa.organisation_id, pa.project_id, o.slug AS organisation, \
                 p.slug AS project FROM pool_access pa \
                 LEFT JOIN organisation o ON o.id = pa.organisation_id \
                 LEFT JOIN project p ON p.id = pa.project_id \
                 WHERE pa.pool_id = ANY ($1) ORDER BY pa.organisation_id, pa.project_id",
                [ids.into()],
            ))
            .await?;
        for entry in entries {
            let pool: Uuid = entry.try_get("", "pool_id")?;
            let Some(list) = access.get_mut(&pool) else {
                continue;
            };
            let organisation: Option<Uuid> = entry.try_get("", "organisation_id")?;
            let project: Option<Uuid> = entry.try_get("", "project_id")?;
            let slug: Option<String> = entry.try_get("", "organisation")?;
            match (organisation, project) {
                (None, _) => list.all_projects = true,
                (Some(organisation), None) => list.organisations.push(PoolOrganisation {
                    id: row::id(ACCESS, organisation)?,
                    slug: slug.unwrap_or_default(),
                }),
                (Some(organisation), Some(project)) => {
                    let name: Option<String> = entry.try_get("", "project")?;
                    list.projects.push(PoolProject {
                        id: row::id(ACCESS, project)?,
                        organisation_id: row::id(ACCESS, organisation)?,
                        path: format!("{}/{}", slug.unwrap_or_default(), name.unwrap_or_default()),
                    });
                }
            }
        }
    }
    pools
        .into_iter()
        .map(|pool| {
            Ok(Pool {
                id: row::id(TABLE, pool.id)?,
                access: access.remove(&pool.id).ok_or_else(|| Error::Row {
                    table: TABLE,
                    message: format!("pool {} was listed twice", pool.id),
                })?,
                name: pool.name,
                trust: row::name(TABLE, pool.trust)?,
                created_at: pool.created_at,
            })
        })
        .collect()
}
