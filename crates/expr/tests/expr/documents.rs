use std::collections::{BTreeMap, BTreeSet};

use bosun_expr::{Code, Key};
use serde_json::Value;

use crate::availability::side;
use crate::cases::files;
use crate::{read, repository};

const REACHED: &[&str] = &["M0", "M1"];

fn document(name: &str) -> String {
    read(&repository().join("docs/design").join(name))
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

#[test]
fn error_codes_match_the_design_and_the_cases() {
    let design = document("03-expressions.md");
    let documented: BTreeSet<String> = ["| Code | Meaning |", "| Code | Failure |"]
        .iter()
        .flat_map(|marker| table(&design, marker))
        .flat_map(|row| quoted(&row[0]))
        .collect();
    let implemented: BTreeSet<String> = Code::ALL
        .iter()
        .map(|code| code.as_str().to_string())
        .collect();
    assert_eq!(implemented, documented);
    let mut pinned = BTreeSet::new();
    for (_, cases) in files() {
        for case in cases {
            let errors = case.get("error").into_iter().chain(
                case.get("errors")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            );
            for error in errors {
                pinned.insert(error["code"].as_str().expect("a code").to_string());
            }
        }
    }
    assert_eq!(
        pinned, documented,
        "every code is pinned by a case in contracts/fixtures/expr/"
    );
}

fn arrivals() -> BTreeMap<(String, String), String> {
    let workflows = document("02-workflows.md");
    let mut arrivals = BTreeMap::new();
    for row in table(&workflows, "| Where | Keys | From |") {
        let place = row[0].trim_matches('`').to_string();
        for key in quoted(&row[1]) {
            arrivals.insert((place.clone(), key), row[2].clone());
        }
    }
    for row in table(&workflows, "| Key | Where | Meaning | From |") {
        for key in quoted(&row[0]) {
            let key = key.strip_prefix("on.").unwrap_or(&key);
            let key = key.split('.').next().unwrap_or(key).to_string();
            for place in row[1].split(',') {
                arrivals.insert((place.trim().to_string(), key.clone()), row[3].clone());
            }
        }
    }
    arrivals
}

fn surface(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.split('.').collect();
    let (place, key) = match parts.as_slice() {
        ["on", event, ..] => ("on", *event),
        ["jobs", _, "steps", key, ..] => ("step", *key),
        ["jobs", _, key, ..] => ("job", *key),
        [key, ..] => ("workflow", *key),
        [] => panic!("an empty key"),
    };
    (place.to_string(), key.to_string())
}

#[test]
fn the_evaluating_side_of_every_key_matches_the_design() {
    let mut documented = BTreeMap::new();
    for row in table(
        &document("03-expressions.md"),
        "| Key | Evaluated by | When |",
    ) {
        let keys = quoted(&row[0]);
        assert_eq!(
            keys.len(),
            1,
            "a row of the key table names one key: {row:?}"
        );
        let previous = documented.insert(keys[0].clone(), row[1].clone());
        assert!(previous.is_none(), "`{}` has two rows", keys[0]);
    }
    for key in Key::ALL {
        assert_eq!(
            documented.get(key.path()).map(String::as_str),
            Some(side(key.side())),
            "`{}` is evaluated by the {} in the crate, which docs/design/03-expressions.md must say",
            key.path(),
            side(key.side())
        );
    }
    let arrivals = arrivals();
    for path in documented.keys() {
        let surface = surface(path);
        let milestone = arrivals.get(&surface).unwrap_or_else(|| {
            panic!(
                "`{path}` belongs to `{}` of {}, which docs/design/02-workflows.md does not list",
                surface.1, surface.0
            )
        });
        let held = Key::ALL.iter().any(|key| key.path() == path);
        assert_eq!(
            held,
            REACHED.contains(&milestone.as_str()),
            "`{path}` belongs to `{}` of {}, which arrives in {milestone}; the crate holds a key \
             exactly when its milestone is reached",
            surface.1,
            surface.0
        );
    }
}
