mod actions;
mod behaviour;
mod documents;
mod expressions;
mod fixtures;
mod github;
mod jobs;
mod lint;
mod on;
mod schema;
mod triggers;

use std::fs;
use std::path::{Path, PathBuf};

use bosun_types::diagnostic::{Diagnostic, PathSegment};
use bosun_types::ir::Workflow;
use bosun_workflow::Parsed;

pub const FILE: &str = ".bosun/workflows/ci.yml";

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn parsed(source: &str) -> Parsed {
    bosun_workflow::parse(FILE, source)
        .unwrap_or_else(|diagnostics| panic!("{source}\nproduced {diagnostics:#?}"))
}

pub fn valid(source: &str) -> Workflow {
    let parsed = parsed(source);
    assert!(
        parsed.warnings.is_empty(),
        "{source}\nwarned {:#?}",
        parsed.warnings
    );
    parsed.workflow
}

pub fn invalid(source: &str) -> Vec<Diagnostic> {
    match bosun_workflow::parse(FILE, source) {
        Ok(workflow) => panic!("{source}\nlowered to {workflow:#?}"),
        Err(diagnostics) => diagnostics,
    }
}

pub fn only(source: &str) -> Diagnostic {
    let mut diagnostics = invalid(source);
    assert_eq!(diagnostics.len(), 1, "{source}\nproduced {diagnostics:#?}");
    diagnostics.remove(0)
}

pub fn path(segments: &[&str]) -> Vec<PathSegment> {
    segments
        .iter()
        .map(|segment| match segment.parse() {
            Ok(index) => PathSegment::Index(index),
            Err(_) => PathSegment::Key((*segment).to_string()),
        })
        .collect()
}
