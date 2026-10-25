use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bosun_types::api::LintResponse;
use bosun_types::diagnostic::Severity;

use crate::client::{Failure, diagnostic};
use crate::{Context, output};

const DIRECTORIES: [&str; 2] = [".bosun/workflows", ".github/workflows"];

fn unreadable(path: &Path, error: std::io::Error) -> Failure {
    Failure::new(format!("{} is not readable: {error}", path.display()))
}

fn workflow_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
}

fn discover() -> Result<Vec<PathBuf>, Failure> {
    let mut found = Vec::new();
    for directory in DIRECTORIES.map(Path::new) {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(unreadable(directory, error)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry.map_err(|error| unreadable(directory, error))?.path();
            if workflow_file(&path) {
                files.push(path);
            }
        }
        files.sort();
        found.extend(files);
    }
    if found.is_empty() {
        return Err(Failure::new(
            "neither .bosun/workflows/ nor .github/workflows/ of the current directory holds a \
             .yml or .yaml file; name the files to lint",
        ));
    }
    Ok(found)
}

pub(crate) fn lint(context: &Context, files: Vec<PathBuf>) -> Result<ExitCode, Failure> {
    let files = if files.is_empty() { discover()? } else { files };
    let mut diagnostics = Vec::new();
    for path in &files {
        let source = fs::read_to_string(path).map_err(|error| unreadable(path, error))?;
        diagnostics.extend(bosun_workflow::lint(&path.to_string_lossy(), &source));
    }
    let count = |severity: Severity| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == severity)
            .count()
    };
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    if context.json() {
        output::json(&LintResponse { diagnostics })?;
    } else {
        for found in &diagnostics {
            println!("{}", diagnostic(found));
        }
        eprintln!(
            "linted {}: {}, {}",
            output::counted(files.len(), "workflow file"),
            output::counted(errors, "error"),
            output::counted(warnings, "warning")
        );
    }
    Ok(if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
