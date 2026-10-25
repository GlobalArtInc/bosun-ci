use std::collections::{BTreeMap, BTreeSet};

use bosun_types::diagnostic::Severity;
use bosun_types::ir::{
    Access, Action, ActionReference, Concurrency, Evaluable, JobKind, Permissions, Queue, Scope,
    SecretReferences, Strategy, Template,
};

use crate::{invalid, only, parsed, path, valid};

const STEPS: &str = "    steps:\n      - run: make\n";

fn job(keys: &str) -> String {
    format!("on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n{keys}{STEPS}")
}

fn step(keys: &str) -> String {
    format!("on: workflow_dispatch\njobs:\n  build:\n    runs-on: shell\n    steps:\n{keys}")
}

fn refusal(source: &str) -> (String, String) {
    let diagnostic = only(source);
    (diagnostic.code, diagnostic.message)
}

fn reference(owner: &str, repository: &str, path: Option<&str>, version: &str) -> ActionReference {
    ActionReference {
        owner: owner.to_string(),
        repository: repository.to_string(),
        path: path.map(str::to_string),
        r#ref: version.to_string(),
    }
}

#[test]
fn uses_takes_a_repository_and_a_ref_and_names_m2_for_other_forms() {
    for (uses, expected) in [
        (
            "actions/checkout@v4",
            reference("actions", "checkout", None, "v4"),
        ),
        (
            "actions/cache/restore@v4",
            reference("actions", "cache", Some("restore"), "v4"),
        ),
        (
            "github/codeql-action/upload-sarif/deep@0123abc",
            reference(
                "github",
                "codeql-action",
                Some("upload-sarif/deep"),
                "0123abc",
            ),
        ),
    ] {
        let workflow = valid(&step(&format!("      - uses: {uses}\n")));
        let JobKind::Steps(steps) = &workflow.jobs[0].kind;
        assert_eq!(steps[0].action, Action::Uses(expected.clone()));
        assert_eq!(expected.to_string(), uses);
    }
    for uses in ["./.github/actions/setup", "./x", "docker://alpine"] {
        let diagnostic = only(&step(&format!("      - uses: {uses}\n")));
        assert_eq!(diagnostic.code, "not_implemented", "{uses}");
        assert!(
            diagnostic.message.ends_with("it arrives in M2"),
            "{}",
            diagnostic.message
        );
        assert_eq!(
            diagnostic.path,
            path(&["jobs", "build", "steps", "0", "uses"])
        );
    }
    let call =
        only("on: workflow_dispatch\njobs:\n  call:\n    uses: ./.github/workflows/build.yml\n");
    assert_eq!(call.code, "not_implemented");
    assert!(call.message.ends_with("it arrives in M2"));
    for uses in [
        "actions/checkout",
        "checkout@v4",
        "actions/checkout@",
        "actions/checkout@v4 extra",
        "actions/checkout//x@v4",
        "act ions/checkout@v4",
    ] {
        let (code, message) = refusal(&step(&format!("      - uses: '{uses}'\n")));
        assert_eq!(code, "invalid_value", "{uses}: {message}");
    }
    assert_eq!(
        refusal(&step("      - uses: actions/checkout\n")).1,
        "`uses` of step 1 of job `build` is `actions/checkout`, which names no ref; write it as `actions/checkout@<ref>`"
    );
}

#[test]
fn run_and_uses_exclude_each_other_and_the_keys_of_the_other_form() {
    let cases = [
        (
            "      - run: make\n        uses: actions/checkout@v4\n",
            "`uses` of step 1 of job `build` cannot be used together with `run`",
            "uses",
        ),
        (
            "      - run: make\n        with:\n          ref: main\n",
            "`with` of step 1 of job `build` applies only to a step that uses an action",
            "with",
        ),
        (
            "      - uses: actions/checkout@v4\n        shell: bash\n",
            "`shell` of step 1 of job `build` applies only to a step that runs a script",
            "shell",
        ),
        (
            "      - uses: actions/checkout@v4\n        working-directory: src\n",
            "`working-directory` of step 1 of job `build` applies only to a step that runs a script",
            "working-directory",
        ),
    ];
    for (keys, message, key) in cases {
        let diagnostic = only(&step(keys));
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            ("invalid_value", message),
            "{keys}"
        );
        assert_eq!(diagnostic.path, path(&["jobs", "build", "steps", "0", key]));
    }
}

