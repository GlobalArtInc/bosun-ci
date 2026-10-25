use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::Actor;
use crate::diagnostic::Diagnostic;
use crate::id::{
    AgentId, AttemptId, ExecutionId, JobId, LabelAliasId, OrganisationId, PoolId, ProjectId,
    RegistrationTokenId, RevisionId, RunId, StepId, UserId, WorkflowId,
};
use crate::key::JobKey;
use crate::permission::Permission;
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
    pub permissions: Permissions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Permissions {
    pub installation: BTreeSet<Permission>,
    #[schemars(extend("propertyNames" = { "format": "uuid" }))]
    pub organisations: BTreeMap<OrganisationId, BTreeSet<Permission>>,
    #[schemars(extend("propertyNames" = { "format": "uuid" }))]
    pub projects: BTreeMap<ProjectId, BTreeSet<Permission>>,
}

impl Permissions {
    pub fn everything() -> Permissions {
        Permissions {
            installation: Permission::ALL.into_iter().collect(),
            organisations: BTreeMap::new(),
            projects: BTreeMap::new(),
        }
    }

    pub fn on_organisation(&self, organisation: OrganisationId) -> BTreeSet<Permission> {
        let mut held = self.installation.clone();
        if let Some(granted) = self.organisations.get(&organisation) {
            held.extend(granted);
        }
        held
    }

    pub fn on_project(
        &self,
        organisation: OrganisationId,
        project: ProjectId,
    ) -> BTreeSet<Permission> {
        let mut held = self.on_organisation(organisation);
        if let Some(granted) = self.projects.get(&project) {
            held.extend(granted);
        }
        held
    }
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
pub struct ProjectSettings {
    pub github_workflows: bool,
    pub fork_policy: ForkPolicy,
    pub token_permissions: BTreeMap<ForgePermission, ForgeAccess>,
    pub report: Reporting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForkPolicy {
    Disabled,
    Approval,
    Automatic,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ForgePermission {
    Actions,
    Attestations,
    Checks,
    Contents,
    Deployments,
    Discussions,
    Issues,
    Metadata,
    Models,
    Packages,
    Pages,
    PullRequests,
    RepositoryProjects,
    SecurityEvents,
    Statuses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForgeAccess {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reporting {
    Checks,
    Statuses,
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
    pub display_name: String,
    pub event: String,
    pub status: RunStatus,
    pub conclusion: Option<RunConclusion>,
    pub attempt: u32,
    pub actor: Actor,
    pub notices: Vec<Notice>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Notice {
    pub job_id: Option<JobId>,
    #[serde(flatten)]
    pub diagnostic: Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    pub id: JobId,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub key: JobKey,
    pub name: String,
    pub group_id: Option<JobId>,
    pub matrix: Option<BTreeMap<String, Value>>,
    pub needs: Vec<JobKey>,
    pub labels: Vec<String>,
    pub status: JobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub conclusion: Option<Conclusion>,
    pub conclusion_source: Option<ConclusionSource>,
    pub outputs: BTreeMap<String, String>,
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
    pub access: PoolAccess,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PoolAccess {
    pub all_projects: bool,
    pub organisations: Vec<PoolOrganisation>,
    pub projects: Vec<PoolProject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PoolOrganisation {
    pub id: OrganisationId,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PoolProject {
    pub id: ProjectId,
    pub organisation_id: OrganisationId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreatePoolRequest {
    pub name: String,
    pub trust: TrustLevel,
    pub access: PoolAccessRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PoolAccessRequest {
    pub all_projects: bool,
    pub organisations: Vec<String>,
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LabelAlias {
    pub id: LabelAliasId,
    pub label: String,
    pub organisation_id: Option<OrganisationId>,
    pub pool_id: Option<PoolId>,
    pub capability: Option<Capability>,
    pub image: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetLabelAliasRequest {
    pub label: String,
    pub organisation: Option<String>,
    pub pool: Option<String>,
    pub capability: Option<Capability>,
    pub image: Option<String>,
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
