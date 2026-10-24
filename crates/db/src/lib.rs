pub mod entity;
mod error;
pub mod journal;
pub mod pools;
mod row;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection};
use sqlmig::Migrations;

pub use error::Error;

const SCHEMA: &str = "bosun";

static MIGRATIONS: Migrations = sqlmig::embed!("$CARGO_MANIFEST_DIR/migrations");

pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, Error> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .set_schema_search_path(SCHEMA);
    Ok(Database::connect(options).await?)
}

pub async fn migrate(db: &DatabaseConnection) -> Result<Vec<String>, Error> {
    db.execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS {SCHEMA}"))
        .await?;
    Ok(MIGRATIONS.apply(db).await?)
}
