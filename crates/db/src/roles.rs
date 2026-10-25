use std::collections::{BTreeMap, BTreeSet};

use bosun_types::api::{BoundRole, BoundTeam, BoundUser, ProjectRef, Role, RoleBinding};
use bosun_types::id::{OrganisationId, ProjectId, RoleBindingId, RoleId, TeamId, UserId};
use bosun_types::permission::Permission;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, EntityTrait,
    QueryFilter, QueryOrder, QueryResult, QuerySelect,
};

use crate::entity::{role, role_binding};
use crate::row::statement;
use crate::{Error, row};

const TABLE: &str = "role";

const BINDING: &str = "role_binding";

const BINDINGS: &str = "SELECT b.id, b.organisation_id, b.project_id, b.user_id, b.team_id, \
                        b.created_at, r.id AS role_id, r.name AS role_name, u.login, \
                        t.slug AS team_slug, o.slug AS organisation_slug, \
                        p.slug AS project_slug FROM role_binding b \
                        JOIN role r ON r.id = b.role_id \
                        JOIN organisation o ON o.id = b.organisation_id \
                        LEFT JOIN \"user\" u ON u.id = b.user_id \
                        LEFT JOIN team t ON t.id = b.team_id \
                        LEFT JOIN project p ON p.id = b.project_id";

pub enum Subject {
    User(UserId),
    Team(TeamId),
}

pub struct NewBinding {
    pub role_id: RoleId,
    pub subject: Subject,
    pub organisation_id: OrganisationId,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Default)]
pub struct Grants {
    pub organisations: BTreeMap<OrganisationId, BTreeSet<Permission>>,
    pub projects: BTreeMap<ProjectId, ProjectGrant>,
}

#[derive(Debug)]
pub struct ProjectGrant {
    pub organisation_id: OrganisationId,
    pub permissions: BTreeSet<Permission>,
}

pub async fn builtin<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<RoleId>, Error> {
    role::Entity::find()
        .filter(role::Column::OrganisationId.is_null())
        .filter(role::Column::Name.eq(name))
        .one(db)
        .await?
        .map(|found| row::id(TABLE, found.id))
        .transpose()
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    name: &str,
    permissions: &BTreeSet<Permission>,
) -> Result<Role, Error> {
    let created = role::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        organisation_id: ActiveValue::Set(Some(organisation_id.into())),
        name: ActiveValue::Set(name.to_owned()),
        permissions: ActiveValue::Set(keys(permissions)),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::RoleTaken(name.to_owned())))?;
    decode(created)
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Role>, Error> {
    let mut query = role::Entity::find()
        .filter(within(organisation_id))
        .order_by_asc(role::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(role::Column::Id.gt(after));
    }
    query.all(db).await?.into_iter().map(decode).collect()
}

pub async fn find<C: ConnectionTrait>(db: &C, id: RoleId) -> Result<Option<Role>, Error> {
    role::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn named<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    name: &str,
) -> Result<Option<Role>, Error> {
    role::Entity::find()
        .filter(within(organisation_id))
        .filter(role::Column::Name.eq(name))
        .order_by_asc(role::Column::Id)
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn change<C: ConnectionTrait>(
    db: &C,
    id: RoleId,
    name: &str,
    permissions: &BTreeSet<Permission>,
) -> Result<Option<Role>, Error> {
    role::Entity::update_many()
        .col_expr(role::Column::Name, name.into())
        .col_expr(role::Column::Permissions, keys(permissions).into())
        .filter(role::Column::Id.eq(Uuid::from(id)))
        .filter(role::Column::OrganisationId.is_not_null())
        .exec_with_returning(db)
        .await
        .map_err(|error| row::taken(error, || Error::RoleTaken(name.to_owned())))?
        .into_iter()
        .next()
        .map(decode)
        .transpose()
}

pub async fn delete<C: ConnectionTrait>(db: &C, id: RoleId) -> Result<bool, Error> {
    let deleted = role::Entity::delete_many()
        .filter(role::Column::Id.eq(Uuid::from(id)))
        .filter(role::Column::OrganisationId.is_not_null())
        .exec(db)
        .await?;
    Ok(deleted.rows_affected > 0)
}

pub async fn bind<C: ConnectionTrait>(db: &C, binding: NewBinding) -> Result<RoleBinding, Error> {
    let (user_id, team_id) = match binding.subject {
        Subject::User(user) => (Some(user.into()), None),
        Subject::Team(team) => (None, Some(team.into())),
    };
    let created = role_binding::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        role_id: ActiveValue::Set(binding.role_id.into()),
        organisation_id: ActiveValue::Set(binding.organisation_id.into()),
        project_id: ActiveValue::Set(binding.project_id.map(Uuid::from)),
        user_id: ActiveValue::Set(user_id),
        team_id: ActiveValue::Set(team_id),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::BindingTaken))?;
    find_binding(db, row::id(BINDING, created.id)?)
        .await?
        .ok_or_else(|| Error::Vanished(format!("the role binding {}", created.id)))
}

