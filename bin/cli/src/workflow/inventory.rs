use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bosun_workflow::Milestone;
use serde::Serialize;

use crate::client::Failure;
use crate::{Context, output};

const USES: &str = "uses.json";
const KEYS: &str = "keys.json";

#[derive(Serialize)]
struct Reference {
    uses: String,
    count: usize,
    versions: Vec<Version>,
}

#[derive(Serialize)]
struct Version {
    version: Option<String>,
    count: usize,
}

#[derive(Serialize)]
struct Key {
    key: String,
    count: usize,
    from: Option<&'static str>,
    extension: bool,
}

#[derive(Serialize)]
struct Summary {
    files: usize,
    uses: usize,
    references: usize,
    keys: usize,
    unknown: usize,
    written: Vec<PathBuf>,
}

fn unreadable(path: &Path, error: std::io::Error) -> Failure {
    Failure::new(format!("{} is not readable: {error}", path.display()))
}

fn files(directory: &Path, found: &mut Vec<PathBuf>) -> Result<(), Failure> {
    for entry in fs::read_dir(directory).map_err(|error| unreadable(directory, error))? {
        let entry = entry.map_err(|error| unreadable(directory, error))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| unreadable(&path, error))?;
        if kind.is_dir() {
            files(&path, found)?;
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            found.push(path);
        }
    }
    Ok(())
}

fn relative(directory: &Path, path: &Path) -> String {
    path.strip_prefix(directory)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn save<T: Serialize>(path: &Path, rows: &T) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(rows)
        .map_err(|error| Failure::new(format!("the inventory cannot be written: {error}")))?;
    fs::write(path, format!("{text}\n"))
        .map_err(|error| Failure::new(format!("{} cannot be written: {error}", path.display())))
}

pub(crate) fn write(
    context: &Context,
    directory: &Path,
    output: &Path,
) -> Result<ExitCode, Failure> {
    let mut paths = Vec::new();
    files(directory, &mut paths)?;
    paths.sort();
    let mut uses: BTreeMap<String, BTreeMap<Option<String>, usize>> = BTreeMap::new();
    let mut keys: BTreeMap<(String, Option<Milestone>, bool), usize> = BTreeMap::new();
    let mut faults = Vec::new();
    for path in &paths {
        let source = fs::read_to_string(path).map_err(|error| unreadable(path, error))?;
        match bosun_workflow::inventory(&relative(directory, path), &source) {
            Ok(found) => {
                for reference in found.uses {
                    *uses
                        .entry(reference.name)
                        .or_default()
                        .entry(reference.version)
                        .or_default() += 1;
                }
                for key in found.keys {
                    *keys.entry((key.key, key.from, key.extension)).or_default() += 1;
                }
            }
            Err(fault) => faults.push(format!(
                "{}:{}:{}: {}",
                fault.file, fault.span.line, fault.span.column, fault.message
            )),
        }
    }
    if !faults.is_empty() {
        let mut failure = Failure::new(format!(
            "{} under {} cannot be read as YAML",
            counted(faults.len(), "workflow file"),
            directory.display()
        ));
        failure.details = faults;
        return Err(failure);
    }
    let mut references: Vec<Reference> = uses
        .into_iter()
        .map(|(name, versions)| {
            let mut versions: Vec<Version> = versions
                .into_iter()
                .map(|(version, count)| Version { version, count })
                .collect();
            versions.sort_by_key(|version| Reverse(version.count));
            Reference {
                uses: name,
                count: versions.iter().map(|version| version.count).sum(),
                versions,
            }
        })
        .collect();
    references.sort_by_key(|reference| Reverse(reference.count));
    let keys: Vec<Key> = keys
        .into_iter()
        .map(|((key, from, extension), count)| Key {
            key,
            count,
            from: from.map(Milestone::name),
            extension,
        })
        .collect();
    fs::create_dir_all(output).map_err(|error| {
        Failure::new(format!("{} cannot be created: {error}", output.display()))
    })?;
    let written = vec![output.join(USES), output.join(KEYS)];
    save(&written[0], &references)?;
    save(&written[1], &keys)?;
    let summary = Summary {
        files: paths.len(),
        uses: references.iter().map(|reference| reference.count).sum(),
        references: references.len(),
        keys: keys.len(),
        unknown: keys.iter().filter(|key| key.from.is_none()).count(),
        written,
    };
    if context.json() {
        output::json(&summary)?;
    } else {
        println!(
            "inventoried {} under {}: {} of {}, {}, {} unknown to the parser",
            counted(summary.files, "workflow file"),
            directory.display(),
            counted(summary.uses, "use"),
            counted(summary.references, "reference"),
            counted(summary.keys, "key path"),
            summary.unknown
        );
        println!(
            "wrote {} and {}",
            summary.written[0].display(),
            summary.written[1].display()
        );
    }
    Ok(ExitCode::SUCCESS)
}
