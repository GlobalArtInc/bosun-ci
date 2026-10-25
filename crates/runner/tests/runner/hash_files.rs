use std::fs;
use std::path::Path;
use std::process::Command;

use bosun_types::ir::{Expr, Segment, Span, Template};
use bosun_types::status::Conclusion;
use serde::Deserialize;

use crate::{job, repository, run, step};

const SPAN: Span = Span {
    line: 1,
    column: 1,
    end_line: 1,
    end_column: 1,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    description: String,
    patterns: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    fails: bool,
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("a directory");
    for entry in fs::read_dir(from).expect("a readable fixture") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("a copied file");
        }
    }
}

fn sha256sum(directory: &Path, files: &[String]) -> Vec<String> {
    let output = Command::new("sha256sum")
        .args(files)
        .current_dir(directory)
        .output()
        .expect("sha256sum runs");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .expect("UTF-8")
        .lines()
        .map(|line| line.split_whitespace().next().expect("a digest").to_owned())
        .collect()
}

fn expected(workspace: &Path, files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let digests: Vec<u8> = sha256sum(workspace, files)
        .iter()
        .flat_map(|digest| {
            (0..digest.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&digest[at..at + 2], 16).expect("hexadecimal"))
                .collect::<Vec<u8>>()
        })
        .collect();
    let directory = tempfile::tempdir().expect("a directory");
    fs::write(directory.path().join("digests"), digests).expect("the digests");
    sha256sum(directory.path(), &["digests".to_owned()]).remove(0)
}

fn hashing(patterns: &[String]) -> Template {
    Template::new(vec![Segment::Expression(Expr::Call {
        function: "hashFiles".to_owned(),
        arguments: patterns
            .iter()
            .map(|pattern| Expr::String {
                value: pattern.clone(),
                span: SPAN,
            })
            .collect(),
        span: SPAN,
    })])
}

async fn hashed(directory: &Path, patterns: &[String]) -> (Conclusion, Vec<String>) {
    let mut printing = step("printf '%s\\n' \"$HASH\"");
    printing.env.insert("HASH".to_owned(), hashing(patterns));
    let finished = run(job(directory, vec![printing])).await;
    (finished.conclusion, finished.lines(0))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_hash_files_case_equals_the_hash_of_sha256sum_over_its_files_in_their_order() {
    let fixtures = repository().join("contracts/fixtures/hash-files");
    let cases: Vec<Case> =
        serde_json::from_str(&fs::read_to_string(fixtures.join("cases.json")).expect("the cases"))
            .expect("cases");
    assert!(!cases.is_empty(), "no hashFiles cases");
    let directory = tempfile::tempdir().expect("a directory");
    let workspace = directory.path().join("work/globalart.keel/keel");
    copy(&fixtures.join("tree"), &workspace);
    for case in cases {
        let (conclusion, lines) = hashed(directory.path(), &case.patterns).await;
        if case.fails {
            assert_eq!(
                conclusion,
                Conclusion::Failure,
                "{}: {lines:?}",
                case.description
            );
            assert!(
                lines.iter().any(|line| line.contains("`hashFiles` failed")),
                "{}: {lines:?}",
                case.description
            );
        } else {
            assert_eq!(
                (conclusion, lines),
                (Conclusion::Success, vec![expected(&workspace, &case.files)]),
                "{}",
                case.description
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_outside_the_workspace_are_not_hashed() {
    let directory = tempfile::tempdir().expect("a directory");
    let outside = tempfile::tempdir().expect("a directory");
    fs::write(outside.path().join("outside.txt"), "outside").expect("a file");
    let workspace = directory.path().join("work/globalart.keel/keel");
    fs::create_dir_all(&workspace).expect("a workspace");
    fs::write(workspace.join("inside.txt"), "inside").expect("a file");
    let outside = fs::canonicalize(outside.path()).expect("a canonical directory");
    let (conclusion, lines) =
        hashed(directory.path(), &[format!("{}/*.txt", outside.display())]).await;
    assert_eq!(
        (conclusion, lines),
        (Conclusion::Success, vec![String::new()])
    );
    let (conclusion, lines) = hashed(
        directory.path(),
        &[format!("{}/*.txt", outside.display()), "*.txt".to_owned()],
    )
    .await;
    assert_eq!(
        (conclusion, lines),
        (
            Conclusion::Success,
            vec![expected(&workspace, &["inside.txt".to_owned()])]
        )
    );
}
