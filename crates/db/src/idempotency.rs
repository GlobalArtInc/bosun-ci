use sea_orm::sea_query::{Expr, OnConflict, SimpleExpr};
use sea_orm::{
    ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TryInsertResult,
};
use serde_json::Value;

use crate::Error;
use crate::entity::idempotency_key;

const TABLE: &str = "idempotency_key";

const KEPT_FOR: &str = "now() - interval '24 hours'";

pub struct Stored {
    pub request_sha256: Vec<u8>,
    pub status: u16,
    pub response: Value,
}

pub async fn find<C: ConnectionTrait>(
    db: &C,
    principal: &str,
    key: &str,
) -> Result<Option<Stored>, Error> {
    let Some(stored) = idempotency_key::Entity::find_by_id((principal.to_owned(), key.to_owned()))
        .filter(
            idempotency_key::Column::CreatedAt
                .into_expr()
                .gt(kept_since()),
        )
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(Stored {
        request_sha256: stored.request_sha256,
        status: u16::try_from(stored.status).map_err(|error| Error::Row {
            table: TABLE,
            message: error.to_string(),
        })?,
        response: stored.response,
    }))
}

pub async fn record<C: ConnectionTrait>(
    db: &C,
    principal: &str,
    key: &str,
    stored: Stored,
) -> Result<bool, Error> {
    let recorded = idempotency_key::Entity::insert(idempotency_key::ActiveModel {
        principal: ActiveValue::Set(principal.to_owned()),
        key: ActiveValue::Set(key.to_owned()),
        request_sha256: ActiveValue::Set(stored.request_sha256),
        status: ActiveValue::Set(i32::from(stored.status)),
        response: ActiveValue::Set(stored.response),
        created_at: ActiveValue::NotSet,
    })
    .on_conflict(
        OnConflict::columns([
            idempotency_key::Column::Principal,
            idempotency_key::Column::Key,
        ])
        .update_columns([
            idempotency_key::Column::RequestSha256,
            idempotency_key::Column::Status,
            idempotency_key::Column::Response,
        ])
        .value(idempotency_key::Column::CreatedAt, Expr::cust("now()"))
        .action_and_where(
            Expr::col((idempotency_key::Entity, idempotency_key::Column::CreatedAt))
                .lte(kept_since()),
        )
        .to_owned(),
    )
    .do_nothing()
    .exec_without_returning(db)
    .await?;
    Ok(matches!(recorded, TryInsertResult::Inserted(rows) if rows > 0))
}

fn kept_since() -> SimpleExpr {
    Expr::cust(KEPT_FOR)
}
