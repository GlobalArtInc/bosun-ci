use bosun_types::diagnostic::{Diagnostic, PathSegment};
use bosun_workflow::{Inventory, MILESTONE, Milestone, Uses};

use crate::fixtures::{cases, fixtures, json};
use crate::{FILE, read};

const SOURCE: &str = "\
on:
  workflow_dispatch:
    inputs:
      level:
        type: choice
        options: [low, high]
  workflow_call:
    inputs:
      version:
        type: string
permissions:
  contents: read
  contnets: write
jobs:
  build:
    runs-on:
      group: large
    timeout: 5
    services:
      db:
        image: postgres
    cache-volumes:
      - name: target
        path: target
    strategy:
      matrix:
        os: [linux]
        include:
          - os: macos
    env:
      LEVEL: high
    steps:
      - run: make
";

fn inventory(source: &str) -> Inventory {
    bosun_workflow::inventory(FILE, source)
        .unwrap_or_else(|fault| panic!("{source}\ncannot be inventoried: {fault:#?}"))
}

fn last(path: &[PathSegment]) -> Option<&str> {
    path.iter().rev().find_map(|segment| match segment {
        PathSegment::Key(name) => Some(name.as_str()),
        PathSegment::Index(_) => None,
    })
}

fn later(from: Option<Milestone>) -> bool {
    from.is_some_and(|from| from > MILESTONE)
}

fn names<'d>(diagnostics: &'d [Diagnostic], code: &str) -> Vec<&'d str> {
    let mut names: Vec<&str> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == code)
        .filter_map(|diagnostic| last(&diagnostic.path))
        .collect();
    names.sort_unstable();
    names
}

fn classified(inventory: &Inventory, pick: impl Fn(Option<Milestone>) -> bool) -> Vec<&str> {
    let mut names: Vec<&str> = inventory
        .keys
        .iter()
        .filter(|key| pick(key.from))
        .filter_map(|key| key.key.rsplit('.').next())
        .collect();
    names.sort_unstable();
    names
}

fn agrees(file: &str, source: &str, inventory: &Inventory) {
    let diagnostics = bosun_workflow::lint(file, source);
    assert_eq!(
        classified(inventory, |from| from.is_none()),
        names(&diagnostics, "unknown_key"),
        "{file}: the keys the inventory does not know are not the `unknown_key` diagnostics"
    );
    let refused = names(&diagnostics, "not_implemented");
    for name in classified(inventory, later) {
        assert!(
            refused.contains(&name),
            "{file}: the inventory says `{name}` arrives later, which no `not_implemented` says"
        );
    }
}

#[test]
fn every_key_is_named_by_its_place_with_the_milestone_it_arrives_in() {
    let mut keys: Vec<(String, Option<&str>, bool)> = inventory(SOURCE)
        .keys
        .into_iter()
        .map(|key| (key.key, key.from.map(Milestone::name), key.extension))
        .collect();
    keys.sort();
    let expected: Vec<(String, Option<&str>, bool)> = [
        ("jobs", Some("M0"), false),
        ("jobs.<job_id>.cache-volumes", Some("M2"), true),
        ("jobs.<job_id>.env", Some("M0"), false),
        ("jobs.<job_id>.runs-on", Some("M0"), false),
        ("jobs.<job_id>.runs-on.group", Some("M0"), false),
        ("jobs.<job_id>.services", Some("M2"), false),
        ("jobs.<job_id>.steps", Some("M0"), false),
        ("jobs.<job_id>.steps.run", Some("M0"), false),
        ("jobs.<job_id>.strategy", Some("M1"), false),
        ("jobs.<job_id>.strategy.matrix", Some("M1"), false),
        ("jobs.<job_id>.strategy.matrix.include", Some("M1"), false),
        ("jobs.<job_id>.timeout", None, false),
        ("on", Some("M0"), false),
        ("on.workflow_call", Some("M2"), false),
        ("on.workflow_dispatch", Some("M0"), false),
        ("on.workflow_dispatch.inputs", Some("M1"), false),
        (
            "on.workflow_dispatch.inputs.<input_id>.options",
            Some("M1"),
            false,
        ),
        (
            "on.workflow_dispatch.inputs.<input_id>.type",
            Some("M1"),
            false,
        ),
        ("permissions", Some("M1"), false),
        ("permissions.contents", Some("M1"), false),
        ("permissions.contnets", None, false),
    ]
    .into_iter()
    .map(|(key, from, extension)| (key.to_string(), from, extension))
    .collect();
    assert_eq!(keys, expected);
}

#[test]
fn the_inventory_knows_the_keys_the_parser_knows() {
    agrees(FILE, SOURCE, &inventory(SOURCE));
    let root = fixtures();
    for case in cases(&root.join("workflows"), "yml") {
        let source = read(&root.join("workflows").join(format!("{case}.yml")));
        let diagnostic = root.join("diagnostics").join(format!("{case}.json"));
        let file = if diagnostic.exists() {
            json(&diagnostic)["file"]
                .as_str()
                .unwrap_or(FILE)
                .to_string()
        } else {
            FILE.to_string()
        };
        match bosun_workflow::inventory(&file, &source) {
            Ok(inventory) => agrees(&file, &source, &inventory),
            Err(fault) => assert_eq!(
                serde_json::to_value(&fault).expect("serialisable"),
                json(&diagnostic),
                "{case}: the inventory refuses the file with another fault than the parser"
            ),
        }
    }
}

#[test]
fn every_uses_is_split_into_what_it_names_and_its_version() {
    let source = "\
on: push
jobs:
  call:
    uses: acme/shared/.github/workflows/build.yml@v1
  build:
    runs-on: linux
    steps:
      - uses: actions/checkout@v4
      - uses: actions/cache/restore@v4.2.0
      - uses: ./.github/actions/local
      - uses: docker://ghcr.io/acme/tool:1.2
      - uses: docker://localhost:5000/tool
      - uses: docker://alpine@sha256:0123
      - uses: actions/checkout
      - run: make
";
    let uses: Vec<(String, Option<String>)> = inventory(source)
        .uses
        .into_iter()
        .map(|Uses { name, version }| (name, version))
        .collect();
    let expected: Vec<(String, Option<String>)> = [
        ("acme/shared/.github/workflows/build.yml", Some("v1")),
        ("actions/checkout", Some("v4")),
        ("actions/cache/restore", Some("v4.2.0")),
        ("./.github/actions/local", None),
        ("docker://ghcr.io/acme/tool", Some("1.2")),
        ("docker://localhost:5000/tool", None),
        ("docker://alpine", Some("sha256:0123")),
        ("actions/checkout", None),
    ]
    .into_iter()
    .map(|(name, version)| (name.to_string(), version.map(str::to_string)))
    .collect();
    assert_eq!(uses, expected);
}

#[test]
fn a_file_that_is_not_yaml_cannot_be_inventoried() {
    let fault = bosun_workflow::inventory(".github/workflows/broken.yml", "on: [push\n")
        .expect_err("the YAML is broken");
    assert_eq!(fault.code, "yaml_syntax");
    assert_eq!(fault.file, ".github/workflows/broken.yml");
    assert!(fault.path.is_empty());
}
