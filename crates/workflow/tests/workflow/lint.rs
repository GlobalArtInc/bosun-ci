use bosun_types::diagnostic::{Diagnostic, Severity};

use crate::{FILE, path};

const GITHUB: &str = ".github/workflows/ci.yml";

fn job(steps: &str) -> String {
    format!("jobs:\n  build:\n    runs-on: shell\n    steps:\n{steps}")
}

fn coded<'d>(diagnostics: &'d [Diagnostic], code: &str) -> Vec<&'d Diagnostic> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == code)
        .collect()
}

#[test]
fn the_bosun_context_warns_only_under_github_workflows() {
    let source = format!(
        "on: workflow_dispatch\n{}",
        job("      - run: echo ${{ bosun.run_url }}\n")
    );
    let warned = bosun_workflow::lint(GITHUB, &source);
    assert_eq!(warned.len(), 1, "{warned:#?}");
    assert_eq!(warned[0].code, "extension");
    assert_eq!(warned[0].severity, Severity::Warning);
    assert_eq!(
        warned[0].path,
        path(&["jobs", "build", "steps", "0", "run"])
    );
    for file in [
        "/home/ci/repo/.github/workflows/ci.yml",
        "./.github/workflows/ci.yml",
    ] {
        assert_eq!(bosun_workflow::lint(file, &source).len(), 1, "{file}");
    }
    for file in [
        FILE,
        ".github/workflows/nested/ci.yml",
        ".github/ci.yml",
        "nightly",
    ] {
        assert!(bosun_workflow::lint(file, &source).is_empty(), "{file}");
    }
    let parsed = bosun_workflow::parse(GITHUB, &source).expect("a valid workflow");
    assert!(parsed.warnings.is_empty(), "{:#?}", parsed.warnings);
}

#[test]
fn every_use_of_the_bosun_context_warns_in_any_case() {
    let source = format!(
        "on: workflow_dispatch\nrun-name: ${{{{ BOSUN.project }}}}\n{}",
        job("      - run: make\n        if: bosun.draft && Bosun['attempt'] > 1\n")
    );
    let warned = coded(&bosun_workflow::lint(GITHUB, &source), "extension").len();
    assert_eq!(warned, 3);
}

#[test]
fn extension_keys_warn_under_github_workflows_beside_their_refusal() {
    let source = format!(
        "priority: high\non:\n  push:\n  upstream:\n    project: a/b\n{}",
        job("      - run: make\n")
    );
    let github = bosun_workflow::lint(GITHUB, &source);
    let extensions = coded(&github, "extension");
    assert_eq!(extensions.len(), 2, "{github:#?}");
    assert_eq!(extensions[0].path, path(&["priority"]));
    assert_eq!(extensions[1].path, path(&["on", "upstream"]));
    assert_eq!(coded(&github, "not_implemented").len(), 2, "{github:#?}");
    let bosun = bosun_workflow::lint(FILE, &source);
    assert!(coded(&bosun, "extension").is_empty(), "{bosun:#?}");
    assert_eq!(coded(&bosun, "not_implemented").len(), 2, "{bosun:#?}");
    let listed = format!("on: [push, plugin]\n{}", job("      - run: make\n"));
    let warned = bosun_workflow::lint(GITHUB, &listed);
    assert_eq!(coded(&warned, "extension")[0].path, path(&["on", "1"]));
}

fn checkout(on: &str, uses: &str, with: &str) -> Vec<Diagnostic> {
    let source = format!(
        "on: {on}\n{}",
        job(&format!(
            "      - uses: {uses}\n        with:\n          {with}\n      - run: make\n"
        ))
    );
    bosun_workflow::lint(FILE, &source)
}

#[test]
fn a_checkout_of_the_head_under_pull_request_target_warns() {
    for with in [
        "ref: ${{ github.event.pull_request.head.sha }}",
        "ref: ${{ github.event.pull_request.head.ref }}",
        "ref: ${{ github.head_ref }}",
        "ref: refs/pull/${{ github.event.number }}/merge",
        "ref: refs/pull/${{ github.event.pull_request.number }}/head",
        "ref: ${{ github.event.pull_request.merge_commit_sha }}",
        "ref: ${{ github.event['pull_request'].head.sha }}",
        "ref: ${{ GITHUB.Event.Pull_Request.Head.SHA }}",
        "ref: ${{ format('{0}', github.event.pull_request.head.sha) }}",
        "repository: ${{ github.event.pull_request.head.repo.full_name }}",
    ] {
        for uses in ["actions/checkout@v4", "Actions/Checkout@v3"] {
            let diagnostics = checkout("pull_request_target", uses, with);
            assert_eq!(diagnostics.len(), 1, "{uses} {with}: {diagnostics:#?}");
            assert_eq!(diagnostics[0].code, "untrusted_checkout");
            assert_eq!(diagnostics[0].severity, Severity::Warning);
            let input = with.split(':').next().expect("an input");
            assert_eq!(
                diagnostics[0].path,
                path(&["jobs", "build", "steps", "0", "with", input])
            );
        }
    }
}

#[test]
fn other_checkouts_and_other_triggers_do_not_warn() {
    let head = "ref: ${{ github.event.pull_request.head.sha }}";
    for (on, uses, with) in [
        ("pull_request", "actions/checkout@v4", head),
        ("[push, pull_request]", "actions/checkout@v4", head),
        ("pull_request_target", "actions/checkout@v4", "ref: main"),
        (
            "pull_request_target",
            "actions/checkout@v4",
            "ref: ${{ github.sha }}",
        ),
        (
            "pull_request_target",
            "actions/checkout@v4",
            "ref: ${{ github.event.pull_request.base.sha }}",
        ),
        (
            "pull_request_target",
            "actions/checkout@v4",
            "path: ${{ github.head_ref }}",
        ),
        ("pull_request_target", "actions/setup-node@v4", head),
        ("pull_request_target", "actions/checkout/sub@v4", head),
    ] {
        let diagnostics = checkout(on, uses, with);
        assert!(
            diagnostics.is_empty(),
            "{on} {uses} {with}: {diagnostics:#?}"
        );
    }
}

#[test]
fn lint_returns_errors_and_warnings_in_the_order_of_the_file() {
    let source = format!(
        "on: workflow_dispatch\nrun-name: ${{{{ bosun.project }}}}\n{}",
        job("      - run: make\n        colour: red\n")
    );
    let diagnostics = bosun_workflow::lint(GITHUB, &source);
    let codes: Vec<&str> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect();
    assert_eq!(codes, ["extension", "unknown_key"]);
    assert!(bosun_workflow::parse(GITHUB, &source).is_err());
}
