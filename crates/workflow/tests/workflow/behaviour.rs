use bosun_types::diagnostic::{Diagnostic, PathSegment};
use bosun_types::ir::{JobKind, Span, Trigger, WorkflowDispatch};

use crate::{invalid, valid};

const JOBS: &str = "jobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n";

fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn only(source: &str) -> Diagnostic {
    let mut diagnostics = invalid(source);
    assert_eq!(diagnostics.len(), 1, "{source}\nproduced {diagnostics:#?}");
    diagnostics.remove(0)
}

fn path(segments: &[&str]) -> Vec<PathSegment> {
    segments
        .iter()
        .map(|segment| match segment.parse() {
            Ok(index) => PathSegment::Index(index),
            Err(_) => PathSegment::Key((*segment).to_string()),
        })
        .collect()
}

#[test]
fn on_parses_as_a_trigger_not_a_boolean() {
    let dispatch = vec![Trigger::WorkflowDispatch(WorkflowDispatch::default())];
    for on in [
        "on: workflow_dispatch",
        "\"on\": workflow_dispatch",
        "on: [workflow_dispatch]",
        "on:\n  workflow_dispatch:",
        "on:\n  workflow_dispatch: {}",
    ] {
        assert_eq!(valid(&format!("{on}\n{JOBS}")).triggers, dispatch, "{on}");
    }
    let boolean = invalid(&format!("true: workflow_dispatch\n{JOBS}"));
    assert_eq!(codes(&boolean), ["invalid_type"]);
    assert_eq!(
        boolean[0].message,
        "every key in the workflow must be a string"
    );
}

#[test]
fn expressions_arrive_in_m1_wherever_a_value_is_read() {
    let cases = [
        (
            "on: workflow_dispatch\nenv:\n  A: ${{ github.sha }}\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n",
            "env.A",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    name: ${{ github.ref }}\n    runs-on: shell\n    steps:\n      - run: make\n",
            "jobs.build.name",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: [shell, '${{ matrix.os }}']\n    steps:\n      - run: make\n",
            "jobs.build.runs-on.1",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: { group: '${{ vars.pool }}' }\n    steps:\n      - run: make\n",
            "jobs.build.runs-on.group",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    env:\n      B: x${{ 1 }}\n    steps:\n      - run: make\n",
            "jobs.build.env.B",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: echo ${{ github.sha }}\n",
            "jobs.build.steps.0.run",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n        name: ${{ env.NAME }}\n        shell: ${{ env.SHELL }}\n        working-directory: ${{ env.DIR }}\n        env:\n          C: ${{ env.C }}\n",
            "jobs.build.steps.0.name jobs.build.steps.0.shell jobs.build.steps.0.working-directory jobs.build.steps.0.env.C",
        ),
    ];
    for (source, paths) in cases {
        let diagnostics = invalid(source);
        let expected: Vec<Vec<PathSegment>> = paths
            .split(' ')
            .map(|dotted| path(&dotted.split('.').collect::<Vec<_>>()))
            .collect();
        let produced: Vec<Vec<PathSegment>> = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.path.clone())
            .collect();
        assert_eq!(produced, expected, "{source}");
        for diagnostic in &diagnostics {
            assert_eq!(diagnostic.code, "not_implemented");
            assert_eq!(
                diagnostic.message,
                "expressions are not supported yet; they arrive in M1"
            );
        }
    }
}

#[test]
fn the_name_of_a_workflow_is_never_evaluated() {
    let workflow = valid(&format!(
        "name: Deploy ${{{{ inputs.target }}}}\non: workflow_dispatch\n{JOBS}"
    ));
    assert_eq!(
        workflow.name.as_deref(),
        Some("Deploy ${{ inputs.target }}")
    );
}

#[test]
fn uses_names_the_milestone_of_its_form() {
    let step = |reference: &str| {
        format!(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - uses: {reference}\n"
        )
    };
    for (source, milestone) in [
        (step("actions/checkout@v4"), "M1"),
        (step("actions/cache/restore@v4"), "M1"),
        (step("./.github/actions/setup"), "M2"),
        (step("docker://alpine:3.20"), "M2"),
        (
            "on: workflow_dispatch\njobs:\n  call:\n    uses: ./.github/workflows/build.yml\n"
                .to_string(),
            "M2",
        ),
    ] {
        let diagnostic = only(&source);
        assert_eq!(diagnostic.code, "not_implemented", "{source}");
        assert!(
            diagnostic
                .message
                .ends_with(&format!("it arrives in {milestone}")),
            "{source}\n{}",
            diagnostic.message
        );
    }
}

