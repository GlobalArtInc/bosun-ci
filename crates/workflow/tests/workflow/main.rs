mod behaviour;
mod documents;
mod fixtures;

use std::fs;
use std::path::{Path, PathBuf};

use bosun_types::diagnostic::Diagnostic;
use bosun_types::ir::Workflow;

pub const FILE: &str = ".bosun/workflows/ci.yml";

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn valid(source: &str) -> Workflow {
    bosun_workflow::parse(FILE, source)
        .unwrap_or_else(|diagnostics| panic!("{source}\nproduced {diagnostics:#?}"))
}

pub fn invalid(source: &str) -> Vec<Diagnostic> {
    match bosun_workflow::parse(FILE, source) {
        Ok(workflow) => panic!("{source}\nlowered to {workflow:#?}"),
        Err(diagnostics) => diagnostics,
    }
}
