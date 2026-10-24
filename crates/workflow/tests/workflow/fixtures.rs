use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{FILE, read, repository};

fn fixtures() -> PathBuf {
    repository().join("contracts/fixtures")
}

fn cases(directory: &Path, extension: &str) -> Vec<String> {
    let mut cases: Vec<String> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|found| found == extension))
        .map(|path| {
            path.file_stem()
                .expect("a file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    cases.sort();
    cases
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("serialisable")
}

#[test]
fn every_workflow_fixture_lowers_to_its_snapshot_or_its_diagnostic() {
    let root = fixtures();
    let cases = cases(&root.join("workflows"), "yml");
    assert!(!cases.is_empty(), "no workflow fixtures");
    for case in cases {
        let source = read(&root.join("workflows").join(format!("{case}.yml")));
        let ir = root.join("ir").join(format!("{case}.json"));
        let diagnostic = root.join("diagnostics").join(format!("{case}.json"));
        let result = bosun_workflow::parse(FILE, &source);
        let produced = match &result {
            Ok(workflow) => serde_json::to_value(workflow),
            Err(diagnostics) => serde_json::to_value(diagnostics),
        }
        .expect("serialisable");
        match (ir.exists(), diagnostic.exists(), result.is_ok()) {
            (true, false, true) => {
                assert_eq!(
                    produced,
                    json(&ir),
                    "{case}: the IR differs from ir/{case}.json; it is now\n{}",
                    pretty(&produced)
                );
            }
            (false, true, false) => {
                assert_eq!(
                    produced,
                    Value::Array(vec![json(&diagnostic)]),
                    "{case}: the diagnostics differ from diagnostics/{case}.json; they are now\n{}",
                    pretty(&produced)
                );
            }
            (true, false, false) => panic!("{case}: expected the IR, got {}", pretty(&produced)),
            (false, true, true) => {
                panic!("{case}: expected a diagnostic, got {}", pretty(&produced))
            }
            _ => panic!(
                "{case}: exactly one of ir/{case}.json and diagnostics/{case}.json must exist"
            ),
        }
    }
}

#[test]
fn every_ir_and_diagnostic_fixture_comes_from_a_workflow_fixture() {
    let root = fixtures();
    for directory in ["ir", "diagnostics"] {
        for case in cases(&root.join(directory), "json") {
            assert!(
                root.join("workflows").join(format!("{case}.yml")).exists(),
                "{directory}/{case}.json has no workflows/{case}.yml that produces it"
            );
        }
    }
}
