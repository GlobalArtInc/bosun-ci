use bosun_types::api::{Team, User};
use bosun_types::id::{OrganisationId, TeamId, UserId};
use sea_orm::prelude::Uuid;
use sea_orm::sea_query::Query;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

use crate::entity::{team, team_member, user};
use crate::row::statement;
use crate::{Error, row, users};

const TABLE: &str = "team";

pub async fn create<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    slug: &str,
    name: &str,
) -> Result<Team, Error> {
    let created = team::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        organisation_id: ActiveValue::Set(organisation_id.into()),
        slug: ActiveValue::Set(slug.to_owned()),
        name: ActiveValue::Set(name.to_owned()),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::TeamTaken(slug.to_owned())))?;
    decode(created)
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Team>, Error> {
    let mut query = team::Entity::find()
        .filter(team::Column::OrganisationId.eq(Uuid::from(organisation_id)))
        .order_by_asc(team::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(team::Column::Id.gt(after));
    }
    query.all(db).await?.into_iter().map(decode).collect()
}

pub async fn find<C: ConnectionTrait>(db: &C, id: TeamId) -> Result<Option<Team>, Error> {
    team::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn find_by_slug<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    slug: &str,
) -> Result<Option<Team>, Error> {
    team::Entity::find()
        .filter(team::Column::OrganisationId.eq(Uuid::from(organisation_id)))
        .filter(team::Column::Slug.eq(slug))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn delete<C: ConnectionTrait>(db: &C, id: TeamId) -> Result<bool, Error> {
    let deleted = team::Entity::delete_by_id(Uuid::from(id)).exec(db).await?;
    Ok(deleted.rows_affected > 0)
}

pub async fn members<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<User>, Error> {
    let mut query = user::Entity::find()
        .filter(
            user::Column::Id.in_subquery(
                Query::select()
                    .column(team_member::Column::UserId)
                    .from(team_member::Entity)
                    .and_where(team_member::Column::TeamId.eq(Uuid::from(team)))
                    .to_owned(),
            ),
        )
        .order_by_asc(user::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(user::Column::Id.gt(after));
    }
    query
        .all(db)
        .await?
        .into_iter()
        .map(users::decode)
        .collect()
}

pub async fn add_member<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<bool, Error> {
    let added = db
        .execute(statement(
            "INSERT INTO team_member (team_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            [Uuid::from(team).into(), Uuid::from(user).into()],
        ))
        .await?;
    Ok(added.rows_affected() > 0)
}

pub async fn remove_member<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<bool, Error> {
    let removed = team_member::Entity::delete_many()
        .filter(team_member::Column::TeamId.eq(Uuid::from(team)))
        .filter(team_member::Column::UserId.eq(Uuid::from(user)))
        .exec(db)
        .await?;
    Ok(removed.rows_affected > 0)
}

fn decode(team: team::Model) -> Result<Team, Error> {
    Ok(Team {
        id: row::id(TABLE, team.id)?,
        organisation_id: row::id(TABLE, team.organisation_id)?,
        slug: team.slug,
        name: team.name,
        created_at: team.created_at,
    })
}
