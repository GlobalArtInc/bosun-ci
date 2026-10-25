use bosun_types::ir::{
    Action, Condition, Evaluable, JobKind, SecretReferences, Segment, Span, Template,
};
use serde_json::Value;

use crate::{invalid, only, parsed, path, valid};

fn expressive(template: &Template) -> bool {
    template
        .segments()
        .iter()
        .any(|segment| matches!(segment, Segment::Expression(_)))
}

fn contexts(value: &Value, name: &str, found: &mut Vec<(u32, u32, u32, u32)>) {
    match value {
        Value::Object(fields) => {
            if let Some(context) = fields.get("context")
                && context["name"] == name
            {
                let span: Span = serde_json::from_value(context["span"].clone()).expect("a span");
                found.push((span.line, span.column, span.end_line, span.end_column));
            }
            for field in fields.values() {
                contexts(field, name, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                contexts(item, name, found);
            }
        }
        _ => {}
    }
}

fn occurrences(source: &str, word: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut found = Vec::new();
    for (number, line) in source.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let word: Vec<char> = word.chars().collect();
        for column in 0..chars.len() {
            if chars[column..].starts_with(&word) {
                let line = u32::try_from(number + 1).expect("a small line number");
                let column = u32::try_from(column + 1).expect("a small column");
                let width = u32::try_from(word.len()).expect("a short word");
                found.push((line, column, line, column + width));
            }
        }
    }
    found
}

#[test]
fn expressions_are_lowered_wherever_a_value_is_read() {
    let workflow = valid(
        "\
on: workflow_dispatch
run-name: Deploy ${{ inputs.target }}
env:
  A: ${{ github.sha }}
concurrency: ci-${{ github.ref }}
jobs:
  build:
    name: Build ${{ matrix.os }}
    runs-on: [shell, '${{ matrix.os }}']
    env:
      B: x${{ 1 }}
    outputs:
      version: ${{ steps.read.outputs.version }}
    steps:
      - id: read
        name: ${{ env.NAME }}
        working-directory: ${{ env.DIR }}
        env:
          C: ${{ env.C }}
        run: echo ${{ github.sha }}
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.sha }}
  test:
    runs-on: ${{ fromJSON(vars.LABELS) }}
    steps:
      - run: make
  deploy:
    runs-on:
      group: ${{ vars.pool }}
    steps:
      - run: make
",
    );
    assert!(expressive(workflow.run_name.as_ref().expect("a run name")));
    assert!(expressive(&workflow.env["A"]));
    assert!(expressive(
        &workflow.concurrency.as_ref().expect("a group").group
    ));
    let build = &workflow.jobs[0];
    assert!(expressive(build.name.as_ref().expect("a name")));
    let Evaluable::Literal(runs_on) = &build.runs_on else {
        panic!("a list of labels");
    };
    assert_eq!(runs_on.labels[0], Template::text("shell"));
    assert!(expressive(&runs_on.labels[1]));
    assert!(expressive(&build.env["B"]));
    assert!(expressive(&build.outputs["version"]));
    let JobKind::Steps(steps) = &build.kind;
    assert!(expressive(steps[0].name.as_ref().expect("a name")));
    assert!(expressive(
        steps[0].working_directory.as_ref().expect("a directory")
    ));
    assert!(expressive(&steps[0].env["C"]));
    let Action::Run { script } = &steps[0].action else {
        panic!("a script");
    };
    assert!(expressive(script));
    assert!(expressive(&steps[1].with["ref"]));
    assert!(matches!(workflow.jobs[1].runs_on, Evaluable::Template(_)));
    let Evaluable::Literal(grouped) = &workflow.jobs[2].runs_on else {
        panic!("a group");
    };
    assert!(expressive(grouped.group.as_ref().expect("a group")));
}

#[test]
fn keys_that_take_no_expressions_refuse_them() {
    let cases = [
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n        shell: ${{ env.SHELL }}\n",
            "jobs.build.steps.0.shell",
            "`shell` of step 1 of job `build` takes no expressions",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - uses: actions/checkout@${{ vars.REF }}\n",
            "jobs.build.steps.0.uses",
            "`uses` of step 1 of job `build` takes no expressions",
        ),
        (
            "on: workflow_dispatch\ndefaults:\n  run:\n    shell: ${{ vars.SHELL }}\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n",
            "defaults.run.shell",
            "`shell` of `defaults.run` of the workflow takes no expressions",
        ),
        (
            "on: workflow_dispatch\ndefaults:\n  run:\n    working-directory: ${{ vars.DIR }}\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n",
            "defaults.run.working-directory",
            "`working-directory` of `defaults.run` of the workflow takes no expressions",
        ),
    ];
    for (source, at, message) in cases {
        let diagnostic = only(source);
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            ("invalid_value", message),
            "{source}"
        );
        assert_eq!(
            diagnostic.path,
            path(&at.split('.').collect::<Vec<_>>()),
            "{source}"
        );
    }
}

