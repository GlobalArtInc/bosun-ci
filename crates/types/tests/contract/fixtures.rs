use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use bosun_types::api::{
    Agent, Attempt, DispatchRequest, Execution, Job, Organisation, Page, Pool, Project,
    RegisterAgentRequest, RegisterAgentResponse, RegistrationTokenCreated, Revision, Run, Step,
    Workflow,
};
use bosun_types::command::CommandRequest;
use bosun_types::diagnostic::Diagnostic;
use bosun_types::error::ErrorEnvelope;
use bosun_types::event::RunEvent;
use bosun_types::ir;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{fixture, repository};

type Check = fn(&Value) -> Result<Value, String>;

fn round_trip<T: Serialize + DeserializeOwned>(value: &Value) -> Result<Value, String> {
    let typed: T = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    serde_json::to_value(&typed).map_err(|error| error.to_string())
}

fn fixtures() -> Vec<(&'static str, Check)> {
    vec![
        ("api/agent.json", round_trip::<Agent>),
        ("api/attempt.json", round_trip::<Attempt>),
        ("api/command-job-cancel.json", round_trip::<CommandRequest>),
        ("api/command-run-cancel.json", round_trip::<CommandRequest>),
        ("api/dispatch.json", round_trip::<DispatchRequest>),
        ("api/error-invalid-input.json", round_trip::<ErrorEnvelope>),
        ("api/error-invalid-state.json", round_trip::<ErrorEnvelope>),
        ("api/execution.json", round_trip::<Execution>),
        ("api/job.json", round_trip::<Job>),
        ("api/organisation.json", round_trip::<Organisation>),
        ("api/page-runs.json", round_trip::<Page<Run>>),
        ("api/pool.json", round_trip::<Pool>),
        ("api/project.json", round_trip::<Project>),
        (
            "api/register-agent-request.json",
            round_trip::<RegisterAgentRequest>,
        ),
        (
            "api/register-agent-response.json",
            round_trip::<RegisterAgentResponse>,
        ),
        (
            "api/registration-token-created.json",
            round_trip::<RegistrationTokenCreated>,
        ),
        ("api/revision.json", round_trip::<Revision>),
        ("api/run.json", round_trip::<Run>),
        ("api/step.json", round_trip::<Step>),
        ("api/workflow.json", round_trip::<Workflow>),
        (
            "diagnostics/repeated-step-key.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/unknown-key.json", round_trip::<Diagnostic>),
        ("events/attempt-created.json", round_trip::<RunEvent>),
        ("events/command-accepted.json", round_trip::<RunEvent>),
        ("events/command-refused.json", round_trip::<RunEvent>),
        ("events/execution-created.json", round_trip::<RunEvent>),
        ("events/execution-status.json", round_trip::<RunEvent>),
        ("events/job-created.json", round_trip::<RunEvent>),
        ("events/job-status.json", round_trip::<RunEvent>),
        ("events/run-created.json", round_trip::<RunEvent>),
        ("events/run-status.json", round_trip::<RunEvent>),
        ("events/step-created.json", round_trip::<RunEvent>),
        ("events/step-status.json", round_trip::<RunEvent>),
        ("ir/two-jobs.json", round_trip::<ir::Workflow>),
    ]
}

fn walk(directory: &Path, root: &Path, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory).expect("the fixtures directory is readable") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            walk(&path, root, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let relative = path.strip_prefix(root).expect("a path under the root");
            found.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn every_fixture_round_trips_without_loss() {
    for (path, check) in fixtures() {
        let value = fixture(path);
        let again = check(&value).unwrap_or_else(|error| panic!("{path}: {error}"));
        assert_eq!(value, again, "{path} changed in a round trip");
    }
}

#[test]
fn every_fixture_is_mapped_to_a_type() {
    let root = repository().join("contracts/fixtures");
    let mut on_disk = BTreeSet::new();
    walk(&root, &root, &mut on_disk);
    let mapped: BTreeSet<String> = fixtures()
        .iter()
        .map(|(path, _)| path.to_string())
        .collect();
    let orphans: Vec<&String> = on_disk.difference(&mapped).collect();
    let missing: Vec<&String> = mapped.difference(&on_disk).collect();
    assert!(
        orphans.is_empty(),
        "fixtures that no test maps to a type: {orphans:?}"
    );
    assert!(
        missing.is_empty(),
        "fixtures the test expects but cannot find: {missing:?}"
    );
}

#[test]
fn every_event_fixture_names_its_kind() {
    for (path, _) in fixtures() {
        if !path.starts_with("events/") {
            continue;
        }
        let value = fixture(path);
        let event: RunEvent =
            serde_json::from_value(value.clone()).unwrap_or_else(|error| panic!("{path}: {error}"));
        assert_eq!(
            value["kind"].as_str(),
            Some(event.event.kind()),
            "{path} carries another kind than its event"
        );
    }
}
