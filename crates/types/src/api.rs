use std::borrow::Cow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::Actor;
use crate::id::{
    AgentId, AttemptId, ExecutionId, JobId, OrganisationId, PoolId, ProjectId, RegistrationTokenId,
    RevisionId, RunId, StepId, UserId, WorkflowId,
};
use crate::key::JobKey;
use crate::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, RunConclusion, RunStatus,
    StatusReason, StepStatus, WaitingReason,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

enum Cursor {}

impl JsonSchema for Cursor {
    fn schema_name() -> Cow<'static, str> {
        "Page".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "bosun_types::api::Page".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "required": ["items", "next"],
            "properties": {
                "items": { "type": "array" },
                "next": { "type": ["string", "null"] }
            }
        })
    }
}

impl<T: JsonSchema> JsonSchema for Page<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        format!("Page_{}", T::schema_name()).into()
    }

    fn schema_id() -> Cow<'static, str> {
        format!("bosun_types::api::Page<{}>", T::schema_id()).into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "allOf": [
                generator.subschema_for::<Cursor>(),
                { "properties": { "items": { "items": generator.subschema_for::<T>() } } }
            ]
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct User {
    pub id: UserId,
    pub login: String,
    pub name: String,
    pub email: Option<String>,
    pub owner: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Me {
    pub user: User,
    pub csrf_token: Option<String>,
    pub password_change_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignInRequest {
    pub login: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PasswordChangeRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignInMethods {
    pub password: bool,
    pub providers: Vec<SignInProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignInProvider {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Organisation {
    pub id: OrganisationId,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateOrganisationRequest {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub id: ProjectId,
    pub organisation_id: OrganisationId,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateProjectRequest {
    pub organisation: String,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowSource {
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Workflow {
    pub id: WorkflowId,
    pub project_id: ProjectId,
    pub name: String,
    pub source: WorkflowSource,
    pub enabled: bool,
    pub revision_id: Option<RevisionId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateWorkflowRequest {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Revision {
    pub id: RevisionId,
    pub workflow_id: WorkflowId,
    pub sha256: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateSourceRequest {
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DispatchRequest {
    pub r#ref: Option<String>,
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub id: RunId,
    pub project_id: ProjectId,
    pub workflow_id: WorkflowId,
    pub revision_id: RevisionId,
    pub number: u64,
    pub global_number: u64,
    pub event: String,
    pub status: RunStatus,
    pub conclusion: Option<RunConclusion>,
    pub attempt: u32,
    pub actor: Actor,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    pub id: JobId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub key: JobKey,
    pub name: String,
    pub needs: Vec<JobKey>,
    pub labels: Vec<String>,
    pub status: JobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub conclusion: Option<Conclusion>,
    pub conclusion_source: Option<ConclusionSource>,
    pub execution_id: Option<ExecutionId>,
    pub queued_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Execution {
    pub id: ExecutionId,
    pub job_id: JobId,
    pub number: u32,
    pub agent_id: AgentId,
    pub status: ExecutionStatus,
    pub conclusion: Option<Conclusion>,
    pub reason: Option<StatusReason>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    pub id: StepId,
    pub execution_id: ExecutionId,
    pub index: u32,
    pub name: String,
    pub status: StepStatus,
    pub outcome: Option<Conclusion>,
    pub conclusion: Option<Conclusion>,
    pub conclusion_source: Option<ConclusionSource>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogLine {
    pub number: u64,
    pub line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogEnd {
    pub lines: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Trusted,
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Pool {
    pub id: PoolId,
    pub name: String,
    pub trust: TrustLevel,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Shell,
    Podman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub pool_id: PoolId,
    pub labels: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub version: Option<String>,
    pub online: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub execution_id: Option<ExecutionId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegistrationTokenRequest {
    pub pool: String,
    pub reusable: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegistrationTokenCreated {
    pub id: RegistrationTokenId,
    pub pool_id: PoolId,
    pub token: String,
    pub reusable: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegisterAgentRequest {
    pub token: String,
    pub name: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegisterAgentResponse {
    pub agent_id: AgentId,
    pub credential: String,
}
