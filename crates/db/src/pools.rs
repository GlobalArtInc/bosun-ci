use bosun_types::api::Pool;
use bosun_types::id::PoolId;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::entity::pool;
use crate::{Error, row};

const DEFAULT: &str = "default";

pub async fn default_pool<C: ConnectionTrait>(db: &C) -> Result<Pool, Error> {
    named(db, DEFAULT)
        .await?
        .ok_or(Error::Missing("the pool `default`"))
}

pub async fn named<C: ConnectionTrait>(db: &C, name: &str) -> Result<Option<Pool>, Error> {
    pool::Entity::find()
        .filter(pool::Column::Name.eq(name))
        .one(db)
        .await?
        .map(|pool| {
            Ok(Pool {
                id: row::id("pool", pool.id)?,
                name: pool.name,
                trust: row::name("pool", pool.trust)?,
                created_at: pool.created_at,
            })
        })
        .transpose()
}

pub async fn eligible<C: ConnectionTrait>(
    db: &C,
    group: Option<&str>,
) -> Result<Vec<PoolId>, Error> {
    let mut query = pool::Entity::find().order_by_asc(pool::Column::Id);
    if let Some(group) = group {
        query = query.filter(pool::Column::Name.eq(group));
    }
    query
        .all(db)
        .await?
        .into_iter()
        .map(|pool| row::id("pool", pool.id))
        .collect()
}
