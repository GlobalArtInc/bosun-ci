use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use bosun_types::api::LintResponse;
use bosun_types::diagnostic::Severity;

use crate::{repository, text};

fn lint(directory: &Path, arguments: &[&str]) -> Output {
    let home = tempfile::tempdir().expect("a home without a configuration");
    Command::new(env!("CARGO_BIN_EXE_bosun"))
        .args(arguments)
        .current_dir(directory)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .output()
        .expect("the bosun binary runs")
}

fn lines(bytes: &[u8]) -> Vec<String> {
    text(bytes).lines().map(str::to_owned).collect()
}

fn fixture(name: &str) -> String {
    let path = repository().join("contracts/fixtures/workflows").join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn place(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
    fs::write(path, source).expect("the file");
}

#[test]
fn a_file_with_an_error_exits_with_one_and_a_file_with_warnings_only_with_zero() {
    let root = repository();
    let failed = lint(
        &root,
        &[
            "workflow",
            "lint",
            "contracts/fixtures/workflows/unknown-key.yml",
        ],
    );
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stderr));
    assert_eq!(
        lines(&failed.stdout),
        [
            "contracts/fixtures/workflows/unknown-key.yml:7:5: error: unknown key `runs_on` in job \
             `build`; did you mean `runs-on`? [unknown_key]"
        ]
    );
    assert_eq!(
        lines(&failed.stderr),
        ["linted 1 workflow file: 1 error, 0 warnings"]
    );

    let warned = lint(
        &root,
        &[
            "workflow",
            "lint",
            "contracts/fixtures/workflows/all-secrets.yml",
        ],
    );
    assert_eq!(warned.status.code(), Some(0), "{}", text(&warned.stderr));
    let printed = lines(&warned.stdout);
    assert_eq!(printed.len(), 1, "{printed:?}");
    assert!(
        printed[0].starts_with("contracts/fixtures/workflows/all-secrets.yml:12:22: warning: ")
            && printed[0].ends_with(" [all_secrets]"),
        "{printed:?}"
    );

    let clean = lint(
        &root,
        &[
            "workflow",
            "lint",
            "contracts/fixtures/workflows/two-jobs.yml",
        ],
    );
    assert_eq!(clean.status.code(), Some(0), "{}", text(&clean.stderr));
    assert!(clean.stdout.is_empty(), "{}", text(&clean.stdout));
    assert_eq!(
        lines(&clean.stderr),
        ["linted 1 workflow file: 0 errors, 0 warnings"]
    );

    let both = lint(
        &root,
        &[
            "--json",
            "workflow",
            "lint",
            "contracts/fixtures/workflows/all-secrets.yml",
            "contracts/fixtures/workflows/unknown-key.yml",
        ],
    );
    assert_eq!(both.status.code(), Some(1), "{}", text(&both.stderr));
    let printed: LintResponse = serde_json::from_slice(&both.stdout).expect("the diagnostics");
    let severities: Vec<Severity> = printed
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.severity)
        .collect();
    assert_eq!(severities, [Severity::Warning, Severity::Error]);
    assert!(both.stderr.is_empty(), "{}", text(&both.stderr));
}

#[test]
fn json_prints_the_diagnostics_of_the_parser_at_the_path_of_each_file() {
    let root = repository();
    let files = [
        "contracts/fixtures/workflows/unknown-key.yml",
        "contracts/fixtures/workflows/two-jobs.yml",
        "contracts/fixtures/workflows/bosun-context-in-github-workflows.yml",
        "contracts/fixtures/workflows/untrusted-checkout.yml",
    ];
    let mut arguments = vec!["--json", "workflow", "lint"];
    arguments.extend(files);
    let ran = lint(&root, &arguments);
    let printed: LintResponse = serde_json::from_slice(&ran.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", text(&ran.stdout)));
    let expected: Vec<_> = files
        .iter()
        .flat_map(|file| {
            bosun_workflow::lint(
                file,
                &fs::read_to_string(root.join(file)).expect("a fixture"),
            )
        })
        .collect();
    assert_eq!(printed.diagnostics, expected);
    assert!(!expected.is_empty());
}

#[test]
fn without_files_it_lints_the_workflow_directories_of_the_current_directory() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let root = scratch.path();
    let empty = lint(root, &["workflow", "lint"]);
    assert_eq!(empty.status.code(), Some(1));
    assert_eq!(
        lines(&empty.stderr),
        [
            "error: neither .bosun/workflows/ nor .github/workflows/ of the current directory \
             holds a .yml or .yaml file; name the files to lint"
        ]
    );

    let bosun = fixture("bosun-context-in-github-workflows.yml");
    place(root, ".github/workflows/ci.yml", &bosun);
    place(root, ".bosun/workflows/release.yml", &bosun);
    place(
        root,
        ".bosun/workflows/build.yaml",
        &fixture("two-jobs.yml"),
    );
    place(root, ".bosun/workflows/notes.md", "not a workflow");
    place(
        root,
        ".bosun/workflows/nested/deep.yml",
        &fixture("unknown-key.yml"),
    );
    place(root, "ci.yml", &fixture("unknown-key.yml"));

    let warned = lint(root, &["--json", "workflow", "lint"]);
    assert_eq!(warned.status.code(), Some(0), "{}", text(&warned.stderr));
    let printed: LintResponse = serde_json::from_slice(&warned.stdout).expect("the diagnostics");
    assert_eq!(printed.diagnostics.len(), 1, "{:#?}", printed.diagnostics);
    assert_eq!(printed.diagnostics[0].code, "extension");
    assert_eq!(printed.diagnostics[0].file, ".github/workflows/ci.yml");
    let summary = lint(root, &["workflow", "lint"]);
    assert_eq!(
        lines(&summary.stderr),
        ["linted 3 workflow files: 0 errors, 1 warning"]
    );

    place(
        root,
        ".bosun/workflows/broken.yml",
        &fixture("unknown-key.yml"),
    );
    let failed = lint(root, &["workflow", "lint"]);
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stderr));
    let printed = lines(&failed.stdout);
    assert_eq!(printed.len(), 2, "{printed:?}");
    assert!(
        printed[0].starts_with(".bosun/workflows/broken.yml:7:5: error: "),
        "{printed:?}"
    );
    assert!(
        printed[1].starts_with(".github/workflows/ci.yml:6:33: warning: "),
        "{printed:?}"
    );
}

#[test]
fn a_file_that_cannot_be_read_fails_the_command() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let ran = lint(scratch.path(), &["workflow", "lint", "missing.yml"]);
    assert_eq!(ran.status.code(), Some(1));
    let said = text(&ran.stderr);
    assert!(
        said.starts_with("error: missing.yml is not readable: "),
        "{said}"
    );
    assert!(ran.stdout.is_empty());
}
