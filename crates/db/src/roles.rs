use std::collections::{BTreeMap, BTreeSet};

use bosun_types::api::Permissions;
use bosun_types::id::{OrganisationId, ProjectId, RoleBindingId, RoleId, TeamId, UserId};
use bosun_types::permission::Permission;
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};

use crate::entity::{role, role_binding};
use crate::row::statement;
use crate::{Error, row};

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

pub async fn builtin<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<RoleId>, Error> {
    role::Entity::find()
        .filter(role::Column::OrganisationId.is_null())
        .filter(role::Column::Name.eq(name))
        .one(db)
        .await?
        .map(|found| row::id("role", found.id))
        .transpose()
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    name: &str,
    permissions: &BTreeSet<Permission>,
) -> Result<RoleId, Error> {
    let created = role::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        organisation_id: ActiveValue::Set(Some(organisation_id.into())),
        name: ActiveValue::Set(name.to_owned()),
        permissions: ActiveValue::Set(
            permissions
                .iter()
                .map(|permission| permission.key().to_owned())
                .collect(),
        ),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    row::id("role", created.id)
}

pub async fn bind<C: ConnectionTrait>(db: &C, binding: NewBinding) -> Result<RoleBindingId, Error> {
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
    .await?;
    row::id("role_binding", created.id)
}

pub async fn permissions<C: ConnectionTrait>(db: &C, user: UserId) -> Result<Permissions, Error> {
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
    let mut organisations: BTreeMap<OrganisationId, BTreeSet<Permission>> = BTreeMap::new();
    let mut projects: BTreeMap<ProjectId, BTreeSet<Permission>> = BTreeMap::new();
    for found in rows {
        let keys: Vec<String> = found.try_get("", "permissions")?;
        let granted: BTreeSet<Permission> = keys
            .iter()
            .filter_map(|key| Permission::from_key(key))
            .collect();
        if granted.is_empty() {
            continue;
        }
        match found.try_get::<Option<Uuid>>("", "project_id")? {
            Some(project) => projects
                .entry(row::id("role_binding", project)?)
                .or_default()
                .extend(granted),
            None => organisations
                .entry(row::id(
                    "role_binding",
                    found.try_get::<Uuid>("", "organisation_id")?,
                )?)
                .or_default()
                .extend(granted),
        }
    }
    Ok(Permissions {
        installation: BTreeSet::new(),
        organisations,
        projects,
    })
}
