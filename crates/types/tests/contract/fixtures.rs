use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use bosun_types::api::{
    Agent, Attempt, CreateOrganisationRequest, CreateProjectRequest, CreateWorkflowRequest,
    DispatchRequest, Execution, Job, Me, Organisation, Page, Pool, Project, RegisterAgentRequest,
    RegisterAgentResponse, RegistrationTokenCreated, RegistrationTokenRequest, Revision, Run, Step,
    UpdateSourceRequest, Workflow,
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

const RUN_BY_OTHER_SUITES: &[&str] = &["expr", "github", "triggers"];

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
        (
            "api/create-organisation-request.json",
            round_trip::<CreateOrganisationRequest>,
        ),
        (
            "api/create-project-request.json",
            round_trip::<CreateProjectRequest>,
        ),
        (
            "api/create-workflow-request.json",
            round_trip::<CreateWorkflowRequest>,
        ),
        ("api/dispatch.json", round_trip::<DispatchRequest>),
        ("api/error-invalid-input.json", round_trip::<ErrorEnvelope>),
        ("api/error-invalid-state.json", round_trip::<ErrorEnvelope>),
        ("api/execution.json", round_trip::<Execution>),
        ("api/job.json", round_trip::<Job>),
        ("api/me.json", round_trip::<Me>),
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
        (
            "api/registration-token-request.json",
            round_trip::<RegistrationTokenRequest>,
        ),
        ("api/revision.json", round_trip::<Revision>),
        ("api/run.json", round_trip::<Run>),
        ("api/step.json", round_trip::<Step>),
        (
            "api/update-source-request.json",
            round_trip::<UpdateSourceRequest>,
        ),
        ("api/workflow.json", round_trip::<Workflow>),
        ("diagnostics/alias-bomb.json", round_trip::<Diagnostic>),
        ("diagnostics/all-secrets.json", round_trip::<Diagnostic>),
        (
            "diagnostics/branches-and-branches-ignore.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/choice-default-outside-options.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/continue-on-error-string.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/defaults-run-expression.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/duplicate-key.json", round_trip::<Diagnostic>),
        (
            "diagnostics/expression-syntax.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/invalid-cron.json", round_trip::<Diagnostic>),
        ("diagnostics/invalid-job-key.json", round_trip::<Diagnostic>),
        (
            "diagnostics/invalid-step-key.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/matrix-in-workflow-env.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/merge-key.json", round_trip::<Diagnostic>),
        ("diagnostics/missing-runs-on.json", round_trip::<Diagnostic>),
        ("diagnostics/needs-cycle.json", round_trip::<Diagnostic>),
        ("diagnostics/needs-mapping.json", round_trip::<Diagnostic>),
        (
            "diagnostics/number-default-not-a-number.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/permissions-id-token-read.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/permissions-models-write.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/permissions-unknown-scope.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/queue-max-cancel-in-progress.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/repeated-step-key.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/run-and-uses.json", round_trip::<Diagnostic>),
        (
            "diagnostics/runs-on-steps-context.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/shell-on-uses-step.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/shell-without-placeholder.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/step-shell-expression.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/template-condition.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/timeout-minutes-fraction.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/timeout-minutes-string.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/unavailable-function.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/unknown-context.json", round_trip::<Diagnostic>),
        (
            "diagnostics/unknown-function-in-step-if.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/unknown-key.json", round_trip::<Diagnostic>),
        ("diagnostics/unknown-needs.json", round_trip::<Diagnostic>),
        (
            "diagnostics/unknown-time-zone.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/uses-expression.json", round_trip::<Diagnostic>),
        (
            "diagnostics/uses-local-action.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/uses-without-ref.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/with-on-run-step.json",
            round_trip::<Diagnostic>,
        ),
        (
            "diagnostics/wrong-argument-count.json",
            round_trip::<Diagnostic>,
        ),
        ("diagnostics/yaml-syntax.json", round_trip::<Diagnostic>),
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
        ("ir/aliases.json", round_trip::<ir::Workflow>),
        ("ir/all-secrets.json", round_trip::<ir::Workflow>),
        ("ir/concurrency.json", round_trip::<ir::Workflow>),
        ("ir/continue-on-error.json", round_trip::<ir::Workflow>),
        ("ir/defaults.json", round_trip::<ir::Workflow>),
        ("ir/flow-style.json", round_trip::<ir::Workflow>),
        ("ir/job-if.json", round_trip::<ir::Workflow>),
        ("ir/job-outputs.json", round_trip::<ir::Workflow>),
        ("ir/on-generic-events.json", round_trip::<ir::Workflow>),
        ("ir/on-list.json", round_trip::<ir::Workflow>),
        ("ir/on-merge-group.json", round_trip::<ir::Workflow>),
        ("ir/on-pull-request.json", round_trip::<ir::Workflow>),
        ("ir/on-push-ignore.json", round_trip::<ir::Workflow>),
        ("ir/on-push.json", round_trip::<ir::Workflow>),
        ("ir/on-repository-dispatch.json", round_trip::<ir::Workflow>),
        ("ir/on-schedule.json", round_trip::<ir::Workflow>),
        ("ir/on-string.json", round_trip::<ir::Workflow>),
        ("ir/on-workflow-dispatch.json", round_trip::<ir::Workflow>),
        ("ir/permissions.json", round_trip::<ir::Workflow>),
        ("ir/run-name.json", round_trip::<ir::Workflow>),
        ("ir/runs-on-expressions.json", round_trip::<ir::Workflow>),
        ("ir/scalars.json", round_trip::<ir::Workflow>),
        ("ir/secrets.json", round_trip::<ir::Workflow>),
        ("ir/step-if.json", round_trip::<ir::Workflow>),
        ("ir/step-uses.json", round_trip::<ir::Workflow>),
        ("ir/strategy.json", round_trip::<ir::Workflow>),
        ("ir/template-condition.json", round_trip::<ir::Workflow>),
        ("ir/timeout-minutes.json", round_trip::<ir::Workflow>),
        ("ir/two-jobs.json", round_trip::<ir::Workflow>),
    ]
}

fn walk(directory: &Path, root: &Path, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory).expect("the fixtures directory is readable") {
        let path = entry.expect("a directory entry").path();
        let relative = path.strip_prefix(root).expect("a path under the root");
        if path.is_dir() {
            if !RUN_BY_OTHER_SUITES
                .iter()
                .any(|directory| relative == Path::new(directory))
            {
                walk(&path, root, found);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
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
