mod availability;
mod cases;
mod documents;
mod evaluation;
mod hosted;
mod limits;
mod properties;
mod recordings;

use std::fs;
use std::path::{Path, PathBuf};

use bosun_expr::{Error, Key};
use serde_json::{Value, json};

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn fixtures() -> PathBuf {
    repository().join("contracts/fixtures/expr")
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn load(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn error(error: &Error) -> Value {
    json!({
        "code": error.code.as_str(),
        "message": error.message,
        "span": error.span,
    })
}

pub fn key(path: &str) -> Key {
    Key::ALL
        .into_iter()
        .find(|key| key.path() == path)
        .unwrap_or_else(|| panic!("`{path}` is no key of the availability table"))
}
