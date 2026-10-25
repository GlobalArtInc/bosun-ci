use bosun_types::api::User;
use bosun_types::id::UserId;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, SqlErr, TransactionTrait,
};
use uuid::Uuid;

use crate::entity::user;
use crate::tokens::{self, NewToken};
use crate::{Error, row};

const TABLE: &str = "user";

const BOOTSTRAP_LOCK: i64 = i64::from_be_bytes(*b"bosunown");

pub struct NewUser<'a> {
    pub login: &'a str,
    pub name: &'a str,
    pub email: &'a str,
    pub password_hash: String,
}

pub struct Credentials {
    pub user: User,
    pub password_hash: Option<String>,
    pub password_change_required: bool,
}

pub async fn create_owner<C: TransactionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
) -> Result<User, Error> {
    let transaction = db.begin().await?;
    let owner = insert_owner(&transaction, login, token, None).await?;
    transaction.commit().await?;
    Ok(owner)
}

pub async fn create<C: ConnectionTrait>(db: &C, login: &str) -> Result<User, Error> {
    decode(insert(db, login, false, None).await?)
}

pub async fn create_local<C: ConnectionTrait>(db: &C, new: NewUser<'_>) -> Result<User, Error> {
    let created = user::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        login: ActiveValue::Set(new.login.to_owned()),
        name: ActiveValue::Set(new.name.to_owned()),
        email: ActiveValue::Set(Some(new.email.to_owned())),
        owner: ActiveValue::Set(false),
        created_at: ActiveValue::NotSet,
        password_hash: ActiveValue::Set(Some(new.password_hash)),
        password_change_required: ActiveValue::Set(true),
    }
    .insert(db)
    .await
    .map_err(|error| row::taken(error, || Error::LoginTaken(new.login.to_owned())))?;
    decode(created)
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<User>, Error> {
    let mut query = user::Entity::find()
        .order_by_asc(user::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(user::Column::Id.gt(after));
    }
    query.all(db).await?.into_iter().map(decode).collect()
}

pub async fn find<C: ConnectionTrait>(db: &C, id: UserId) -> Result<Option<User>, Error> {
    user::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn find_by_login<C: ConnectionTrait>(db: &C, login: &str) -> Result<Option<User>, Error> {
    user::Entity::find()
        .filter(user::Column::Login.eq(login))
        .one(db)
        .await?
        .map(decode)
        .transpose()
}

pub async fn delete<C: ConnectionTrait>(db: &C, id: UserId) -> Result<bool, Error> {
    let deleted = user::Entity::delete_by_id(Uuid::from(id)).exec(db).await?;
    Ok(deleted.rows_affected > 0)
}

pub async fn bootstrap_owner<C: TransactionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
) -> Result<Option<User>, Error> {
    create_alone(db, login, token, None).await
}

pub async fn first_owner<C: TransactionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
    password_hash: String,
) -> Result<Option<User>, Error> {
    create_alone(db, login, token, Some(password_hash)).await
}

pub async fn credentials_by_login<C: ConnectionTrait>(
    db: &C,
    login: &str,
) -> Result<Option<Credentials>, Error> {
    user::Entity::find()
        .filter(user::Column::Login.eq(login))
        .one(db)
        .await?
        .map(credentials_of)
        .transpose()
}

pub async fn credentials<C: ConnectionTrait>(
    db: &C,
    id: UserId,
) -> Result<Option<Credentials>, Error> {
    user::Entity::find_by_id(Uuid::from(id))
        .one(db)
        .await?
        .map(credentials_of)
        .transpose()
}

pub async fn set_password<C: ConnectionTrait>(
    db: &C,
    id: UserId,
    password_hash: String,
) -> Result<(), Error> {
    let updated = user::Entity::update_many()
        .col_expr(user::Column::PasswordHash, Some(password_hash).into())
        .col_expr(user::Column::PasswordChangeRequired, false.into())
        .filter(user::Column::Id.eq(Uuid::from(id)))
        .exec(db)
        .await?;
    if updated.rows_affected == 0 {
        return Err(Error::Vanished(format!("the user {id}")));
    }
    Ok(())
}

async fn create_alone<C: TransactionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
    password_hash: Option<String>,
) -> Result<Option<User>, Error> {
    let transaction = db.begin().await?;
    transaction
        .execute_unprepared(&format!("SELECT pg_advisory_xact_lock({BOOTSTRAP_LOCK})"))
        .await?;
    if user::Entity::find().count(&transaction).await? > 0 {
        return Ok(None);
    }
    let owner = insert_owner(&transaction, login, token, password_hash).await?;
    transaction.commit().await?;
    Ok(Some(owner))
}

async fn insert_owner<C: ConnectionTrait>(
    db: &C,
    login: &str,
    token: NewToken,
    password_hash: Option<String>,
) -> Result<User, Error> {
    let owner = insert(db, login, true, password_hash).await?;
    tokens::insert(db, owner.id, token).await?;
    decode(owner)
}

async fn insert<C: ConnectionTrait>(
    db: &C,
    login: &str,
    owner: bool,
    password_hash: Option<String>,
) -> Result<user::Model, Error> {
    let password_change_required = password_hash.is_some();
    user::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        login: ActiveValue::Set(login.to_owned()),
        name: ActiveValue::Set(login.to_owned()),
        email: ActiveValue::Set(None),
        owner: ActiveValue::Set(owner),
        created_at: ActiveValue::NotSet,
        password_hash: ActiveValue::Set(password_hash),
        password_change_required: ActiveValue::Set(password_change_required),
    }
    .insert(db)
    .await
    .map_err(|error| match error.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => Error::LoginTaken(login.to_owned()),
        _ => Error::Database(error),
    })
}

fn credentials_of(found: user::Model) -> Result<Credentials, Error> {
    Ok(Credentials {
        password_hash: found.password_hash.clone(),
        password_change_required: found.password_change_required,
        user: decode(found)?,
    })
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
