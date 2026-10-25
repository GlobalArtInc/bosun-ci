mod inventory;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn bosun(arguments: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bosun"))
        .args(arguments)
        .output()
        .expect("the bosun binary runs")
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
