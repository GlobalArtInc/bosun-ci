use std::fs;
use std::path::{Path, PathBuf};

use crate::{read, repository};

fn workflows(directory: &Path, found: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            workflows(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            found.push(path);
        }
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn unknown_keys(root: &Path) -> Result<usize, String> {
    let mut files = Vec::new();
    workflows(root, &mut files);
    files.sort();
    let mut found = Vec::new();
    for path in &files {
        let file = relative(root, path);
        for diagnostic in bosun_workflow::lint(&file, &read(path)) {
            if diagnostic.code == "unknown_key" {
                found.push(format!(
                    "{file}:{}:{}: {}",
                    diagnostic.span.line, diagnostic.span.column, diagnostic.message
                ));
            }
        }
    }
    if found.is_empty() {
        Ok(files.len())
    } else {
        Err(format!(
            "workflows of the corpus under {} use keys the parser does not know:\n{}",
            root.display(),
            found.join("\n")
        ))
    }
}

#[test]
fn no_workflow_of_the_corpus_has_an_unknown_key() {
    if let Err(failure) = unknown_keys(&repository().join("compat/workflows")) {
        panic!("{failure}");
    }
}

#[test]
fn the_corpus_test_names_the_file_and_the_key_it_fails_on() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let directory = scratch.path().join("acme/app/.github/workflows");
    fs::create_dir_all(&directory).expect("the workflow directory");
    fs::write(
        directory.join("ci.yml"),
        "on: push\njobs:\n  build:\n    runs_on: linux\n    steps:\n      - run: make\n",
    )
    .expect("the workflow");
    fs::write(
        directory.join("release.yml"),
        "on: push\njobs:\n  build:\n    runs-on: linux\n    steps:\n      - run: make\n",
    )
    .expect("the workflow");
    let failure = unknown_keys(scratch.path()).expect_err("an unknown key fails the corpus test");
    assert!(
        failure.ends_with(
            "acme/app/.github/workflows/ci.yml:4:5: unknown key `runs_on` in job `build`; did you \
             mean `runs-on`?"
        ),
        "{failure}"
    );
    assert!(!failure.contains("release.yml"), "{failure}");
    fs::remove_file(directory.join("ci.yml")).expect("the workflow is removed");
    assert_eq!(unknown_keys(scratch.path()), Ok(1));
}

#[test]
fn every_workflow_of_bosun_validates_without_diagnostics() {
    let directory = repository().join(".bosun/workflows");
    let mut files = Vec::new();
    for entry in
        fs::read_dir(&directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
    {
        let path = entry.expect("a directory entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            files.push(path);
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no workflows under .bosun/workflows/");
    for path in files {
        let file = format!(".bosun/workflows/{}", relative(&directory, &path));
        let diagnostics = bosun_workflow::lint(&file, &read(&path));
        assert!(
            diagnostics.is_empty(),
            "{file} does not validate without diagnostics: {diagnostics:#?}"
        );
    }
}

#[test]
fn the_recording_workflows_validate_without_diagnostics() {
    for path in [
        "compat/recordings/expr.yml",
        "compat/fixtures/recordings/expr.yml",
    ] {
        let diagnostics = bosun_workflow::lint(
            ".github/workflows/expr.yml",
            &read(&repository().join(path)),
        );
        assert!(
            diagnostics.is_empty(),
            "{path} does not validate without diagnostics: {diagnostics:#?}"
        );
    }
}
