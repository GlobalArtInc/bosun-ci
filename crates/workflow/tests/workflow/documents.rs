use std::collections::BTreeMap;
use std::fs;

use bosun_types::diagnostic::{Diagnostic, PathSegment};
use bosun_workflow::{MILESTONE, Milestone, PENDING};
use serde_json::Value;

use crate::{FILE, read, repository};

pub(crate) fn workflows_document() -> String {
    read(&repository().join("docs/design/02-workflows.md"))
}

pub(crate) fn table(document: &str, marker: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = document.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with(marker))
        .unwrap_or_else(|| panic!("no `{marker}` in the document"));
    let rows: Vec<&str> = lines[start..]
        .iter()
        .take_while(|line| line.starts_with('|'))
        .copied()
        .collect();
    assert!(rows.len() > 2, "the table `{marker}` has no rows");
    rows[2..]
        .iter()
        .map(|row| {
            row.trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

pub(crate) fn quoted(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn workflow_with(place: &str, key: &str) -> String {
    let job = "    runs-on: shell\n    steps:\n      - run: make\n";
    match place {
        "workflow" => format!("{key}: x\non: workflow_dispatch\njobs:\n  build:\n{job}"),
        "on" | "trigger" => format!("on:\n  {key}: x\njobs:\n  build:\n{job}"),
        "workflow_dispatch" => {
            format!("on:\n  workflow_dispatch:\n    {key}: x\njobs:\n  build:\n{job}")
        }
        "job" => format!("on: workflow_dispatch\njobs:\n  build:\n    {key}: x\n{job}"),
        "step" => format!("on: workflow_dispatch\njobs:\n  build:\n{job}        {key}: x\n"),
        other => panic!("no workflow template for keys of `{other}`"),
    }
}

fn extension_rows() -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for row in table(&workflows_document(), "| Key | Where | Meaning | From |") {
        for key in quoted(&row[0]) {
            let key = key.strip_prefix("on.").unwrap_or(&key);
            let key = key.split('.').next().unwrap_or(key).to_string();
            for place in row[1].split(',') {
                rows.push((place.trim().to_string(), key.clone(), row[3].clone()));
            }
        }
    }
    rows
}

pub(crate) fn extensions() -> Vec<(String, String)> {
    extension_rows()
        .into_iter()
        .map(|(place, key, _)| (place, key))
        .collect()
}

pub(crate) fn arrivals() -> Vec<(String, String, String)> {
    let mut arrivals = Vec::new();
    for row in table(&workflows_document(), "| Where | Keys | From |") {
        let place = row[0].trim_matches('`').to_string();
        for key in quoted(&row[1]) {
            arrivals.push((place.clone(), key, row[2].clone()));
        }
    }
    arrivals.extend(extension_rows());
    arrivals
}

pub(crate) fn milestone(name: &str) -> Milestone {
    Milestone::ALL
        .into_iter()
        .find(|milestone| milestone.name() == name)
        .unwrap_or_else(|| panic!("`{name}` is not a milestone"))
}

pub(crate) fn surface_place(place: &str) -> &str {
    if place == "trigger" { "on" } else { place }
}

fn about(diagnostic: &Diagnostic, key: &str) -> bool {
    ["not_implemented", "unknown_key"].contains(&diagnostic.code.as_str())
        && diagnostic.path.last() == Some(&PathSegment::Key(key.to_string()))
        && diagnostic.message.contains(&format!("`{key}` in "))
}

#[test]
fn a_key_is_implemented_exactly_when_its_milestone_is_reached_and_it_is_not_pending() {
    let arrivals = arrivals();
    assert!(
        arrivals.len() > 60,
        "the tables of docs/design/02-workflows.md list only {} keys",
        arrivals.len()
    );
    for &(place, key) in PENDING {
        assert!(
            arrivals
                .iter()
                .any(|(listed, name, _)| surface_place(listed) == place && name == key),
            "the pending key `{key}` of {place} is in neither table of docs/design/02-workflows.md"
        );
    }
    for (place, key, from) in arrivals {
        let milestone = milestone(&from);
        let source = workflow_with(&place, &key);
        let diagnostics = bosun_workflow::parse(FILE, &source)
            .err()
            .unwrap_or_default();
        let pending = PENDING.contains(&(surface_place(&place), key.as_str()));
        if milestone <= MILESTONE && !pending {
            assert!(
                !diagnostics.iter().any(|diagnostic| about(diagnostic, &key)),
                "`{key}` of {place} arrives in {from} and should be known: {diagnostics:#?}"
            );
            continue;
        }
        let expected = format!("it arrives in {from}");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                about(diagnostic, &key)
                    && diagnostic.code == "not_implemented"
                    && diagnostic.message.ends_with(&expected)
            }),
            "`{key}` of {place} should be refused as arriving in {from}: {diagnostics:#?}"
        );
    }
}

#[test]
fn diagnostic_codes_match_the_design_and_the_fixtures() {
    let mut documented = BTreeMap::new();
    for row in table(&workflows_document(), "| Code | Severity | Meaning |") {
        for code in quoted(&row[0]) {
            documented.insert(code, row[1].clone());
        }
    }
    let fixtures = repository().join("contracts/fixtures");
    let mut found = Vec::new();
    for (directory, list) in [("diagnostics", false), ("actions", true)] {
        let directory = fixtures.join(directory);
        for entry in fs::read_dir(&directory).expect("the fixtures are readable") {
            let path = entry.expect("a directory entry").path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let value: Value = serde_json::from_str(&read(&path))
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            if list {
                found.extend(value["diagnostics"].as_array().cloned().unwrap_or_default());
            } else {
                found.push(value);
            }
        }
    }
    let mut pinned = BTreeMap::new();
    for value in found {
        let code = value["code"].as_str().expect("a code").to_string();
        let severity = value["severity"].as_str().expect("a severity").to_string();
        let previous = pinned.insert(code.clone(), severity.clone());
        assert!(
            previous.is_none_or(|previous| previous == severity),
            "`{code}` is pinned with two severities"
        );
    }
    assert_eq!(pinned, documented);
}
