use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use bosun_workflow::github::{
    self, BosunFacts, Dispatch, Facts, RepositoryDispatch, RunnerFacts, StepFacts,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{read, repository};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    event: String,
    produce: Option<Produce>,
    payload: Value,
    facts: Facts,
    runner: RunnerFacts,
    step: StepFacts,
    bosun: BosunFacts,
    context: Map<String, Value>,
    environment: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Produce {
    WorkflowDispatch(Dispatch),
    Schedule { cron: String },
    RepositoryDispatch(RepositoryDispatch),
}

impl Produce {
    fn payload(&self) -> Value {
        match self {
            Produce::WorkflowDispatch(dispatch) => github::dispatch_payload(dispatch),
            Produce::Schedule { cron } => github::schedule_payload(cron),
            Produce::RepositoryDispatch(dispatch) => github::repository_dispatch_payload(dispatch),
        }
    }
}

fn directory() -> PathBuf {
    repository().join("contracts/fixtures/github")
}

fn names() -> Vec<String> {
    let directory = directory();
    let mut names: Vec<String> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|found| found == "json"))
        .map(|path| {
            path.file_stem()
                .expect("a file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn case(name: &str) -> Case {
    let path = directory().join(format!("{name}.json"));
    serde_json::from_str(&read(&path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn derived(case: &Case) -> Map<String, Value> {
    github::context(&case.event, &case.payload, &case.facts)
}

fn property<'c>(context: &'c Map<String, Value>, key: &str) -> &'c str {
    context
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("`{key}` is not a string in {context:#?}"))
}

fn pretty<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("serialisable")
}

#[test]
fn every_github_fixture_derives_its_context_and_environment() {
    let names = names();
    assert!(!names.is_empty(), "no github fixtures");
    for name in names {
        let case = case(&name);
        if let Some(produce) = &case.produce {
            let payload = produce.payload();
            assert_eq!(
                payload,
                case.payload,
                "{name}: the produced payload differs from the fixture's; it is now\n{}",
                pretty(&payload)
            );
        }
        let mut context = derived(&case);
        assert_eq!(
            context.remove("event").as_ref(),
            Some(&case.payload),
            "{name}: github.event is not the payload"
        );
        assert_eq!(
            context,
            case.context,
            "{name}: the context differs from the fixture's; it is now\n{}",
            pretty(&context)
        );
        let step = github::step_context(&derived(&case), &case.runner, &case.step);
        let environment = github::environment(&step, &case.runner, &case.bosun);
        assert_eq!(
            environment,
            case.environment,
            "{name}: the environment differs from the fixture's; it is now\n{}",
            pretty(&environment)
        );
    }
}

#[test]
fn the_fixtures_cover_every_event_the_step_names() {
    let events: BTreeSet<String> = names().iter().map(|name| case(name).event).collect();
    for event in [
        "push",
        "create",
        "pull_request",
        "pull_request_target",
        "merge_group",
        "workflow_dispatch",
        "schedule",
        "repository_dispatch",
        "issues",
    ] {
        assert!(events.contains(event), "no github fixture for `{event}`");
    }
}

#[test]
fn dispatch_inputs_are_strings_in_the_payload() {
    let dispatch: Dispatch = serde_json::from_value(json!({
        "inputs": { "flag": true, "count": 3 },
        "ref": "refs/heads/main",
        "workflow": ".bosun/workflows/deploy.yml",
        "repository": null,
        "organization": null,
        "sender": null,
    }))
    .expect("a dispatch");
    let payload = github::dispatch_payload(&dispatch);
    assert_eq!(payload["inputs"]["flag"], json!("true"));
    assert_eq!(payload["inputs"]["count"], json!("3"));
    let workflow_dispatch = case("workflow-dispatch");
    assert_eq!(workflow_dispatch.payload["inputs"]["flag"], json!("true"));
    assert_eq!(workflow_dispatch.payload["inputs"]["count"], json!("3"));
}

#[test]
fn a_pull_request_runs_on_its_merge_ref_and_a_push_has_no_head_or_base_ref() {
    let pull_request = derived(&case("pull-request"));
    assert_eq!(property(&pull_request, "ref"), "refs/pull/2/merge");
    assert_eq!(property(&pull_request, "head_ref"), "changes");
    assert_eq!(property(&pull_request, "base_ref"), "master");
    for name in ["push-branch", "push-tag"] {
        let push = derived(&case(name));
        assert_eq!(property(&push, "head_ref"), "", "{name}");
        assert_eq!(property(&push, "base_ref"), "", "{name}");
    }
}

#[test]
fn a_tag_push_names_its_tag() {
    let push = derived(&case("push-tag"));
    assert_eq!(property(&push, "ref_type"), "tag");
    assert_eq!(property(&push, "ref_name"), "simple-tag");
}

#[test]
fn a_merged_pull_request_runs_on_its_base_branch_at_the_merge_commit() {
    let mut merged = case("pull-request");
    merged.payload["action"] = json!("closed");
    merged.payload["pull_request"]["merged"] = json!(true);
    merged.payload["pull_request"]["merge_commit_sha"] =
        json!("c4295bd74fb0f4fda03689c3df3f2803b658fd85");
    merged.facts.sha = None;
    let context = derived(&merged);
    assert_eq!(property(&context, "ref"), "refs/heads/master");
    assert_eq!(property(&context, "ref_name"), "master");
    assert_eq!(
        property(&context, "sha"),
        "c4295bd74fb0f4fda03689c3df3f2803b658fd85"
    );
    assert_eq!(property(&context, "head_ref"), "changes");
}

#[test]
fn a_release_runs_on_its_tag_and_a_deployment_at_its_commit() {
    let mut release = case("issues");
    release.event = "release".to_string();
    release.payload = json!({ "action": "published", "release": { "tag_name": "v1.2.0" } });
    let context = derived(&release);
    assert_eq!(property(&context, "ref"), "refs/tags/v1.2.0");
    assert_eq!(property(&context, "ref_type"), "tag");
    assert_eq!(
        property(&context, "sha"),
        release.facts.sha.as_deref().expect("a commit in the facts")
    );

    let mut deployment = case("issues");
    deployment.event = "deployment".to_string();
    deployment.payload = json!({
        "action": "created",
        "deployment": { "ref": "master", "sha": "9049f1265b7d61be4a8904a9a27120d2064dab3b" }
    });
    let context = derived(&deployment);
    assert_eq!(
        property(&context, "sha"),
        "9049f1265b7d61be4a8904a9a27120d2064dab3b"
    );
    assert_eq!(
        property(&context, "ref"),
        deployment
            .facts
            .git_ref
            .as_deref()
            .expect("a ref in the facts")
    );
}

#[test]
fn a_property_the_payload_lacks_comes_from_the_facts() {
    let mut push = case("push-branch");
    push.payload = json!({});
    push.facts.git_ref = Some("refs/heads/fallback".to_string());
    push.facts.sha = Some("1111111111111111111111111111111111111111".to_string());
    let context = derived(&push);
    assert_eq!(property(&context, "ref"), "refs/heads/fallback");
    assert_eq!(
        property(&context, "sha"),
        "1111111111111111111111111111111111111111"
    );
}

#[test]
fn the_step_context_adds_only_the_properties_of_the_step() {
    let case = case("push-branch");
    let job = derived(&case);
    let step = github::step_context(&job, &case.runner, &case.step);
    let added: BTreeSet<&str> = step
        .keys()
        .filter(|key| !job.contains_key(*key))
        .map(String::as_str)
        .collect();
    assert_eq!(
        added,
        BTreeSet::from([
            "action",
            "action_ref",
            "action_repository",
            "env",
            "event_path",
            "output",
            "path",
            "state",
            "step_summary",
        ])
    );
    for (key, value) in &job {
        assert_eq!(step.get(key), Some(value), "{key}");
    }
}

#[test]
fn only_strings_and_booleans_of_the_target_list_become_variables() {
    let case = case("push-branch");
    let step = github::step_context(&derived(&case), &case.runner, &case.step);
    let environment = github::environment(&step, &case.runner, &case.bosun);
    assert_eq!(environment["GITHUB_REF_PROTECTED"], "true");
    for absent in [
        "GITHUB_EVENT",
        "GITHUB_REPOSITORYURL",
        "GITHUB_ACTION_PATH",
        "GITHUB_TOKEN",
    ] {
        assert!(!environment.contains_key(absent), "{absent} is set");
    }
}
