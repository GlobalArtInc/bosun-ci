use bosun_types::api::User;
use sea_orm::prelude::DateTimeUtc;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
};
use uuid::Uuid;

use crate::entity::{user, user_identity};
use crate::row::statement;
use crate::{Error, row, users};

pub struct NewIdentity<'a> {
    pub issuer: &'a str,
    pub subject: &'a str,
    pub login: &'a str,
    pub name: &'a str,
    pub email: Option<&'a str>,
}

pub async fn find_user<C: ConnectionTrait>(
    db: &C,
    issuer: &str,
    subject: &str,
) -> Result<Option<User>, Error> {
    let found = user_identity::Entity::find()
        .filter(user_identity::Column::Issuer.eq(issuer))
        .filter(user_identity::Column::Subject.eq(subject))
        .find_also_related(user::Entity)
        .one(db)
        .await?;
    match found {
        Some((_, Some(user))) => users::decode(user).map(Some),
        _ => Ok(None),
    }
}

pub async fn create_user<C: ConnectionTrait>(db: &C, new: NewIdentity<'_>) -> Result<User, Error> {
    let created = user::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        login: ActiveValue::Set(new.login.to_owned()),
        name: ActiveValue::Set(new.name.to_owned()),
        email: ActiveValue::Set(new.email.map(str::to_owned)),
        owner: ActiveValue::Set(false),
        created_at: ActiveValue::NotSet,
        password_hash: ActiveValue::Set(None),
        password_change_required: ActiveValue::Set(false),
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::LoginTaken(new.login.to_owned())))?;
    user_identity::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        user_id: ActiveValue::Set(created.id),
        issuer: ActiveValue::Set(new.issuer.to_owned()),
        subject: ActiveValue::Set(new.subject.to_owned()),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await?;
    users::decode(created)
}

pub async fn spend_state<C: ConnectionTrait>(
    db: &C,
    sha256: &[u8],
    expires_at: DateTimeUtc,
) -> Result<bool, Error> {
    let spent = db
        .execute(statement(
            "INSERT INTO oidc_state (sha256, expires_at) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            [sha256.to_vec().into(), expires_at.into()],
        ))
        .await?;
    Ok(spent.rows_affected() > 0)
}
