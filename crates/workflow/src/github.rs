use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};

const PUBLIC_SERVER_URL: &str = "https://github.com";
const PUBLIC_API_URL: &str = "https://api.github.com";
const PUBLIC_GRAPHQL_URL: &str = "https://api.github.com/graphql";

const MIRRORED: &[&str] = &[
    "action",
    "action_path",
    "action_ref",
    "action_repository",
    "actor",
    "actor_id",
    "api_url",
    "base_ref",
    "env",
    "event_name",
    "event_path",
    "graphql_url",
    "head_ref",
    "job",
    "output",
    "path",
    "ref",
    "ref_name",
    "ref_protected",
    "ref_type",
    "repository",
    "repository_id",
    "repository_owner",
    "repository_owner_id",
    "retention_days",
    "run_attempt",
    "run_id",
    "run_number",
    "server_url",
    "sha",
    "state",
    "step_summary",
    "triggering_actor",
    "workflow",
    "workflow_ref",
    "workflow_sha",
    "workspace",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    pub repository: Option<Repository>,
    pub forge: Option<Forge>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    pub ref_protected: bool,
    pub workflow: WorkflowFile,
    pub run: Run,
    pub actor: Account,
    pub triggering_actor: String,
    pub job: String,
    pub workspace: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub owner: String,
    pub name: String,
    pub id: u64,
    pub owner_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forge {
    pub server_url: String,
    pub api_url: String,
    pub graphql_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFile {
    pub name: Option<String>,
    pub file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: u64,
    pub number: u64,
    pub attempt: u32,
    pub retention_days: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub login: String,
    pub id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerFacts {
    pub name: String,
    pub os: String,
    pub arch: String,
    pub temp: String,
    pub tool_cache: String,
    pub workspace: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepFacts {
    pub action: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BosunFacts {
    pub url: String,
    pub run_id: String,
    pub run_url: String,
    pub job_id: String,
    pub draft: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dispatch {
    pub inputs: BTreeMap<String, Value>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub workflow: String,
    pub repository: Option<Value>,
    pub organization: Option<Value>,
    pub sender: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryDispatch {
    pub event_type: String,
    pub client_payload: Value,
    pub branch: Option<String>,
    pub repository: Option<Value>,
    pub organization: Option<Value>,
    pub sender: Option<Value>,
    pub installation: Option<Value>,
}

struct Refs {
    git_ref: String,
    sha: String,
    head_ref: String,
    base_ref: String,
}

impl Refs {
    fn of(event: &str, payload: &Value, facts: &Facts) -> Refs {
        let text = |pointer: &str| {
            payload
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let flag = |pointer: &str| payload.pointer(pointer) == Some(&Value::Bool(true));
        let (git_ref, sha) = match event {
            "push" if flag("/deleted") => (None, None),
            "push" => (text("/ref"), text("/after")),
            "create" => (created_ref(payload), None),
            "pull_request" | "pull_request_review" | "pull_request_review_comment" => {
                if flag("/pull_request/merged") {
                    (
                        text("/pull_request/base/ref").map(|base| format!("refs/heads/{base}")),
                        text("/pull_request/merge_commit_sha"),
                    )
                } else {
                    (
                        payload
                            .pointer("/pull_request/number")
                            .and_then(Value::as_u64)
                            .map(|number| format!("refs/pull/{number}/merge")),
                        None,
                    )
                }
            }
            "pull_request_target" => (
                text("/pull_request/base/ref").map(|base| format!("refs/heads/{base}")),
                None,
            ),
            "merge_group" => (text("/merge_group/head_ref"), text("/merge_group/head_sha")),
            "workflow_dispatch" => (text("/ref"), None),
            "release" => (
                text("/release/tag_name").map(|tag| format!("refs/tags/{tag}")),
                None,
            ),
            "deployment" | "deployment_status" => (None, text("/deployment/sha")),
            _ => (None, None),
        };
        let (head_ref, base_ref) = match event {
            "pull_request" | "pull_request_target" => (
                text("/pull_request/head/ref").unwrap_or_default(),
                text("/pull_request/base/ref").unwrap_or_default(),
            ),
            _ => (String::new(), String::new()),
        };
        Refs {
            git_ref: git_ref
                .or_else(|| facts.git_ref.clone())
                .unwrap_or_default(),
            sha: sha.or_else(|| facts.sha.clone()).unwrap_or_default(),
            head_ref,
            base_ref,
        }
    }
}

fn created_ref(payload: &Value) -> Option<String> {
    let name = payload.get("ref").and_then(Value::as_str)?;
    match payload.get("ref_type").and_then(Value::as_str)? {
        "branch" => Some(format!("refs/heads/{name}")),
        "tag" => Some(format!("refs/tags/{name}")),
        _ => None,
    }
}

fn ref_name(git_ref: &str) -> &str {
    ["refs/heads/", "refs/tags/", "refs/pull/"]
        .iter()
        .find_map(|prefix| git_ref.strip_prefix(prefix))
        .unwrap_or(git_ref)
}

fn ref_type(git_ref: &str) -> &'static str {
    if git_ref.starts_with("refs/heads/") || git_ref.starts_with("refs/pull/") {
        "branch"
    } else if git_ref.starts_with("refs/tags/") {
        "tag"
    } else {
        ""
    }
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

fn digits(number: Option<u64>) -> String {
    number.map(|number| number.to_string()).unwrap_or_default()
}

pub fn context(event: &str, payload: &Value, facts: &Facts) -> Map<String, Value> {
    let refs = Refs::of(event, payload, facts);
    let (server_url, api_url, graphql_url) = match &facts.forge {
        Some(forge) => (
            forge.server_url.as_str(),
            forge.api_url.as_str(),
            forge.graphql_url.as_str(),
        ),
        None => (PUBLIC_SERVER_URL, PUBLIC_API_URL, PUBLIC_GRAPHQL_URL),
    };
    let repository = facts.repository.as_ref();
    let full_name = repository
        .map(|repository| format!("{}/{}", repository.owner, repository.name))
        .unwrap_or_default();
    let (repository_url, workflow_ref) = match repository {
        Some(_) => (
            format!("git://{}/{full_name}.git", host(server_url)),
            format!("{full_name}/{}@{}", facts.workflow.file, refs.git_ref),
        ),
        None => (String::new(), String::new()),
    };
    let texts = [
        ("actor", facts.actor.login.clone()),
        ("actor_id", digits(facts.actor.id)),
        ("api_url", api_url.to_string()),
        ("base_ref", refs.base_ref),
        ("event_name", event.to_string()),
        ("graphql_url", graphql_url.to_string()),
        ("head_ref", refs.head_ref),
        ("job", facts.job.clone()),
        ("ref_name", ref_name(&refs.git_ref).to_string()),
        ("ref_type", ref_type(&refs.git_ref).to_string()),
        ("repository", full_name),
        ("repository_id", digits(repository.map(|found| found.id))),
        (
            "repository_owner",
            repository
                .map(|found| found.owner.clone())
                .unwrap_or_default(),
        ),
        (
            "repository_owner_id",
            digits(repository.map(|found| found.owner_id)),
        ),
        ("repositoryUrl", repository_url),
        ("retention_days", facts.run.retention_days.to_string()),
        ("run_attempt", facts.run.attempt.to_string()),
        ("run_id", facts.run.id.to_string()),
        ("run_number", facts.run.number.to_string()),
        ("server_url", server_url.to_string()),
        ("triggering_actor", facts.triggering_actor.clone()),
        (
            "workflow",
            facts
                .workflow
                .name
                .clone()
                .unwrap_or_else(|| facts.workflow.file.clone()),
        ),
        ("workflow_ref", workflow_ref),
        ("workflow_sha", refs.sha.clone()),
        ("workspace", facts.workspace.clone()),
        ("ref", refs.git_ref),
        ("sha", refs.sha),
    ];
    let mut context: Map<String, Value> = texts
        .into_iter()
        .map(|(key, value)| (key.to_string(), Value::String(value)))
        .collect();
    context.insert("event".to_string(), payload.clone());
    context.insert(
        "ref_protected".to_string(),
        Value::Bool(facts.ref_protected),
    );
    context
}

pub fn step_context(
    job: &Map<String, Value>,
    runner: &RunnerFacts,
    step: &StepFacts,
) -> Map<String, Value> {
    let commands = format!("{}/_runner_file_commands", runner.temp);
    let file = |prefix: &str| format!("{commands}/{prefix}_{}", step.id);
    let properties = [
        ("action", step.action.clone()),
        ("action_ref", String::new()),
        ("action_repository", String::new()),
        (
            "event_path",
            format!("{}/_github_workflow/event.json", runner.temp),
        ),
        ("env", file("set_env")),
        ("output", file("set_output")),
        ("path", file("add_path")),
        ("state", file("save_state")),
        ("step_summary", file("step_summary")),
    ];
    let mut context = job.clone();
    for (key, value) in properties {
        context.insert(key.to_string(), Value::String(value));
    }
    context
}

pub fn environment(
    github: &Map<String, Value>,
    runner: &RunnerFacts,
    bosun: &BosunFacts,
) -> BTreeMap<String, String> {
    let mirrors = MIRRORED.iter().filter_map(|property| {
        let value = match github.get(*property)? {
            Value::String(text) => text.clone(),
            Value::Bool(flag) => flag.to_string(),
            _ => return None,
        };
        Some((format!("GITHUB_{}", property.to_ascii_uppercase()), value))
    });
    let fixed = [
        ("CI", "true".to_string()),
        ("GITHUB_ACTIONS", "true".to_string()),
        ("RUNNER_ARCH", runner.arch.clone()),
        ("RUNNER_ENVIRONMENT", "self-hosted".to_string()),
        ("RUNNER_NAME", runner.name.clone()),
        ("RUNNER_OS", runner.os.clone()),
        ("RUNNER_TEMP", runner.temp.clone()),
        ("RUNNER_TOOL_CACHE", runner.tool_cache.clone()),
        ("RUNNER_WORKSPACE", runner.workspace.clone()),
        ("BOSUN_URL", bosun.url.clone()),
        ("BOSUN_RUN_ID", bosun.run_id.clone()),
        ("BOSUN_RUN_URL", bosun.run_url.clone()),
        ("BOSUN_JOB_ID", bosun.job_id.clone()),
        ("BOSUN_DRAFT", bosun.draft.to_string()),
    ];
    fixed
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .chain(mirrors)
        .collect()
}

fn input_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn with_objects(mut payload: Map<String, Value>, objects: &[(&str, &Option<Value>)]) -> Value {
    for (key, object) in objects {
        if let Some(object) = *object {
            payload.insert(key.to_string(), object.clone());
        }
    }
    Value::Object(payload)
}

pub fn dispatch_payload(dispatch: &Dispatch) -> Value {
    let inputs = if dispatch.inputs.is_empty() {
        Value::Null
    } else {
        Value::Object(
            dispatch
                .inputs
                .iter()
                .map(|(name, value)| (name.clone(), Value::String(input_text(value))))
                .collect(),
        )
    };
    let mut payload = Map::new();
    payload.insert("inputs".to_string(), inputs);
    payload.insert(
        "ref".to_string(),
        dispatch.git_ref.clone().map_or(Value::Null, Value::String),
    );
    payload.insert(
        "workflow".to_string(),
        Value::String(dispatch.workflow.clone()),
    );
    with_objects(
        payload,
        &[
            ("repository", &dispatch.repository),
            ("organization", &dispatch.organization),
            ("sender", &dispatch.sender),
        ],
    )
}

pub fn schedule_payload(cron: &str) -> Value {
    let mut payload = Map::new();
    payload.insert("schedule".to_string(), Value::String(cron.to_string()));
    Value::Object(payload)
}

pub fn repository_dispatch_payload(dispatch: &RepositoryDispatch) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "action".to_string(),
        Value::String(dispatch.event_type.clone()),
    );
    payload.insert(
        "branch".to_string(),
        dispatch.branch.clone().map_or(Value::Null, Value::String),
    );
    payload.insert(
        "client_payload".to_string(),
        dispatch.client_payload.clone(),
    );
    with_objects(
        payload,
        &[
            ("repository", &dispatch.repository),
            ("organization", &dispatch.organization),
            ("sender", &dispatch.sender),
            ("installation", &dispatch.installation),
        ],
    )
}