#[test]
fn concurrency_takes_a_group_or_a_mapping_and_refuses_cancelling_a_full_queue() {
    let workflow = valid(
        "\
on: workflow_dispatch
concurrency: deploy
jobs:
  build:
    runs-on: shell
    concurrency:
      group: ci-${{ github.ref }}
      cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}
      queue: max
    steps:
      - run: make
",
    );
    assert_eq!(
        workflow.concurrency,
        Some(Concurrency {
            group: Template::text("deploy"),
            cancel_in_progress: Evaluable::Literal(false),
            queue: Queue::Single,
        })
    );
    let group = workflow.jobs[0].concurrency.as_ref().expect("a group");
    assert!(matches!(group.cancel_in_progress, Evaluable::Template(_)));
    assert_eq!(group.queue, Queue::Max);

    let full = only(&job(
        "    concurrency:\n      group: ci\n      cancel-in-progress: true\n      queue: max\n",
    ));
    assert_eq!(full.code, "invalid_value");
    assert_eq!(
        full.path,
        path(&["jobs", "build", "concurrency", "cancel-in-progress"])
    );
    valid(&job(
        "    concurrency:\n      group: ci\n      cancel-in-progress: false\n      queue: max\n",
    ));
    for (keys, code) in [
        (
            "    concurrency:\n      cancel-in-progress: true\n",
            "missing_key",
        ),
        (
            "    concurrency:\n      group: ci\n      queue: all\n",
            "invalid_value",
        ),
        (
            "    concurrency:\n      group: ci\n      cancel-in-progress: 'true'\n",
            "invalid_type",
        ),
        ("    concurrency: ''\n", "invalid_value"),
        (
            "    concurrency:\n      group: ${{ steps.a.outputs.group }}\n",
            "unavailable_context",
        ),
    ] {
        assert_eq!(only(&job(keys)).code, code, "{keys}");
    }
}

#[test]
fn permissions_take_every_form_of_the_target() {
    let workflow = valid(
        "\
on: workflow_dispatch
permissions: read-all
jobs:
  a:
    permissions: write-all
    runs-on: shell
    steps: [{ run: a }]
  b:
    permissions: {}
    runs-on: shell
    steps: [{ run: b }]
  c:
    permissions:
      contents: read
      id-token: write
      pull-requests: none
      models: read
    runs-on: shell
    steps: [{ run: c }]
  d:
    runs-on: shell
    steps: [{ run: d }]
",
    );
    assert_eq!(workflow.permissions, Some(Permissions::ReadAll));
    assert_eq!(workflow.jobs[0].permissions, Some(Permissions::WriteAll));
    assert_eq!(
        workflow.jobs[1].permissions,
        Some(Permissions::Scopes(BTreeMap::new()))
    );
    assert_eq!(
        workflow.jobs[2].permissions,
        Some(Permissions::Scopes(BTreeMap::from([
            (Scope::Contents, Access::Read),
            (Scope::IdToken, Access::Write),
            (Scope::PullRequests, Access::None),
            (Scope::Models, Access::Read),
        ])))
    );
    assert_eq!(workflow.jobs[3].permissions, None);
    let cases = [
        (
            "    permissions: read_all\n",
            "invalid_value",
            "`permissions` of job `build` is `read_all`, which is neither `read-all`, `write-all` nor a mapping of scopes; did you mean `read-all`?",
        ),
        (
            "    permissions:\n      contnets: read\n",
            "unknown_key",
            "unknown scope `contnets` in `permissions` of job `build`; did you mean `contents`?",
        ),
        (
            "    permissions:\n      id-token: read\n",
            "invalid_value",
            "scope `id-token` in `permissions` of job `build` must be `write` or `none`",
        ),
        (
            "    permissions:\n      models: write\n",
            "invalid_value",
            "scope `models` in `permissions` of job `build` must be `read` or `none`",
        ),
        (
            "    permissions:\n      contents: admin\n",
            "invalid_value",
            "scope `contents` in `permissions` of job `build` must be `read`, `write` or `none`",
        ),
        (
            "    permissions: [contents]\n",
            "invalid_type",
            "`permissions` of job `build` must be `read-all`, `write-all` or a mapping of scopes",
        ),
    ];
    for (keys, code, message) in cases {
        let (found, text) = refusal(&job(keys));
        assert_eq!((found.as_str(), text.as_str()), (code, message), "{keys}");
    }
}

