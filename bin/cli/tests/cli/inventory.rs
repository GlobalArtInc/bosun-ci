use std::fs;
use std::path::Path;

use crate::{bosun, repository, text};

const FILES: [&str; 2] = ["uses.json", "keys.json"];

fn inventory(directory: &Path, output: &Path) -> std::process::Output {
    bosun(&[
        "workflow".as_ref(),
        "inventory".as_ref(),
        directory.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ])
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn assert_inventory(workflows: &str, committed: &str, hint: &str) {
    let root = repository();
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let ran = inventory(&root.join(workflows), scratch.path());
    assert!(
        ran.status.success(),
        "the inventory of {workflows} fails: {}",
        text(&ran.stderr)
    );
    for name in FILES {
        let produced = read(&scratch.path().join(name));
        assert_eq!(
            produced,
            read(&root.join(committed).join(name)),
            "{committed}/{name} is not the inventory of {workflows}; {hint}. It is now\n{produced}"
        );
    }
    let mut written: Vec<String> = fs::read_dir(scratch.path())
        .expect("the output directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    written.sort();
    assert_eq!(written, ["keys.json", "uses.json"]);
}

#[test]
fn the_inventory_of_the_fixture_tree_equals_its_committed_inventory() {
    assert_inventory(
        "compat/fixtures/inventory/workflows",
        "compat/fixtures/inventory/expected",
        "the inventory changed",
    );
}

#[test]
fn the_committed_inventory_of_the_corpus_is_current() {
    assert_inventory(
        "compat/workflows",
        "compat/inventory",
        "run `bosun workflow inventory compat/workflows` and commit compat/inventory/",
    );
}

#[test]
fn a_file_that_is_not_yaml_fails_the_inventory_and_is_named() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let workflows = scratch.path().join("acme/app/.github/workflows");
    fs::create_dir_all(&workflows).expect("the workflow directory");
    fs::write(workflows.join("ci.yml"), "on: [push\njobs: {}\n").expect("the workflow");
    let output = scratch.path().join("inventory");
    let ran = inventory(scratch.path(), &output);
    assert_eq!(ran.status.code(), Some(1));
    let said = text(&ran.stderr);
    assert!(
        said.starts_with("error: 1 workflow file under ")
            && said.contains("acme/app/.github/workflows/ci.yml:")
            && said.contains("invalid YAML"),
        "{said}"
    );
    assert!(!output.exists(), "a failed inventory writes nothing");
}

#[test]
fn the_inventory_prints_its_counts() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let ran = bosun(&[
        "--json".as_ref(),
        "workflow".as_ref(),
        "inventory".as_ref(),
        repository()
            .join("compat/fixtures/inventory/workflows")
            .as_os_str(),
        "--output".as_ref(),
        scratch.path().as_os_str(),
    ]);
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    let summary: serde_json::Value =
        serde_json::from_slice(&ran.stdout).expect("the summary is JSON");
    assert_eq!(summary["files"], 3);
    assert_eq!(summary["uses"], 8);
    assert_eq!(summary["references"], 5);
    assert_eq!(summary["keys"], 50);
    assert_eq!(summary["unknown"], 1);
}