#[test]
fn unknown_keys_suggest_the_closest_known_key() {
    let cases = [
        (
            "on: workflow_dispatch\njob:\n  build: {}\n".to_string(),
            "unknown key `job` in the workflow; did you mean `jobs`?",
        ),
        (
            format!("on: [workflow-dispatch]\n{JOBS}"),
            "unknown event `workflow-dispatch` in `on`; did you mean `workflow_dispatch`?",
        ),
        (
            format!("on:\n  pusj:\n{JOBS}"),
            "unknown event `pusj` in `on`; did you mean `push`?",
        ),
        (
            format!("on:\n  workflow_dispatch:\n    input: {{}}\n{JOBS}"),
            "unknown key `input` in `workflow_dispatch`; did you mean `inputs`?",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: { label: shell }\n    steps:\n      - run: make\n".to_string(),
            "unknown key `label` in `runs-on` of job `build`; did you mean `labels`?",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n        working_directory: src\n".to_string(),
            "unknown key `working_directory` in step 1 of job `build`; did you mean `working-directory`?",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    Steps:\n      - run: make\n".to_string(),
            "unknown key `Steps` in job `build`; did you mean `steps`?",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    colour: red\n    steps:\n      - run: make\n".to_string(),
            "unknown key `colour` in job `build`",
        ),
    ];
    for (source, message) in cases {
        let diagnostic = only(&source);
        assert_eq!(diagnostic.code, "unknown_key", "{source}");
        assert_eq!(diagnostic.message, message, "{source}");
    }
}

#[test]
fn every_problem_is_reported_in_source_order() {
    let source = "\
on: workflow_dispatch
jobs:
  build:
    needs: compile
    runs-on: shell
    steps:
      - id: 1st
        run: make
  test:
    runs-on: shell
    colour: red
    steps:
      - run: make test
";
    let diagnostics = invalid(source);
    let found: Vec<(&str, u32, u32)> = diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.code.as_str(),
                diagnostic.span.line,
                diagnostic.span.column,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("unknown_job", 4, 12),
            ("invalid_step_key", 7, 13),
            ("unknown_key", 11, 5),
        ]
    );
}

#[test]
fn every_job_of_a_longer_cycle_is_named() {
    let source = "\
on: workflow_dispatch
jobs:
  a:
    needs: c
    runs-on: shell
    steps: [{ run: a }]
  b:
    needs: a
    runs-on: shell
    steps: [{ run: b }]
  c:
    needs: [b]
    runs-on: shell
    steps: [{ run: c }]
  d:
    needs: d
    runs-on: shell
    steps: [{ run: d }]
";
    let diagnostics = invalid(source);
    let messages: Vec<&str> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "`needs` forms a cycle: `a` -> `c` -> `b` -> `a`",
            "`needs` forms a cycle: `d` -> `d`",
        ]
    );
    assert_eq!(diagnostics[0].path, path(&["jobs", "b", "needs"]));
}

#[test]
fn empty_and_null_values_are_refused() {
    let cases = [
        (format!("on: []\n{JOBS}"), "invalid_value", "`on` names no events"),
        (
            "on: workflow_dispatch\njobs: {}\n".to_string(),
            "invalid_value",
            "`jobs` defines no jobs",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps: []\n".to_string(),
            "invalid_value",
            "`steps` of job `build` lists no steps",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: []\n    steps: [{ run: make }]\n".to_string(),
            "invalid_value",
            "`runs-on` of job `build` names no labels",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: {}\n    steps: [{ run: make }]\n".to_string(),
            "invalid_value",
            "`runs-on` of job `build` names neither a group nor labels",
        ),
        (
            format!("on: workflow_dispatch\nenv:\n  EMPTY:\n{JOBS}"),
            "invalid_type",
            "variable `EMPTY` in `env` of the workflow must be a string",
        ),
        (
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run:\n".to_string(),
            "invalid_type",
            "`run` of step 1 of job `build` must be a string",
        ),
        (
            format!("on: [workflow_dispatch, workflow_dispatch]\n{JOBS}"),
            "duplicate_key",
            "event `workflow_dispatch` appears twice in `on`",
        ),
        (
            format!("on: workflow_dispatch\n{JOBS}    runs-on: podman\n"),
            "duplicate_key",
            "key `runs-on` appears twice in job `build`",
        ),
    ];
    for (source, code, message) in cases {
        let diagnostic = only(&source);
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            (code, message),
            "{source}"
        );
    }
}

