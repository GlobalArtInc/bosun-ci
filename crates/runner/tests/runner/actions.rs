use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use bosun_runner::{Annotation, AnnotationKind, Event, Job, NodeAction};
use bosun_types::ir::{Action, ActionReference, Step};
use bosun_types::status::Conclusion;
use serde_json::json;

use crate::{Finished, job, repository, run, run_cancelled_on, step, template};

const SECRET: &str = "s3cr3t-of-the-first-action";

fn node() -> PathBuf {
    env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .map(|directory| directory.join("node"))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| panic!("the tests of Node actions need `node` on the PATH"))
}

fn version() -> String {
    let output = Command::new(node())
        .arg("--version")
        .output()
        .expect("node runs");
    String::from_utf8(output.stdout)
        .expect("UTF-8")
        .trim()
        .to_owned()
}

fn reference(name: &str) -> ActionReference {
    ActionReference {
        owner: "bosun-fixtures".to_owned(),
        repository: "actions".to_owned(),
        path: Some(name.to_owned()),
        r#ref: "v1".to_owned(),
    }
}

fn fixture(name: &str) -> NodeAction {
    let directory = repository().join("compat/fixtures/actions").join(name);
    let file = format!("{name}/action.yml");
    let source = fs::read_to_string(directory.join("action.yml"))
        .unwrap_or_else(|error| panic!("{file}: {error}"));
    let parsed = bosun_workflow::action(&file, &source)
        .unwrap_or_else(|diagnostics| panic!("{file}: {diagnostics:#?}"));
    assert_eq!(parsed.warnings, Vec::new(), "{file}");
    NodeAction {
        uses: reference(name),
        directory,
        node: node(),
        metadata: parsed.metadata,
    }
}

fn uses(name: &str, id: Option<&str>, with: &[(&str, &str)]) -> Step {
    let mut uses = step("");
    uses.key = id.map(|id| id.parse().expect("a step id"));
    uses.action = Action::Uses(reference(name));
    uses.with = with
        .iter()
        .map(|(input, value)| ((*input).to_owned(), template(value)))
        .collect();
    uses
}

fn script(text: &str) -> Step {
    let mut script = step("");
    script.action = Action::Run {
        script: template(text),
    };
    script
}

fn fixtures(directory: &std::path::Path, steps: Vec<Step>, names: &[&str]) -> Job {
    let mut job = job(directory, steps);
    job.actions = names.iter().map(|name| fixture(name)).collect();
    job.contexts.github = json!({ "job": "build", "workspace": "" })
        .as_object()
        .cloned()
        .expect("an object");
    job
}

fn first(directory: &str) -> Step {
    uses(
        "first",
        Some("first"),
        &[
            ("value", "round trip"),
            ("directory", directory),
            ("secret", SECRET),
        ],
    )
}

fn warning(message: &str) -> Annotation {
    Annotation {
        kind: AnnotationKind::Warning,
        message: message.to_owned(),
        title: None,
        file: None,
        line: None,
        end_line: None,
        column: None,
        end_column: None,
    }
}

fn order(finished: &Finished) -> Vec<(u32, bool)> {
    finished
        .events
        .iter()
        .filter_map(|event| match event {
            Event::StepStarted { index, .. } => Some((*index, true)),
            Event::StepFinished { index, .. } => Some((*index, false)),
            Event::Line { .. } => None,
        })
        .collect()
}

