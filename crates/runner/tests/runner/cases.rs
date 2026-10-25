use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use bosun_types::ir::{Action, Evaluable, Shell, Step};
use bosun_types::status::Conclusion;
use serde::Deserialize;

use crate::{job, repository, run_cancelled_on, step, template};

const MINUTE: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Case {
    conclusion: Conclusion,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    defaults: CaseDefaults,
    timeout_minutes: Option<u32>,
    cancel_on: Option<String>,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
    reported: Option<BTreeMap<String, String>>,
    steps: Vec<CaseStep>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct CaseDefaults {
    shell: Option<String>,
    working_directory: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct CaseStep {
    id: Option<String>,
    #[serde(rename = "if")]
    condition: Option<String>,
    run: String,
    shell: Option<String>,
    working_directory: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    continue_on_error: Option<Written<bool>>,
    timeout_minutes: Option<Written<u32>>,
    outcome: Option<Conclusion>,
    conclusion: Option<Conclusion>,
    output: Option<Vec<String>>,
    outputs: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Written<T> {
    Literal(T),
    Expression(String),
}

impl<T: Copy> Written<T> {
    fn lowered(&self) -> Evaluable<T> {
        match self {
            Written::Literal(value) => Evaluable::Literal(*value),
            Written::Expression(text) => Evaluable::Template(template(text)),
        }
    }
}

fn shell(text: &str) -> Shell {
    text.parse()
        .unwrap_or_else(|error| panic!("the shell of a case is {error}"))
}

fn lowered(written: &CaseStep) -> Step {
    let mut lowered = step("");
    lowered.key = written
        .id
        .as_deref()
        .map(|id| id.parse().expect("a step id"));
    lowered.r#if = written.condition.as_deref().and_then(|condition| {
        bosun_expr::condition(condition).unwrap_or_else(|error| panic!("`{condition}`: {error:?}"))
    });
    lowered.action = Action::Run {
        script: template(&written.run),
    };
    lowered.shell = written.shell.as_deref().map(shell);
    lowered.working_directory = written.working_directory.as_deref().map(template);
    lowered.env = written
        .env
        .iter()
        .map(|(name, value)| (name.clone(), template(value)))
        .collect();
    if let Some(flag) = &written.continue_on_error {
        lowered.continue_on_error = flag.lowered();
    }
    lowered.timeout_minutes = written.timeout_minutes.as_ref().map(Written::lowered);
    lowered
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

async fn every_case_of(directory: &str) {
    let cases = cases(&repository().join("contracts/fixtures").join(directory));
    assert!(!cases.is_empty(), "no cases in {directory}");
    for (name, case) in cases {
        let directory = tempfile::tempdir().expect("a directory");
        let steps = case.steps.iter().map(lowered).collect();
        let mut job = job(directory.path(), steps);
        job.minute = MINUTE;
        job.evaluated.env = case.env.clone();
        job.evaluated.shell = case.defaults.shell.as_deref().map(shell);
        job.evaluated.working_directory = case.defaults.working_directory.clone();
        if let Some(minutes) = case.timeout_minutes {
            job.evaluated.timeout_minutes = minutes;
        }
        job.template.outputs = case
            .outputs
            .iter()
            .map(|(name, value)| (name.clone(), template(value)))
            .collect();
        let finished = run_cancelled_on(job, case.cancel_on.as_deref()).await;
        assert_eq!(
            finished.conclusion, case.conclusion,
            "{name}: the job concluded otherwise; it produced {:#?}",
            finished.events
        );
        if let Some(reported) = &case.reported {
            assert_eq!(
                &finished.outputs, reported,
                "{name}: the outputs of the job"
            );
        }
        for (step, index) in case.steps.iter().zip(0..) {
            let ended = finished.ended(index);
            match step.outcome {
                Some(outcome) => {
                    let (found, conclusion, outputs) = ended.unwrap_or_else(|| {
                        panic!(
                            "{name}: step {index} did not end; the job produced {:#?}",
                            finished.events
                        )
                    });
                    assert_eq!(
                        (found, conclusion),
                        (outcome, step.conclusion.unwrap_or(outcome)),
                        "{name}: the outcome and the conclusion of step {index}; the job \
                         produced {:#?}",
                        finished.events
                    );
                    assert_eq!(
                        finished.started(index),
                        outcome != Conclusion::Skipped,
                        "{name}: step {index} started otherwise than its outcome says"
                    );
                    if let Some(expected) = &step.outputs {
                        assert_eq!(&outputs, expected, "{name}: the outputs of step {index}");
                    }
                }
                None => assert!(
                    !finished.started(index) && ended.is_none(),
                    "{name}: step {index} was reached, but the case expects the job to end \
                     before it"
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

#[tokio::test(flavor = "multi_thread")]
async fn every_step_case_produces_its_outcomes_and_its_output() {
    every_case_of("steps").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_file_command_case_produces_its_outcomes_its_output_and_its_outputs() {
    every_case_of("file-commands").await;
}
