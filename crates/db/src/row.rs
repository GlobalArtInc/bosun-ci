use bosun_types::id::IdError;
use sea_orm::prelude::Uuid;
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
