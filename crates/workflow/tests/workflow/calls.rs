use bosun_types::diagnostic::Diagnostic;
use bosun_types::ir::{Span, Workflow};
use bosun_workflow::Pending;

use crate::{FILE, path};

const CALLEE: &str = "\
on:
  workflow_call:
    inputs:
      version:
        required: true
        type: string
      debug:
        type: boolean
      jobs:
        type: number
        required: true
        default: 2
    secrets:
      token:
        required: true
      key:
jobs:
  build:
    runs-on: shell
    steps:
      - run: make
";

fn lowered(source: &str) -> Workflow {
    bosun_workflow::parse_with(FILE, source, Pending::Admit)
        .unwrap_or_else(|diagnostics| panic!("{source}\nproduced {diagnostics:#?}"))
        .workflow
}

fn check(call: &str, callee: &str) -> Vec<Diagnostic> {
    let caller = lowered(&format!(
        "on: push\njobs:\n  release:\n    uses: ./.github/workflows/build.yml\n{call}"
    ));
    bosun_workflow::check_call("caller.yml", &caller.jobs[0], &lowered(callee))
}

fn at(line: u32, column: u32, end_column: u32) -> Span {
    Span {
        line,
        column,
        end_line: line,
        end_column,
    }
}

#[test]
fn an_input_the_callee_does_not_declare_is_reported_at_its_entry() {
    let found = check(
        "    with:\n      version: '1'\n      verison: '2'\n      jobs: 1\n    secrets: inherit\n",
        CALLEE,
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "unknown_key");
    assert_eq!(found[0].file, "caller.yml");
    assert_eq!(found[0].span, at(7, 7, 19));
    assert_eq!(found[0].path, path(&["jobs", "release", "with", "verison"]));
    assert!(found[0].message.ends_with("did you mean `version`?"));
}

#[test]
fn a_required_input_without_a_default_that_is_not_passed_is_reported_at_the_uses() {
    let found = check("    secrets: inherit\n", CALLEE);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "missing_key");
    assert_eq!(found[0].span, at(4, 11, 40));
    assert_eq!(found[0].path, path(&["jobs", "release", "uses"]));
    assert!(found[0].message.contains("requires input `version`"));
}

#[test]
fn an_input_of_the_wrong_type_is_reported_and_one_that_may_fit_is_not() {
    let found = check(
        "    with:\n      version: 1\n      debug: maybe\n      jobs: true\n    secrets: inherit\n",
        CALLEE,
    );
    let wrong: Vec<(&str, Span)> = found
        .iter()
        .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.span))
        .collect();
    assert_eq!(
        wrong,
        vec![
            ("invalid_type", at(7, 7, 19)),
            ("invalid_type", at(8, 7, 17))
        ],
        "{found:#?}"
    );
    let fine = check(
        "    with:\n      version: ${{ github.sha }}\n      debug: ${{ vars.DEBUG }}\n      jobs: '4'\n    secrets: inherit\n",
        CALLEE,
    );
    assert!(fine.is_empty(), "{fine:#?}");
}

#[test]
fn secrets_are_checked_unless_the_call_inherits_them() {
    let with = "    with:\n      version: '1'\n      jobs: 1\n";
    let found = check(
        &format!("{with}    secrets:\n      tokn: ${{{{ secrets.TOKEN }}}}\n"),
        CALLEE,
    );
    let codes: Vec<(&str, Vec<_>)> = found
        .iter()
        .map(|diagnostic| (diagnostic.code.as_str(), diagnostic.path.clone()))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("missing_key", path(&["jobs", "release", "uses"])),
            ("unknown_key", path(&["jobs", "release", "secrets", "tokn"])),
        ]
    );
    assert!(check(&format!("{with}    secrets: inherit\n"), CALLEE).is_empty());
    let named = check(
        &format!("{with}    secrets:\n      TOKEN: ${{{{ secrets.TOKEN }}}}\n"),
        CALLEE,
    );
    assert!(named.is_empty(), "{named:#?}");
}

#[test]
fn a_callee_without_workflow_call_is_refused_once_at_the_uses() {
    let callee = "on: push\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n";
    let found = check("    with:\n      version: '1'\n", callee);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "invalid_value");
    assert_eq!(found[0].file, "caller.yml");
    assert_eq!(found[0].span, at(4, 11, 40));
    assert_eq!(found[0].path, path(&["jobs", "release", "uses"]));
    assert!(found[0].message.contains("no `workflow_call` trigger"));
}
