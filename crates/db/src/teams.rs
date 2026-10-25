use bosun_types::id::{OrganisationId, TeamId, UserId};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};

use crate::entity::{team, team_member};
use crate::{Error, row};

pub async fn create<C: ConnectionTrait>(
    db: &C,
    organisation_id: OrganisationId,
    slug: &str,
    name: &str,
) -> Result<TeamId, Error> {
    let created = team::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        organisation_id: ActiveValue::Set(organisation_id.into()),
        slug: ActiveValue::Set(slug.to_owned()),
        name: ActiveValue::Set(name.to_owned()),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    row::id("team", created.id)
}

pub async fn add_member<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    user: UserId,
) -> Result<(), Error> {
    team_member::ActiveModel {
        team_id: ActiveValue::Set(team.into()),
        user_id: ActiveValue::Set(user.into()),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    Ok(())
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