pub async fn bindings<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<RoleBinding>, Error> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    db.query_all(statement(
        &format!(
            "{BINDINGS} WHERE b.organisation_id = $1 AND ($2::uuid IS NULL OR b.id > $2) \
             ORDER BY b.id LIMIT $3"
        ),
        [
            Uuid::from(organisation_id).into(),
            after.into(),
            limit.into(),
        ],
    ))
    .await?
    .iter()
    .map(decode_binding)
    .collect()
}

pub async fn find_binding<C: ConnectionTrait>(
    db: &C,
    id: RoleBindingId,
) -> Result<Option<RoleBinding>, Error> {
    db.query_one(statement(
        &format!("{BINDINGS} WHERE b.id = $1"),
        [Uuid::from(id).into()],
    ))
    .await?
    .as_ref()
    .map(decode_binding)
    .transpose()
}

pub async fn unbind<C: ConnectionTrait>(db: &C, id: RoleBindingId) -> Result<bool, Error> {
    let deleted = role_binding::Entity::delete_by_id(Uuid::from(id))
        .exec(db)
        .await?;
    Ok(deleted.rows_affected > 0)
}

pub async fn grants<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Grants, Error> {
    let rows = db
        .query_all(statement(
            "SELECT b.organisation_id, b.project_id, r.permissions FROM role_binding b \
             JOIN role r ON r.id = b.role_id \
             AND (r.organisation_id IS NULL OR r.organisation_id = b.organisation_id) \
             WHERE b.user_id = $1 \
             OR b.team_id IN (SELECT team_id FROM team_member WHERE user_id = $1)",
            [Uuid::from(user).into()],
        ))
        .await?;
    let mut grants = Grants::default();
    for found in rows {
        let stored: Vec<String> = found.try_get("", "permissions")?;
        let granted = known(&stored);
        if granted.is_empty() {
            continue;
        }
        let organisation_id = row::id(BINDING, found.try_get::<Uuid>("", "organisation_id")?)?;
        match found.try_get::<Option<Uuid>>("", "project_id")? {
            Some(project) => grants
                .projects
                .entry(row::id(BINDING, project)?)
                .or_insert_with(|| ProjectGrant {
                    organisation_id,
                    permissions: BTreeSet::new(),
                })
                .permissions
                .extend(granted),
            None => grants
                .organisations
                .entry(organisation_id)
                .or_default()
                .extend(granted),
        }
    }
    Ok(grants)
}

pub(crate) fn known(stored: &[String]) -> BTreeSet<Permission> {
    stored
        .iter()
        .filter_map(|key| Permission::from_key(key))
        .collect()
}

pub(crate) fn keys(permissions: &BTreeSet<Permission>) -> Vec<String> {
    permissions
        .iter()
        .map(|permission| permission.key().to_owned())
        .collect()
}

fn within(organisation_id: OrganisationId) -> Condition {
    Condition::any()
        .add(role::Column::OrganisationId.is_null())
        .add(role::Column::OrganisationId.eq(Uuid::from(organisation_id)))
}

fn decode(role: role::Model) -> Result<Role, Error> {
    Ok(Role {
        id: row::id(TABLE, role.id)?,
        organisation_id: row::optional_id(TABLE, role.organisation_id)?,
        permissions: known(&role.permissions),
        name: role.name,
        created_at: role.created_at,
    })
}

fn decode_binding(found: &QueryResult) -> Result<RoleBinding, Error> {
    let user = match found.try_get::<Option<Uuid>>("", "user_id")? {
        Some(id) => Some(BoundUser {
            id: row::id(BINDING, id)?,
            login: found.try_get("", "login")?,
        }),
        None => None,
    };
    let team = match found.try_get::<Option<Uuid>>("", "team_id")? {
        Some(id) => Some(BoundTeam {
            id: row::id(BINDING, id)?,
            slug: found.try_get("", "team_slug")?,
        }),
        None => None,
    };
    let organisation_id: Uuid = found.try_get("", "organisation_id")?;
    let project = match found.try_get::<Option<Uuid>>("", "project_id")? {
        Some(id) => Some(ProjectRef {
            id: row::id(BINDING, id)?,
            organisation_id: row::id(BINDING, organisation_id)?,
            path: format!(
                "{}/{}",
                found.try_get::<String>("", "organisation_slug")?,
                found.try_get::<String>("", "project_slug")?
            ),
        }),
        None => None,
    };
    Ok(RoleBinding {
        id: row::id(BINDING, found.try_get("", "id")?)?,
        role: BoundRole {
            id: row::id(BINDING, found.try_get("", "role_id")?)?,
            name: found.try_get("", "role_name")?,
        },
        user,
        team,
        organisation_id: row::id(BINDING, organisation_id)?,
        project,
        created_at: found.try_get("", "created_at")?,
    })
}
