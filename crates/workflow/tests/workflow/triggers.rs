use std::collections::BTreeSet;
use std::fs;

use bosun_workflow::Event;
use serde_json::Value;

use crate::{read, repository, valid};

fn source(on: &Value) -> String {
    format!("on: {on}\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n")
}

fn text<'v>(event: &'v Value, key: &str) -> Option<&'v str> {
    event.get(key).and_then(Value::as_str)
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn every_trigger_case_matches_as_committed() {
    let directory = repository().join("contracts/fixtures/triggers");
    let mut files: Vec<_> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no trigger cases");
    let case_keys = BTreeSet::from(["description", "on", "event", "matches"]);
    let event_keys = BTreeSet::from(["name", "action", "ref", "base_ref", "changed_files"]);
    for path in files {
        let file = path.file_name().expect("a file name").to_string_lossy();
        let cases: Vec<Value> =
            serde_json::from_str(&read(&path)).unwrap_or_else(|error| panic!("{file}: {error}"));
        assert!(!cases.is_empty(), "{file} holds no cases");
        for case in &cases {
            assert_eq!(keys(case), case_keys, "{file}: {case}");
            let description = case["description"].as_str().expect("a description");
            let event = &case["event"];
            assert!(
                keys(event).is_subset(&event_keys),
                "{file}: {description}: unknown keys in {event}"
            );
            let workflow = valid(&source(&case["on"]));
            let changed: Option<Vec<String>> = event.get("changed_files").map(|files| {
                files
                    .as_array()
                    .expect("a list of changed files")
                    .iter()
                    .map(|file| file.as_str().expect("a path").to_string())
                    .collect()
            });
            let produced = bosun_workflow::matches(
                &workflow.triggers,
                &Event {
                    name: text(event, "name").expect("an event name"),
                    action: text(event, "action"),
                    r#ref: text(event, "ref"),
                    base_ref: text(event, "base_ref"),
                    changed_files: changed.as_deref(),
                },
            );
            assert_eq!(
                Some(produced),
                case["matches"].as_bool(),
                "{file}: {description}"
            );
        }
    }
}
