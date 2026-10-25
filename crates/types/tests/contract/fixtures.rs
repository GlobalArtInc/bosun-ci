use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use bosun_types::api::{
    AddMemberRequest, Agent, CreateOrganisationRequest, CreatePersonalTokenRequest,
    CreatePoolRequest, CreateProjectRequest, CreateRoleBindingRequest, CreateRoleRequest,
    CreateTeamRequest, CreateUserRequest, CreateWorkflowRequest, DispatchRequest, Execution, Job,
    LabelAlias, LogEnd, LogLine, Me, Organisation, Page, PasswordChangeRequest, PersonalToken,
    PersonalTokenCreated, Pool, Project, ProjectSettings, QueuedJob, RegisterAgentRequest,
    RegisterAgentResponse, RegistrationTokenCreated, RegistrationTokenRequest,
    RepositoryDispatchRequest, Revision, Role, RoleBinding, Run, Secret, SetLabelAliasRequest,
    SetSecretRequest, SetVariableRequest, SignInMethods, SignInRequest, Step, Team,
    UpdateAgentRequest, UpdateOrganisationRequest, UpdateProjectRequest, UpdateSourceRequest,
    UpdateWorkflowRequest, User, Variable, Workflow,
};
use bosun_types::command::CommandRequest;
use bosun_types::diagnostic::Diagnostic;
use bosun_types::error::ErrorEnvelope;
use bosun_types::event::{RunEvent, StreamEvent};
use bosun_types::ir;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{fixture, repository};

type Check = fn(&Value) -> Result<Value, String>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionCase {
    metadata: Option<ir::ActionMetadata>,
    diagnostics: Vec<Diagnostic>,
}

const RUN_BY_OTHER_SUITES: &[&str] = &[
    "expr",
    "github",
    "hash-files",
    "matchers",
    "matrix",
    "names",
    "pushes",
    "runs",
    "triggers",
];

fn round_trip<T: Serialize + DeserializeOwned>(value: &Value) -> Result<Value, String> {
    let typed: T = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    serde_json::to_value(&typed).map_err(|error| error.to_string())
}