#[test]
fn a_required_key_is_reported_where_its_mapping_is() {
    let diagnostics = invalid("name: ci\n");
    let found: Vec<(&str, &str, Span)> = diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.code.as_str(),
                diagnostic.message.as_str(),
                diagnostic.span,
            )
        })
        .collect();
    let document = Span {
        line: 1,
        column: 1,
        end_line: 1,
        end_column: 9,
    };
    assert_eq!(
        found,
        [
            ("missing_key", "missing key `on` in the workflow", document),
            (
                "missing_key",
                "missing key `jobs` in the workflow",
                document
            ),
        ]
    );
    let step = only(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - name: Build\n",
    );
    assert_eq!(step.message, "missing key `run` in step 1 of job `build`");
    assert_eq!(
        step.span,
        Span {
            line: 6,
            column: 9,
            end_line: 6,
            end_column: 20
        }
    );
}

#[test]
fn the_yaml_itself_is_held_to_one_plain_document() {
    let cases = [
        (
            format!("on: workflow_dispatch\n{JOBS}---\non: workflow_dispatch\n"),
            "unsupported_yaml",
            "a workflow file holds exactly one YAML document",
            7,
        ),
        (
            format!("name: !!str ci\non: workflow_dispatch\n{JOBS}"),
            "unsupported_yaml",
            "tags are not supported",
            1,
        ),
        (
            format!("on: workflow_dispatch\nenv: &env {{ A: *env }}\n{JOBS}"),
            "yaml_syntax",
            "invalid YAML: an alias refers to a node that contains it",
            2,
        ),
        (
            format!(
                "on: workflow_dispatch\nx: {}{}\n{JOBS}",
                "[".repeat(70),
                "]".repeat(70)
            ),
            "limit_exceeded",
            "collections nest deeper than 64 levels",
            2,
        ),
    ];
    for (source, code, message, line) in cases {
        let diagnostic = only(&source);
        assert_eq!(
            (
                diagnostic.code.as_str(),
                diagnostic.message.as_str(),
                diagnostic.span.line
            ),
            (code, message, line),
            "{source}"
        );
        assert!(diagnostic.path.is_empty(), "{source}");
    }
}

#[test]
fn line_endings_and_a_byte_order_mark_do_not_move_spans() {
    let source = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: |\n          make\n          make install\n      - run: 'make test'\n";
    let plain = valid(source);
    assert_eq!(valid(&source.replace('\n', "\r\n")), plain);
    assert_eq!(valid(&format!("\u{feff}{source}")), plain);
    let JobKind::Steps(steps) = &plain.jobs[0].kind;
    assert_eq!(
        steps[0].span,
        Span {
            line: 7,
            column: 9,
            end_line: 9,
            end_column: 23
        }
    );
    assert_eq!(
        steps[1].span,
        Span {
            line: 10,
            column: 9,
            end_line: 10,
            end_column: 25
        }
    );
}

#[test]
fn columns_count_characters_and_ignore_trailing_comments() {
    let diagnostic = only(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - { name: \"Сборка\", colour: x, run: make }\n",
    );
    assert_eq!(
        diagnostic.span,
        Span {
            line: 6,
            column: 27,
            end_line: 6,
            end_column: 33
        }
    );
    let workflow = valid(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: \"make\"   # the build\n",
    );
    let JobKind::Steps(steps) = &workflow.jobs[0].kind;
    assert_eq!(
        steps[0].span,
        Span {
            line: 6,
            column: 9,
            end_line: 6,
            end_column: 20
        }
    );
}
