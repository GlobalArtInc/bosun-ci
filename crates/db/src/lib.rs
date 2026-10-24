pub mod entity;
mod error;
pub mod journal;
pub mod pools;
mod row;
pub mod tokens;
pub mod users;

use sea_orm::sea_query::OnConflict;
use sea_orm::sqlx::{self, Connection, PgConnection};
use sea_orm::{
    ActiveValue, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr, EntityTrait,
    QueryOrder, RuntimeErr, Statement,
};
use sqlmig::Migrations;

use entity::schema_version;

pub use error::Error;

const SCHEMA: &str = "bosun";

const MIGRATION_LOCK: i64 = i64::from_be_bytes(*b"bosunmig");

const LOCK_WAIT: &str = "150s";

const MAJOR: i32 = match i32::from_str_radix(env!("CARGO_PKG_VERSION_MAJOR"), 10) {
    Ok(major) => major,
    Err(_) => panic!("the major version of the package is not a number"),
};

static MIGRATIONS: Migrations = sqlmig::embed!("$CARGO_MANIFEST_DIR/migrations");

pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, Error> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .set_schema_search_path(SCHEMA);
    Ok(Database::connect(options).await?)
}

pub async fn migrate(db: &DatabaseConnection) -> Result<Vec<String>, Error> {
    let lock = hold_migration_lock(db).await.map_err(Error::Lock)?;
    let migrated = migrate_locked(db).await;
    let _ = lock.close().await;
    migrated
}

async fn hold_migration_lock(db: &DatabaseConnection) -> Result<PgConnection, DbErr> {
    let mut lock = db
        .get_postgres_connection_pool()
        .acquire()
        .await
        .map_err(sqlx_error)?
        .detach();
    sqlx::raw_sql(&format!(
        "SET lock_timeout = '{LOCK_WAIT}'; SELECT pg_advisory_lock({MIGRATION_LOCK}); \
         RESET lock_timeout"
    ))
    .execute(&mut lock)
    .await
    .map_err(sqlx_error)?;
    Ok(lock)
}

async fn migrate_locked(db: &DatabaseConnection) -> Result<Vec<String>, Error> {
    db.execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS {SCHEMA}"))
        .await?;
    refuse_a_newer_major(db).await?;
    let applied = MIGRATIONS.apply(db).await?;
    schema_version::Entity::insert(schema_version::ActiveModel {
        major: ActiveValue::Set(MAJOR),
        migrated_at: ActiveValue::NotSet,
    })
    .on_conflict(
        OnConflict::column(schema_version::Column::Major)
            .do_nothing()
            .to_owned(),
    )
    .do_nothing()
    .exec_without_returning(db)
    .await?;
    Ok(applied)
}

async fn refuse_a_newer_major(db: &DatabaseConnection) -> Result<(), Error> {
    let recorded = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            format!("SELECT to_regclass('{SCHEMA}.schema_version') IS NOT NULL AS recorded"),
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "recorded"))
        .transpose()?
        .unwrap_or(false);
    if !recorded {
        return Ok(());
    }
    let newest = schema_version::Entity::find()
        .order_by_desc(schema_version::Column::Major)
        .one(db)
        .await?;
    match newest {
        Some(newest) if newest.major > MAJOR => Err(Error::NewerSchema {
            found: newest.major,
            supported: MAJOR,
        }),
        _ => Ok(()),
    }
}

fn sqlx_error(error: sqlx::Error) -> DbErr {
    DbErr::Conn(RuntimeErr::SqlxError(error))
}