fn fixtures() -> Vec<(&'static str, Check)> {
    vec![
        ("actions/actions-cache-v4.json", round_trip::<ActionCase>),
        ("actions/actions-checkout-v4.json", round_trip::<ActionCase>),
        (
            "actions/actions-github-script-v7.json",
            round_trip::<ActionCase>,
        ),
        (
            "actions/actions-rs-toolchain-v1.json",
            round_trip::<ActionCase>,
        ),
        (
            "actions/actions-setup-node-v3.json",
            round_trip::<ActionCase>,
        ),
        ("actions/hooks.json", round_trip::<ActionCase>),
        (
            "actions/input-default-secrets.json",
            round_trip::<ActionCase>,
        ),
        ("actions/main-expression.json", round_trip::<ActionCase>),
        (
            "actions/marocchino-sticky-pull-request-comment-v2.json",
            round_trip::<ActionCase>,
        ),
        (
            "actions/oven-sh-setup-bun-v2.json",
            round_trip::<ActionCase>,
        ),
        (
            "actions/pre-if-steps-context.json",
            round_trip::<ActionCase>,
        ),
        ("actions/runs-without-main.json", round_trip::<ActionCase>),
        (
            "actions/softprops-action-gh-release-v2.json",
            round_trip::<ActionCase>,
        ),
        (
            "actions/step-security-harden-runner-v2.json",
            round_trip::<ActionCase>,
        ),
        (
            "actions/swatinem-rust-cache-v2.json",
            round_trip::<ActionCase>,
        ),
        ("actions/unknown-key-in-runs.json", round_trip::<ActionCase>),
        ("actions/using-composite.json", round_trip::<ActionCase>),
        ("actions/using-docker.json", round_trip::<ActionCase>),
        ("actions/using-ignores-case.json", round_trip::<ActionCase>),
        (
            "actions/using-unknown-runtime.json",
            round_trip::<ActionCase>,
        ),
        (
            "api/add-member-request.json",
            round_trip::<AddMemberRequest>,
        ),
        ("api/agent.json", round_trip::<Agent>),
        ("api/command-job-cancel.json", round_trip::<CommandRequest>),
        ("api/command-run-cancel.json", round_trip::<CommandRequest>),
        (
            "api/create-organisation-request.json",
            round_trip::<CreateOrganisationRequest>,
        ),
        (
            "api/create-personal-token-request.json",
            round_trip::<CreatePersonalTokenRequest>,
        ),
        (
            "api/create-pool-request.json",
            round_trip::<CreatePoolRequest>,
        ),
        (
            "api/create-project-request.json",
            round_trip::<CreateProjectRequest>,
        ),
        (
            "api/create-role-binding-request.json",
            round_trip::<CreateRoleBindingRequest>,
        ),
        (
            "api/create-role-request.json",
            round_trip::<CreateRoleRequest>,
        ),
        (
            "api/create-team-request.json",
            round_trip::<CreateTeamRequest>,
        ),
        (
            "api/create-user-request.json",
            round_trip::<CreateUserRequest>,
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
        ("api/label-alias.json", round_trip::<LabelAlias>),
        ("api/log-end.json", round_trip::<LogEnd>),
        ("api/log-line.json", round_trip::<LogLine>),
        ("api/me.json", round_trip::<Me>),
        ("api/organisation.json", round_trip::<Organisation>),
        ("api/page-jobs.json", round_trip::<Page<Job>>),
        (
            "api/password-change-request.json",
            round_trip::<PasswordChangeRequest>,
        ),
        (
            "api/personal-token-created.json",
            round_trip::<PersonalTokenCreated>,
        ),
        ("api/personal-token.json", round_trip::<PersonalToken>),
        ("api/pool.json", round_trip::<Pool>),
        ("api/project.json", round_trip::<Project>),
        ("api/project-settings.json", round_trip::<ProjectSettings>),
        ("api/queued-job.json", round_trip::<QueuedJob>),
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
        (
            "api/repository-dispatch-request.json",
            round_trip::<RepositoryDispatchRequest>,
        ),
        ("api/revision.json", round_trip::<Revision>),
        ("api/role-binding.json", round_trip::<RoleBinding>),
        ("api/role.json", round_trip::<Role>),
        ("api/run-actions.json", round_trip::<Run>),
        ("api/run.json", round_trip::<Run>),
        ("api/run-startup-failure.json", round_trip::<Run>),
        ("api/secret.json", round_trip::<Secret>),
        (
            "api/set-label-alias-request.json",
            round_trip::<SetLabelAliasRequest>,
        ),
        (
            "api/set-secret-request.json",
            round_trip::<SetSecretRequest>,
        ),
        (
            "api/set-variable-request.json",
            round_trip::<SetVariableRequest>,
        ),
        ("api/sign-in-methods.json", round_trip::<SignInMethods>),
        ("api/sign-in-request.json", round_trip::<SignInRequest>),
        ("api/step.json", round_trip::<Step>),
        ("api/team.json", round_trip::<Team>),
        (
            "api/update-agent-request.json",
            round_trip::<UpdateAgentRequest>,
        ),
        (
            "api/update-organisation-request.json",
            round_trip::<UpdateOrganisationRequest>,
        ),
        (
            "api/update-project-request.json",
            round_trip::<UpdateProjectRequest>,
        ),
        (
            "api/update-source-request.json",
            round_trip::<UpdateSourceRequest>,
        ),
        (
            "api/update-workflow-request.json",
            round_trip::<UpdateWorkflowRequest>,
        ),
        ("api/user.json", round_trip::<User>),
        ("api/variable.json", round_trip::<Variable>),
        ("api/workflow.json", round_trip::<Workflow>),
        ("diagnostics/alias-bomb.json", round_trip::<Diagnostic>),
        ("diagnostics/all-secrets.json", round_trip::<Diagnostic>),
        (
            "diagnostics/bosun-context-in-github-workflows.json",
            round_trip::<Diagnostic>,
        ),
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
        (
            "diagnostics/untrusted-checkout.json",
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
        ("events/agent-status.json", round_trip::<StreamEvent>),
        ("events/attempt-created.json", round_trip::<RunEvent>),
        ("events/command-accepted.json", round_trip::<RunEvent>),
        ("events/command-refused.json", round_trip::<RunEvent>),
        ("events/execution-created.json", round_trip::<RunEvent>),
        ("events/execution-status.json", round_trip::<RunEvent>),
        ("events/job-created.json", round_trip::<RunEvent>),
        ("events/job-created-member.json", round_trip::<RunEvent>),
        ("events/job-status.json", round_trip::<RunEvent>),
        ("events/queue-job.json", round_trip::<StreamEvent>),
        ("events/run-created.json", round_trip::<RunEvent>),
        ("events/run-notice.json", round_trip::<RunEvent>),
        ("events/run-status.json", round_trip::<RunEvent>),
        ("events/step-created.json", round_trip::<RunEvent>),
        ("events/step-status.json", round_trip::<RunEvent>),
        ("ir/aliases.json", round_trip::<ir::Workflow>),
        ("ir/all-secrets.json", round_trip::<ir::Workflow>),
        (
            "ir/bosun-context-in-github-workflows.json",
            round_trip::<ir::Workflow>,
        ),
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
        ("ir/untrusted-checkout.json", round_trip::<ir::Workflow>),
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
        let kind = match serde_json::from_value::<RunEvent>(value.clone()) {
            Ok(event) => event.event.kind(),
            Err(_) => serde_json::from_value::<StreamEvent>(value.clone())
                .unwrap_or_else(|error| panic!("{path}: {error}"))
                .change
                .kind(),
        };
        assert_eq!(
            value["kind"].as_str(),
            Some(kind),
            "{path} carries another kind than its event"
        );
    }
}
