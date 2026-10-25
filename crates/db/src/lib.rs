pub mod agents;
pub mod control;
pub mod entity;
mod error;
pub mod idempotency;
mod jobs;
pub mod journal;
pub mod logs;
pub mod notify;
pub mod organisations;
mod periodic;
pub mod pools;
pub mod projects;
pub mod queue;
mod row;
pub mod runs;
pub mod tokens;
pub mod users;
pub mod views;
pub mod workflows;

use std::time::{Duration, Instant};

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

const LOCK_WAIT: Duration = Duration::from_secs(150);

const LOCK_RETRY: Duration = Duration::from_millis(100);

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
    let lock = hold_migration_lock(db).await?;
    let migrated = migrate_locked(db).await;
    let _ = lock.close().await;
    migrated
}

async fn hold_migration_lock(db: &DatabaseConnection) -> Result<PgConnection, Error> {
    let failed = |error| Error::Lock(sqlx_error(error));
    let mut lock = db
        .get_postgres_connection_pool()
        .acquire()
        .await
        .map_err(failed)?
        .detach();
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(MIGRATION_LOCK)
            .fetch_one(&mut lock)
            .await
            .map_err(failed)?;
        if held {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            let _ = lock.close().await;
            return Err(Error::LockTimeout(LOCK_WAIT));
        }
        tokio::time::sleep(LOCK_RETRY).await;
    }
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

pub(crate) fn sqlx_error(error: sqlx::Error) -> DbErr {
    DbErr::Conn(RuntimeErr::SqlxError(error))
}
