use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use bosun_runner::Step;
use bosun_types::ir::Shell;
use bosun_types::status::Conclusion;
use serde::Deserialize;

use crate::{job, repository, run};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    conclusion: Conclusion,
    #[serde(default)]
    env: BTreeMap<String, String>,
    steps: Vec<CaseStep>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct CaseStep {
    id: Option<String>,
    run: String,
    shell: Option<String>,
    working_directory: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    outcome: Option<Conclusion>,
    output: Option<Vec<String>>,
}

fn shell(text: &str) -> Shell {
    text.parse()
        .unwrap_or_else(|error| panic!("the shell of a case is {error}"))
}

fn cases(directory: &Path) -> Vec<(String, Case)> {
    let mut cases: Vec<(String, Case)> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|found| found == "toml"))
        .map(|path| {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let case =
                toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let name = path
                .file_stem()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            (name, case)
        })
        .collect();
    cases.sort_by(|left, right| left.0.cmp(&right.0));
    cases
}

#[tokio::test(flavor = "multi_thread")]
async fn every_step_case_produces_its_outcomes_and_its_output() {
    let cases = cases(&repository().join("contracts/fixtures/steps"));
    assert!(!cases.is_empty(), "no step cases");
    for (name, case) in cases {
        let directory = tempfile::tempdir().expect("a directory");
        let steps: Vec<Step> = case
            .steps
            .iter()
            .zip(0..)
            .map(|(step, index)| Step {
                index,
                key: step.id.clone(),
                script: step.run.clone(),
                shell: step.shell.as_deref().map(shell),
                working_directory: step.working_directory.clone(),
                env: step.env.clone(),
            })
            .collect();
        let mut job = job(directory.path(), steps);
        job.env = case.env.clone();
        let finished = run(job).await;
        assert_eq!(
            finished.conclusion, case.conclusion,
            "{name}: the job concluded otherwise; it produced {:#?}",
            finished.events
        );
        for (step, index) in case.steps.iter().zip(0..) {
            match step.outcome {
                Some(outcome) => assert_eq!(
                    finished.outcome(index),
                    Some(outcome),
                    "{name}: step {index}; the job produced {:#?}",
                    finished.events
                ),
                None => assert!(
                    !finished.started(index),
                    "{name}: step {index} started, but the case expects it not to run"
                ),
            }
            if let Some(output) = &step.output {
                assert_eq!(
                    &finished.lines(index),
                    output,
                    "{name}: the output of step {index}"
                );
            }
        }
    }
}