#[test]
fn secrets_a_job_names_are_collected_and_a_computed_name_takes_them_all() {
    let exact = valid(&step(
        "      - run: echo ${{ secrets.A }}\n        env:\n          B: ${{ secrets['B'] }}\n",
    ));
    assert_eq!(
        exact.jobs[0].secrets,
        SecretReferences::Named(BTreeSet::from(["A".to_string(), "B".to_string()]))
    );
    let workflow = valid(
        "\
on: workflow_dispatch
env:
  SHARED: ${{ secrets.shared }}
jobs:
  named:
    runs-on: shell
    env:
      A: ${{ secrets.A }}
    steps:
      - run: echo ${{ secrets['B'] }} ${{ contains(secrets.C.x, 'y') }} ${{ secrets[null] }}
      - run: echo ${{ secrets[true] }} ${{ secrets[0x10] }}
  quiet:
    runs-on: shell
    steps:
      - run: make
",
    );
    let names = |names: &[&str]| {
        SecretReferences::Named(names.iter().map(|name| (*name).to_string()).collect())
    };
    assert_eq!(
        workflow.jobs[0].secrets,
        names(&["16", "A", "B", "C", "SHARED", "TRUE"])
    );
    assert_eq!(workflow.jobs[1].secrets, names(&["SHARED"]));

    let computed = parsed(
        "\
on: workflow_dispatch
jobs:
  build:
    runs-on: shell
    strategy:
      matrix:
        k: [a, b]
    steps:
      - run: echo ${{ secrets.A }} ${{ secrets[format('X_{0}', matrix.k)] }}
  other:
    runs-on: shell
    steps:
      - run: echo ${{ secrets.B }}
",
    );
    assert_eq!(computed.workflow.jobs[0].secrets, SecretReferences::All);
    assert_eq!(computed.workflow.jobs[1].secrets, names(&["B"]));
    assert_eq!(computed.warnings.len(), 1, "{:#?}", computed.warnings);
    let warning = &computed.warnings[0];
    assert_eq!(
        (warning.severity, warning.code.as_str()),
        (Severity::Warning, "all_secrets")
    );
    assert_eq!(
        (
            warning.span.line,
            warning.span.column,
            warning.span.end_column
        ),
        (9, 40, 74)
    );
    assert_eq!(
        warning.message,
        "this expression reads `secrets` by a computed name, so job `build` receives every secret in scope instead of only the ones it names"
    );

    let whole = parsed(
        "on: workflow_dispatch\nenv:\n  ALL: ${{ toJSON(secrets) }}\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n",
    );
    assert_eq!(whole.workflow.jobs[0].secrets, SecretReferences::All);
    assert_eq!(
        whole.warnings[0].message,
        "this expression reads the whole `secrets` context, so every job of the workflow receives every secret in scope instead of only the ones it names"
    );
}

#[test]
fn strategy_keeps_vectors_include_exclude_and_expressions() {
    let workflow = valid(
        "\
on: workflow_dispatch
jobs:
  build:
    runs-on: shell
    strategy:
      fail-fast: false
      max-parallel: 2
      matrix:
        os: [linux, macos]
        node: ${{ fromJSON(vars.NODES) }}
        include:
          - os: linux
            experimental: true
        exclude: ${{ fromJSON(vars.EXCLUDED) }}
    steps:
      - run: make
  computed:
    runs-on: shell
    strategy:
      matrix: ${{ fromJSON(vars.MATRIX) }}
    steps:
      - run: make
  whole:
    runs-on: shell
    strategy: ${{ fromJSON(vars.STRATEGY) }}
    steps:
      - run: make
",
    );
    let Some(Evaluable::Literal(Strategy {
        matrix: Some(Evaluable::Literal(matrix)),
        fail_fast,
        max_parallel,
    })) = &workflow.jobs[0].strategy
    else {
        panic!("a literal strategy with a literal matrix");
    };
    assert_eq!(*fail_fast, Evaluable::Literal(false));
    assert_eq!(*max_parallel, Some(Evaluable::Literal(2)));
    let names: Vec<&str> = matrix
        .vectors
        .iter()
        .map(|vector| vector.name.as_str())
        .collect();
    assert_eq!(names, ["os", "node"]);
    assert!(matches!(matrix.vectors[1].values, Evaluable::Template(_)));
    assert!(matches!(matrix.include, Some(Evaluable::Literal(_))));
    assert!(matches!(matrix.exclude, Some(Evaluable::Template(_))));
    let Some(Evaluable::Literal(computed)) = &workflow.jobs[1].strategy else {
        panic!("a literal strategy");
    };
    assert!(matches!(computed.matrix, Some(Evaluable::Template(_))));
    assert_eq!(computed.fail_fast, Evaluable::Literal(true));
    assert!(matches!(
        workflow.jobs[2].strategy,
        Some(Evaluable::Template(_))
    ));
    let cases = [
        (
            "    strategy:\n      matrix:\n        os: linux\n",
            "invalid_type",
        ),
        (
            "    strategy:\n      matrix:\n        include: { os: linux }\n",
            "invalid_type",
        ),
        (
            "    strategy:\n      matrix:\n        include: [linux]\n",
            "invalid_type",
        ),
        ("    strategy:\n      max-parallel: 0\n", "invalid_value"),
        ("    strategy:\n      fail-fast: yes\n", "invalid_type"),
        ("    strategy: [os]\n", "invalid_type"),
        ("    strategy:\n      matrix: linux\n", "invalid_type"),
        ("    strategy:\n      parallel: 2\n", "unknown_key"),
        (
            "    strategy:\n      matrix:\n        os: ['${{ steps.a.outputs.os }}']\n",
            "unavailable_context",
        ),
    ];
    for (keys, code) in cases {
        assert_eq!(only(&job(keys)).code, code, "{keys}");
    }
}

