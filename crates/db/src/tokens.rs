use bosun_types::api::User;
use bosun_types::id::TokenId;
use sea_orm::prelude::DateTimeUtc;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};
use uuid::Uuid;

use crate::entity::{api_token, user};
use crate::{Error, row, users};

pub struct NewToken {
    pub name: String,
    pub sha256: Vec<u8>,
    pub expires_at: DateTimeUtc,
}

pub struct PersonalToken {
    pub id: TokenId,
    pub user: User,
    pub password_change_required: bool,
    pub expires_at: DateTimeUtc,
    pub revoked_at: Option<DateTimeUtc>,
}

pub async fn find_personal<C: ConnectionTrait>(
    db: &C,
    sha256: &[u8],
) -> Result<Option<PersonalToken>, Error> {
    let found = api_token::Entity::find()
        .filter(api_token::Column::Sha256.eq(sha256.to_vec()))
        .find_also_related(user::Entity)
        .one(db)
        .await?;
    let Some((token, Some(user))) = found else {
        return Ok(None);
    };
    Ok(Some(PersonalToken {
        id: row::id("api_token", token.id)?,
        password_change_required: user.password_change_required,
        user: users::decode(user)?,
        expires_at: token.expires_at,
        revoked_at: token.revoked_at,
    }))
}

pub(crate) async fn insert<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    token: NewToken,
) -> Result<(), Error> {
    api_token::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        user_id: ActiveValue::Set(user_id),
        name: ActiveValue::Set(token.name),
        sha256: ActiveValue::Set(token.sha256),
        created_at: ActiveValue::NotSet,
        expires_at: ActiveValue::Set(token.expires_at),
        revoked_at: ActiveValue::Set(None),
    }
    .insert(db)
    .await?;
    Ok(())
}
