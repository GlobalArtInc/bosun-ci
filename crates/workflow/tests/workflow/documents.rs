use std::collections::BTreeSet;
use std::fs;

use bosun_types::diagnostic::PathSegment;
use serde_json::Value;

use crate::{FILE, invalid, read, repository};

fn workflows_document() -> String {
    read(&repository().join("docs/design/02-workflows.md"))
}

fn table(document: &str, marker: &str) -> Vec<Vec<String>> {
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

fn quoted(cell: &str) -> Vec<String> {
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

pub(crate) fn arrivals() -> Vec<(String, String, String)> {
    let document = workflows_document();
    let mut arrivals = Vec::new();
    for row in table(&document, "| Where | Keys | From |") {
        let place = row[0].trim_matches('`').to_string();
        for key in quoted(&row[1]) {
            arrivals.push((place.clone(), key, row[2].clone()));
        }
    }
    for row in table(&document, "| Key | Where | Meaning | From |") {
        for key in quoted(&row[0]) {
            let key = key.strip_prefix("on.").unwrap_or(&key);
            let key = key.split('.').next().unwrap_or(key).to_string();
            for place in row[1].split(',') {
                arrivals.push((place.trim().to_string(), key.clone(), row[3].clone()));
            }
        }
    }
    arrivals
}

fn implemented(place: &str, milestone: &str) -> bool {
    milestone == "M1" && matches!(place, "on" | "workflow_dispatch")
}

#[test]
fn keys_of_later_milestones_are_refused_naming_their_milestone() {
    let arrivals = arrivals();
    assert!(
        arrivals.len() > 60,
        "the tables of docs/design/02-workflows.md list only {} keys",
        arrivals.len()
    );
    for (place, key, milestone) in arrivals {
        assert!(
            ["M0", "M1", "M2", "M3", "M4", "M5"].contains(&milestone.as_str()),
            "`{key}` of {place} arrives in `{milestone}`, which is not a milestone"
        );
        if milestone == "M0" {
            continue;
        }
        let source = workflow_with(&place, &key);
        if implemented(&place, &milestone) {
            let diagnostics = bosun_workflow::parse(FILE, &source)
                .err()
                .unwrap_or_default();
            assert!(
                !diagnostics.iter().any(|diagnostic| {
                    ["not_implemented", "unknown_key"].contains(&diagnostic.code.as_str())
                        && diagnostic.path.last() == Some(&PathSegment::Key(key.clone()))
                }),
                "`{key}` of {place} arrives in {milestone} and should be known: {diagnostics:#?}"
            );
            continue;
        }
        let diagnostics = invalid(&source);
        let expected = format!("it arrives in {milestone}");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "not_implemented"
                    && diagnostic.path.last() == Some(&PathSegment::Key(key.clone()))
                    && diagnostic.message.ends_with(&expected)
            }),
            "`{key}` of {place} should be refused as arriving in {milestone}: {diagnostics:#?}"
        );
    }
}

#[test]
fn diagnostic_codes_match_the_design_and_the_fixtures() {
    let documented: BTreeSet<String> = table(&workflows_document(), "| Code | Meaning |")
        .iter()
        .flat_map(|row| quoted(&row[0]))
        .collect();
    let directory = repository().join("contracts/fixtures/diagnostics");
    let mut pinned = BTreeSet::new();
    for entry in fs::read_dir(&directory).expect("the diagnostic fixtures are readable") {
        let path = entry.expect("a directory entry").path();
        let value: Value = serde_json::from_str(&read(&path))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        pinned.insert(value["code"].as_str().expect("a code").to_string());
    }
    assert_eq!(pinned, documented);
}
