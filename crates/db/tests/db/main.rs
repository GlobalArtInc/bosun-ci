mod entities;
mod idempotency;
mod journal;
mod migrations;
mod parallel;
mod pools;
mod rows;
mod runs;
mod tenancy;
mod users;
mod workflows;

use std::fs;
use std::path::{Path, PathBuf};

use sea_orm::{ConnectionTrait, Statement};
use serde_json::Value;

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn fixture(path: &str) -> Value {
    let full = repository().join("contracts/fixtures").join(path);
    let text =
        fs::read_to_string(&full).unwrap_or_else(|error| panic!("{}: {error}", full.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
}

pub async fn strings<C: ConnectionTrait>(db: &C, sql: &str) -> Vec<String> {
    db.query_all(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_iter()
        .map(|row| {
            row.try_get::<String>("", "value")
                .expect("a text column `value`")
        })
        .collect()
}
