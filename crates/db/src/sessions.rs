use bosun_types::api::User;
use bosun_types::id::{SessionId, UserId};
use sea_orm::prelude::DateTimeUtc;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    TransactionTrait,
};
use uuid::Uuid;

use crate::entity::{oidc_state, session, sign_in_failure, user};
use crate::periodic::{self, Task};
use crate::{Error, row, users};

const TABLE: &str = "session";

pub struct NewSession {
    pub user_id: UserId,
    pub sha256: Vec<u8>,
    pub at: DateTimeUtc,
}

pub struct Session {
    pub id: SessionId,
    pub user: User,
    pub password_change_required: bool,
    pub created_at: DateTimeUtc,
    pub last_seen_at: DateTimeUtc,
}

pub struct Sweep {
    pub idle_before: DateTimeUtc,
    pub created_before: DateTimeUtc,
    pub failed_before: DateTimeUtc,
    pub now: DateTimeUtc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    pub sessions: u64,
    pub failures: u64,
    pub states: u64,
}

pub async fn create<C: ConnectionTrait>(db: &C, new: NewSession) -> Result<SessionId, Error> {
    let created = session::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        user_id: ActiveValue::Set(Uuid::from(new.user_id)),
        sha256: ActiveValue::Set(new.sha256),
        created_at: ActiveValue::Set(new.at),
        last_seen_at: ActiveValue::Set(new.at),
    }
    .insert(db)
    .await?;
    row::id(TABLE, created.id)
}

pub async fn find<C: ConnectionTrait>(db: &C, sha256: &[u8]) -> Result<Option<Session>, Error> {
    let found = session::Entity::find()
        .filter(session::Column::Sha256.eq(sha256.to_vec()))
        .find_also_related(user::Entity)
        .one(db)
        .await?;
    let Some((session, Some(user))) = found else {
        return Ok(None);
    };
    Ok(Some(Session {
        id: row::id(TABLE, session.id)?,
        password_change_required: user.password_change_required,
        user: users::decode(user)?,
        created_at: session.created_at,
        last_seen_at: session.last_seen_at,
    }))
}

pub async fn touch<C: ConnectionTrait>(
    db: &C,
    id: SessionId,
    at: DateTimeUtc,
) -> Result<(), Error> {
    session::Entity::update_many()
        .col_expr(session::Column::LastSeenAt, at.into())
        .filter(session::Column::Id.eq(Uuid::from(id)))
        .filter(session::Column::LastSeenAt.lt(at))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn end<C: ConnectionTrait>(db: &C, id: SessionId) -> Result<(), Error> {
    session::Entity::delete_by_id(Uuid::from(id))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn end_others<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    kept: Option<SessionId>,
) -> Result<u64, Error> {
    let mut query =
        session::Entity::delete_many().filter(session::Column::UserId.eq(Uuid::from(user)));
    if let Some(kept) = kept {
        query = query.filter(session::Column::Id.ne(Uuid::from(kept)));
    }
    Ok(query.exec(db).await?.rows_affected)
}

pub async fn sweep<C: TransactionTrait>(db: &C, sweep: Sweep) -> Result<Option<Swept>, Error> {
    let transaction = db.begin().await?;
    if !periodic::try_lock(&transaction, Task::SessionSweep).await? {
        return Ok(None);
    }
    let sessions = session::Entity::delete_many()
        .filter(
            session::Column::LastSeenAt
                .lte(sweep.idle_before)
                .or(session::Column::CreatedAt.lte(sweep.created_before)),
        )
        .exec(&transaction)
        .await?
        .rows_affected;
    let failures = sign_in_failure::Entity::delete_many()
        .filter(sign_in_failure::Column::LastFailedAt.lte(sweep.failed_before))
        .filter(
            sign_in_failure::Column::LockedUntil
                .is_null()
                .or(sign_in_failure::Column::LockedUntil.lte(sweep.now)),
        )
        .exec(&transaction)
        .await?
        .rows_affected;
    let states = oidc_state::Entity::delete_many()
        .filter(oidc_state::Column::ExpiresAt.lte(sweep.now))
        .exec(&transaction)
        .await?
        .rows_affected;
    transaction.commit().await?;
    Ok(Some(Swept {
        sessions,
        failures,
        states,
    }))
}