fn ran(indexes: impl IntoIterator<Item = u32>) -> Vec<(u32, bool)> {
    indexes
        .into_iter()
        .flat_map(|index| [(index, true), (index, false)])
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn pre_steps_run_in_order_before_the_main_steps_and_post_steps_in_reverse_after_them() {
    let directory = tempfile::tempdir().expect("a directory");
    let tools = directory.path().join("added-tools");
    let tools = tools.display().to_string();
    let job = fixtures(
        directory.path(),
        vec![
            first(&tools),
            uses("second", None, &[]),
            script(&format!(
                "echo \"output ${{{{ steps.first.outputs.echo }}}}\"\necho \"variable \
                 $FIRST_VALUE\"\necho \"path ${{PATH%%:*}}\"\necho \"secret {SECRET}\""
            )),
        ],
        &["first", "second"],
    );
    let path = |name: &str| fixture(name).directory.display().to_string();
    let finished = run(job).await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Success,
        "{:#?}",
        finished.events
    );
    assert_eq!(order(&finished), ran(0..7));
    assert_eq!(finished.lines(0), ["pre first"]);
    assert_eq!(finished.lines(1), ["pre second"]);
    assert_eq!(
        finished.lines(2),
        [
            "main first".to_owned(),
            "state saved by the pre step of first".to_owned(),
            "secret ***".to_owned(),
            "job build".to_owned(),
            "GITHUB_ACTION=first".to_owned(),
            format!("GITHUB_ACTION_PATH={}", path("first")),
            "GITHUB_ACTION_REPOSITORY=bosun-fixtures/actions".to_owned(),
            "GITHUB_ACTION_REF=v1".to_owned(),
        ]
    );
    let required = "the action `bosun-fixtures/actions/second@v1` requires the input `name`, \
                    which the step does not supply";
    assert_eq!(
        finished.lines(3),
        [
            format!("##[warning]{required}"),
            "main second".to_owned(),
            "state saved by the pre step of second".to_owned(),
            format!("node {}", version()),
            "name ".to_owned(),
        ]
    );
    assert_eq!(
        finished.reported(3).map(|(annotations, _)| annotations),
        Some(vec![warning(required)])
    );
    assert_eq!(
        finished.lines(4),
        [
            "output round trip".to_owned(),
            "variable round trip".to_owned(),
            format!("path {tools}"),
            "secret ***".to_owned(),
        ]
    );
    assert_eq!(
        finished.lines(5),
        [
            "post second",
            "state saved by the pre step of second, saved by the main step of second"
        ]
    );
    assert_eq!(
        finished.lines(6),
        [
            "post first",
            "state saved by the pre step of first, saved by the main step of first"
        ]
    );
    assert_eq!(
        finished.ended(2),
        Some((
            Conclusion::Success,
            Conclusion::Success,
            BTreeMap::from([("echo".to_owned(), "round trip".to_owned())])
        ))
    );
    let everything = format!("{:?}", finished.events);
    assert!(
        !everything.contains(SECRET),
        "the secret of the action left the agent: {everything}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_step_with_post_if_failure_runs_only_after_a_failure() {
    for (last, conclusion, posted) in [
        ("exit 1", Conclusion::Failure, true),
        ("true", Conclusion::Success, false),
    ] {
        let directory = tempfile::tempdir().expect("a directory");
        let job = fixtures(
            directory.path(),
            vec![uses("failure", None, &[]), script(last)],
            &["failure"],
        );
        let finished = run(job).await;
        assert_eq!(finished.conclusion, conclusion, "{:#?}", finished.events);
        assert_eq!(finished.lines(0), ["main failure"]);
        assert_eq!(finished.started(2), posted, "{last}");
        if posted {
            assert_eq!(finished.lines(2), ["post failure"]);
            assert_eq!(finished.outcome(2), Some(Conclusion::Success));
        } else {
            assert_eq!(finished.outcome(2), Some(Conclusion::Skipped));
            assert_eq!(finished.lines(2), Vec::<String>::new());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_step_runs_after_a_cancel_and_sees_the_state_of_its_main_step() {
    let directory = tempfile::tempdir().expect("a directory");
    let tools = directory.path().join("added-tools").display().to_string();
    let job = fixtures(
        directory.path(),
        vec![first(&tools), script("echo ready\nsleep 30")],
        &["first"],
    );
    let finished = run_cancelled_on(job, Some("ready")).await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Cancelled,
        "{:#?}",
        finished.events
    );
    assert_eq!(finished.outcome(2), Some(Conclusion::Cancelled));
    assert_eq!(finished.outcome(3), Some(Conclusion::Success));
    assert_eq!(
        finished.lines(3),
        [
            "post first",
            "state saved by the pre step of first, saved by the main step of first"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_step_follows_a_pre_step_that_ran_and_none_follows_a_skipped_main_step_alone() {
    let directory = tempfile::tempdir().expect("a directory");
    let tools = directory.path().join("added-tools").display().to_string();
    let mut skipped_first = first(&tools);
    skipped_first.r#if = bosun_expr::condition("false").expect("a condition");
    let mut skipped_failure = uses("failure", None, &[]);
    skipped_failure.r#if = bosun_expr::condition("false").expect("a condition");
    let job = fixtures(
        directory.path(),
        vec![skipped_first, skipped_failure, script("exit 1")],
        &["first", "failure"],
    );
    let finished = run(job).await;
    assert_eq!(finished.conclusion, Conclusion::Failure);
    assert_eq!(finished.lines(0), ["pre first"]);
    for skipped in [1, 2, 4] {
        assert!(!finished.started(skipped), "step {skipped} started");
        assert_eq!(finished.outcome(skipped), Some(Conclusion::Skipped));
    }
    assert_eq!(
        finished.lines(5),
        ["post first", "state saved by the pre step of first, "]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deprecated_input_that_a_step_supplies_gives_a_warning() {
    let directory = tempfile::tempdir().expect("a directory");
    let job = fixtures(
        directory.path(),
        vec![uses(
            "second",
            None,
            &[("name", "${{ github.job }}"), ("Former", "x")],
        )],
        &["second"],
    );
    let finished = run(job).await;
    assert_eq!(finished.conclusion, Conclusion::Success);
    let deprecated = "the input `former` of the action `bosun-fixtures/actions/second@v1` is \
                      deprecated: Use `name` instead.";
    assert_eq!(
        finished.reported(1).map(|(annotations, _)| annotations),
        Some(vec![warning(deprecated)])
    );
    assert!(
        finished.lines(1).contains(&"name build".to_owned()),
        "{:?}",
        finished.lines(1)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_actions_could_not_be_fetched_fails_in_its_first_step() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut job = fixtures(directory.path(), vec![uses("first", None, &[])], &["first"]);
    job.fetched = Err("the server answered 403 Forbidden".to_owned());
    let finished = run(job).await;
    assert_eq!(finished.conclusion, Conclusion::Failure);
    assert_eq!(
        finished.lines(0),
        ["the job could not be prepared on this agent: the server answered 403 Forbidden"]
    );
    assert_eq!(order(&finished), ran([0]));
}