#[test]
fn timeouts_and_continue_on_error_take_literals_or_expressions() {
    let workflow = valid(
        "\
on: workflow_dispatch
jobs:
  build:
    runs-on: shell
    timeout-minutes: ${{ fromJSON(vars.TIMEOUT) }}
    continue-on-error: ${{ matrix.experimental }}
    strategy:
      matrix:
        experimental: [true, false]
    steps:
      - run: make
        timeout-minutes: 5
        continue-on-error: true
      - run: make
  plain:
    runs-on: shell
    steps:
      - run: make
",
    );
    let build = &workflow.jobs[0];
    assert!(matches!(build.timeout_minutes, Evaluable::Template(_)));
    assert!(matches!(build.continue_on_error, Evaluable::Template(_)));
    let JobKind::Steps(steps) = &build.kind;
    assert_eq!(steps[0].timeout_minutes, Some(Evaluable::Literal(5)));
    assert_eq!(steps[0].continue_on_error, Evaluable::Literal(true));
    assert_eq!(steps[1].timeout_minutes, None);
    assert_eq!(steps[1].continue_on_error, Evaluable::Literal(false));
    assert_eq!(workflow.jobs[1].timeout_minutes, Evaluable::Literal(360));
    assert_eq!(
        workflow.jobs[1].continue_on_error,
        Evaluable::Literal(false)
    );
    for (keys, code) in [
        ("    timeout-minutes: 1.5\n", "invalid_value"),
        ("    timeout-minutes: -1\n", "invalid_value"),
        ("    timeout-minutes: '10'\n", "invalid_type"),
        ("    continue-on-error: 'true'\n", "invalid_type"),
        (
            "    timeout-minutes: ${{ steps.a.outputs.minutes }}\n",
            "unavailable_context",
        ),
    ] {
        assert_eq!(only(&job(keys)).code, code, "{keys}");
    }
}

#[test]
fn defaults_hold_the_shell_and_the_directory_of_run_steps() {
    let workflow = valid(
        "\
on: workflow_dispatch
defaults:
  run:
    shell: bash
    working-directory: src
jobs:
  build:
    runs-on: shell
    defaults:
      run:
        shell: ${{ matrix.shell }}
        working-directory: ${{ env.DIR }}
    strategy:
      matrix:
        shell: [bash, sh]
    steps:
      - run: make
",
    );
    assert_eq!(
        workflow.defaults.shell,
        Some(Evaluable::Literal(bosun_types::ir::Shell::Bash))
    );
    assert_eq!(
        workflow.defaults.working_directory,
        Some(Template::text("src"))
    );
    let defaults = &workflow.jobs[0].defaults;
    assert!(matches!(defaults.shell, Some(Evaluable::Template(_))));
    assert!(defaults.working_directory.is_some());
    for (keys, code) in [
        (
            "    defaults:\n      run:\n        shell: perl\n",
            "invalid_value",
        ),
        ("    defaults:\n      shell: bash\n", "unknown_key"),
        (
            "    defaults:\n      run:\n        shell: ${{ steps.a.outputs.shell }}\n",
            "unavailable_context",
        ),
    ] {
        assert_eq!(only(&job(keys)).code, code, "{keys}");
    }
}

#[test]
fn outputs_and_inputs_are_named_templates() {
    let workflow = valid(
        "\
on: workflow_dispatch
jobs:
  build:
    runs-on: shell
    outputs:
      version: ${{ steps.read.outputs.version }}
      fixed: 1.0
    steps:
      - id: read
        uses: actions/checkout@v4
        with:
          fetch-depth: 0
          ref: ${{ github.sha }}
",
    );
    let build = &workflow.jobs[0];
    let names: BTreeSet<&str> = build.outputs.keys().map(String::as_str).collect();
    assert_eq!(names, BTreeSet::from(["fixed", "version"]));
    assert_eq!(build.outputs["fixed"], Template::text("1"));
    let JobKind::Steps(steps) = &build.kind;
    assert_eq!(steps[0].with["fetch-depth"], Template::text("0"));
    let empty = invalid(&job("    outputs:\n      '': x\n"));
    assert_eq!(
        empty[0].message,
        "an output in `outputs` of job `build` has an empty name"
    );
}
