mod documents;
mod fixtures;
mod stages;
mod values;

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
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

pub fn name<T: Serialize>(value: T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        other => panic!("not serialised as a string: {other:?}"),
    }
}
