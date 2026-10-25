use bosun_types::id::IdError;
use sea_orm::prelude::Uuid;
use sea_orm::{DbBackend, DbErr, SqlErr, Statement, Value as Parameter};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::Error;

pub(crate) fn id<T>(table: &'static str, value: Uuid) -> Result<T, Error>
where
    T: TryFrom<Uuid, Error = IdError>,
{
    T::try_from(value).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}

pub(crate) fn optional_id<T>(table: &'static str, value: Option<Uuid>) -> Result<Option<T>, Error>
where
    T: TryFrom<Uuid, Error = IdError>,
{
    value.map(|value| id(table, value)).transpose()
}

pub(crate) fn json<T: DeserializeOwned>(table: &'static str, value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}

pub(crate) fn name<T: DeserializeOwned>(table: &'static str, value: String) -> Result<T, Error> {
    json(table, Value::String(value))
}

pub(crate) fn text<T: Serialize>(table: &'static str, value: T) -> Result<String, Error> {
    match serde_json::to_value(value)? {
        Value::String(text) => Ok(text),
        other => Err(Error::Row {
            table,
            message: format!("{other} is not stored as text"),
        }),
    }
}

pub(crate) fn unsigned(table: &'static str, value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}

pub(crate) fn signed(table: &'static str, value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|error| Error::Row {
        table,
        message: error.to_string(),
    })
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn taken(error: DbErr, taken: impl FnOnce() -> Error) -> Error {
    match error.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => taken(),
        _ => Error::Database(error),
    }
}

pub(crate) fn statement(sql: &str, values: impl IntoIterator<Item = Parameter>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}
