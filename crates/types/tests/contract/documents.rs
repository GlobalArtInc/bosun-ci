use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fs;

use bosun_types::command::CommandRequest;
use bosun_types::error::ErrorCode;
use bosun_types::permission::Permission;
use bosun_types::status::{
    ExecutionStatus, JobStatus, RunStatus, StatusReason, StepStatus, WaitingReason,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{fixture, name, repository};

fn document(file: &str) -> String {
    let path = repository().join("docs/design").join(file);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn table(document: &str, marker: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = document.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("no `{marker}` in the document"));
    let rows: Vec<&str> = lines[start..]
        .iter()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .copied()
        .collect();
    assert!(rows.len() > 2, "the table after `{marker}` has no rows");
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

fn first_column(document: &str, marker: &str) -> BTreeSet<String> {
    table(document, marker)
        .iter()
        .flat_map(|row| quoted(&row[0]))
        .collect()
}

fn parse<T: DeserializeOwned>(name: &str, context: &str) -> T {
    serde_json::from_value(Value::String(name.to_string()))
        .unwrap_or_else(|error| panic!("{context}: `{name}` is not a known value: {error}"))
}

fn check_transitions<S>(entity: &str, all: &[S], can_transition: fn(S, S) -> bool)
where
    S: Copy + Debug + Serialize + DeserializeOwned,
{
    let runs = document("04-runs.md");
    let marker = format!("The transitions of {entity}, exhaustively:");
    let mut allowed = BTreeSet::new();
    for row in table(&runs, &marker) {
        for from in quoted(&row[0]) {
            for to in quoted(&row[1]) {
                parse::<S>(&from, &marker);
                parse::<S>(&to, &marker);
                allowed.insert((from.clone(), to));
            }
        }
    }
    for &from in all {
        for &to in all {
            let pair = (name(from), name(to));
            assert_eq!(
                can_transition(from, to),
                allowed.contains(&pair),
                "{entity}: `{}` -> `{}` disagrees with docs/design/04-runs.md",
                pair.0,
                pair.1
            );
        }
    }
}

#[test]
fn run_transitions_match_the_design() {
    check_transitions("a run", &RunStatus::ALL, RunStatus::can_transition);
}

#[test]
fn job_transitions_match_the_design() {
    check_transitions("a job", &JobStatus::ALL, JobStatus::can_transition);
}

#[test]
fn execution_transitions_match_the_design() {
    check_transitions(
        "an execution",
        &ExecutionStatus::ALL,
        ExecutionStatus::can_transition,
    );
}

#[test]
fn step_transitions_match_the_design() {
    check_transitions("a step", &StepStatus::ALL, StepStatus::can_transition);
}

#[test]
fn error_codes_match_the_api_document() {
    let api = document("11-api.md");
    let mut documented = BTreeMap::new();
    for row in table(&api, "| Code | Status |") {
        let status: u16 = row[1]
            .parse()
            .unwrap_or_else(|_| panic!("`{}` is not a status", row[1]));
        for code in quoted(&row[0]) {
            documented.insert(code, status);
        }
    }
    let implemented: BTreeMap<String, u16> = ErrorCode::ALL
        .iter()
        .map(|code| (name(code), code.http_status()))
        .collect();
    assert_eq!(implemented, documented);
}

#[test]
fn waiting_reasons_match_the_design() {
    let implemented: BTreeSet<String> = WaitingReason::ALL.iter().map(name).collect();
    assert_eq!(
        implemented,
        first_column(&document("04-runs.md"), "| Reason | Ends when |")
    );
}

#[test]
fn status_reasons_match_the_design() {
    let implemented: BTreeSet<String> = StatusReason::ALL.iter().map(name).collect();
    assert_eq!(
        implemented,
        first_column(&document("04-runs.md"), "| Reason | Meaning |")
    );
}

#[test]
fn event_kinds_match_the_design_and_the_fixtures() {
    let documented = first_column(&document("04-runs.md"), "| Kind | Data |");
    let root = repository().join("contracts/fixtures/events");
    let mut fixtures = BTreeSet::new();
    for entry in fs::read_dir(&root).expect("the event fixtures are readable") {
        let file = entry.expect("a directory entry").file_name();
        let value = fixture(&format!("events/{}", file.to_string_lossy()));
        fixtures.insert(value["kind"].as_str().expect("a kind").to_string());
    }
    assert_eq!(fixtures, documented);
}

#[test]
fn command_fixtures_name_documented_commands() {
    let documented = first_column(&document("04-runs.md"), "| Command | Valid when |");
    for path in ["api/command-run-cancel.json", "api/command-job-cancel.json"] {
        let command = fixture(path)["type"]
            .as_str()
            .expect("a command type")
            .to_string();
        assert!(
            documented.contains(&command),
            "{path}: `{command}` is not in the command table of docs/design/04-runs.md"
        );
    }
}

#[test]
fn permission_keys_match_the_access_document() {
    let documented: Vec<String> = table(&document("08-access.md"), "| Permission | Allows |")
        .iter()
        .flat_map(|row| quoted(&row[0]))
        .collect();
    let implemented: Vec<String> = Permission::ALL
        .iter()
        .map(|permission| permission.key().to_owned())
        .collect();
    assert_eq!(implemented, documented);
    for permission in Permission::ALL {
        assert_eq!(permission.key(), name(permission));
        assert_eq!(Permission::from_key(permission.key()), Some(permission));
    }
    let mut ordered = Permission::ALL.to_vec();
    ordered.sort();
    assert_eq!(ordered, Permission::ALL);
}

#[test]
fn commands_need_the_permissions_of_the_command_table() {
    let documented: BTreeMap<String, Vec<String>> =
        table(&document("04-runs.md"), "| Command | Valid when |")
            .iter()
            .flat_map(|row| {
                let permissions = quoted(&row[3]);
                quoted(&row[0])
                    .into_iter()
                    .map(move |command| (command, permissions.clone()))
            })
            .collect();
    for path in ["api/command-run-cancel.json", "api/command-job-cancel.json"] {
        let request: CommandRequest =
            serde_json::from_value(fixture(path)).unwrap_or_else(|error| panic!("{path}: {error}"));
        let command = fixture(path)["type"]
            .as_str()
            .expect("a command type")
            .to_string();
        assert_eq!(
            documented[&command],
            vec![name(request.command.permission())],
            "{path}: `{command}` needs another permission in docs/design/04-runs.md"
        );
    }
}
