use bosun_types::api::User;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectionTrait, EntityTrait, PaginatorTrait, SqlErr,
    TransactionTrait,
};
use uuid::Uuid;

use crate::entity::user;
use crate::tokens::{self, NewToken};
use crate::{Error, row};

const TABLE: &str = "user";

const BOOTSTRAP_LOCK: i64 = i64::from_be_bytes(*b"bosunown");

pub async fn create_owner<C: TransactionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
) -> Result<User, Error> {
    let transaction = db.begin().await?;
    let owner = insert_owner(&transaction, login, token).await?;
    transaction.commit().await?;
    Ok(owner)
}

pub async fn bootstrap_owner<C: TransactionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
) -> Result<Option<User>, Error> {
    let transaction = db.begin().await?;
    transaction
        .execute_unprepared(&format!("SELECT pg_advisory_xact_lock({BOOTSTRAP_LOCK})"))
        .await?;
    if user::Entity::find().count(&transaction).await? > 0 {
        return Ok(None);
    }
    let owner = insert_owner(&transaction, login, token).await?;
    transaction.commit().await?;
    Ok(Some(owner))
}

async fn insert_owner<C: ConnectionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
) -> Result<User, Error> {
    let owner = user::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        login: ActiveValue::Set(login.to_owned()),
        name: ActiveValue::Set(login.to_owned()),
        email: ActiveValue::Set(None),
        owner: ActiveValue::Set(true),
        created_at: ActiveValue::NotSet,
    }
    .insert(db)
    .await
    .map_err(|error| match error.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => Error::LoginTaken(login.to_owned()),
        _ => Error::Database(error),
    })?;
    tokens::insert(db, owner.id, token).await?;
    decode(owner)
}

pub(crate) fn decode(user: user::Model) -> Result<User, Error> {
    Ok(User {
        id: row::id(TABLE, user.id)?,
        login: user.login,
        name: user.name,
        email: user.email,
        owner: user.owner,
        created_at: user.created_at,
    })
}