#[test]
fn spans_of_expressions_point_at_the_file_in_every_scalar_style() {
    let source = "\
on: workflow_dispatch
env:
  PLAIN: x ${{ github.sha }} y
  SINGLE: 'it''s ${{ github.sha }}'
  DOUBLE: \"tab\\t\\u00e9 ${{ github.sha }}\"
  CRLF_FREE: \"${{ format('{0}', github.ref) }}\"
jobs:
  build:
    runs-on: shell
    steps:
      - run: |
          echo one
            echo ${{ github.sha }}
      - run: >-
          folded
          ${{ github.sha }} and ${{ github.ref }}
      - run: echo ${{
          github.sha }}
";
    let workflow = valid(source);
    let mut found = Vec::new();
    contexts(
        &serde_json::to_value(&workflow).expect("serialisable"),
        "github",
        &mut found,
    );
    found.sort_unstable();
    assert_eq!(found, occurrences(source, "github"));
    assert_eq!(
        valid(&source.replace('\n', "\r\n")),
        workflow,
        "a CRLF file places its expressions on the same lines and columns"
    );
}

#[test]
fn diagnostics_of_expressions_sit_at_the_expression_in_the_file() {
    let source = "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: |\n          make\n          echo ${{ github.sha ) }}\n";
    let diagnostic = only(source);
    assert_eq!(diagnostic.code, "expression_syntax");
    assert_eq!(diagnostic.span.line, 8);
    assert!(diagnostic.span.column > 14, "{diagnostic:#?}");
    let unknown = only(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n        env:\n          A: '${{ foo.bar }}'\n",
    );
    assert_eq!(unknown.code, "unknown_context");
    assert_eq!(
        (
            unknown.span.line,
            unknown.span.column,
            unknown.span.end_column
        ),
        (8, 19, 22)
    );
    assert_eq!(
        unknown.path,
        path(&["jobs", "build", "steps", "0", "env", "A"])
    );
}

#[test]
fn an_if_is_an_expression_with_or_without_its_delimiters() {
    for condition in ["x == 1", "${{ x == 1 }}", "\" ${{ x == 1 }} \""] {
        let source = format!(
            "on: workflow_dispatch\njobs:\n  build:\n    if: {condition}\n    runs-on: shell\n    steps:\n      - run: make\n"
        );
        let diagnostics = invalid(&source);
        assert_eq!(diagnostics.len(), 1, "{source}");
        assert_eq!(diagnostics[0].code, "unknown_context", "{source}");
        assert_eq!(
            diagnostics[0].severity,
            bosun_types::diagnostic::Severity::Error
        );
    }
    let workflow = valid(
        "on: workflow_dispatch\njobs:\n  build:\n    if: ''\n    runs-on: shell\n    steps:\n      - if:\n        run: make\n      - if: \" ${{ success() }} \"\n        run: make\n",
    );
    assert_eq!(workflow.jobs[0].r#if, None);
    let JobKind::Steps(steps) = &workflow.jobs[0].kind;
    assert_eq!(steps[0].r#if, None);
    assert!(matches!(steps[1].r#if, Some(Condition::Expression(_))));
}

#[test]
fn an_if_with_text_around_its_expression_is_a_template_and_warned_about() {
    let parsed = parsed(
        "on: workflow_dispatch\njobs:\n  build:\n    if: ${{ always() }} && ${{ github.ref == 'refs/heads/main' }}\n    runs-on: shell\n    steps:\n      - if: \"ok ${{ failure() }}\"\n        run: make\n",
    );
    assert!(matches!(
        parsed.workflow.jobs[0].r#if,
        Some(Condition::Template(_))
    ));
    let JobKind::Steps(steps) = &parsed.workflow.jobs[0].kind;
    assert!(matches!(steps[0].r#if, Some(Condition::Template(_))));
    let warnings: Vec<(&str, Vec<_>)> = parsed
        .warnings
        .iter()
        .map(|warning| (warning.code.as_str(), warning.path.clone()))
        .collect();
    assert_eq!(
        warnings,
        [
            ("template_condition", path(&["jobs", "build", "if"])),
            (
                "template_condition",
                path(&["jobs", "build", "steps", "0", "if"])
            ),
        ]
    );
    assert_eq!(
        parsed.workflow.jobs[0].secrets,
        SecretReferences::Named(Default::default())
    );
}
