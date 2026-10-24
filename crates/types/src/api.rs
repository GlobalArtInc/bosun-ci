use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::Actor;
use crate::id::{
    AgentId, AttemptId, ExecutionId, JobId, OrganisationId, PoolId, ProjectId, RegistrationTokenId,
    RevisionId, RunId, StepId, WorkflowId,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organisation {
    pub id: OrganisationId,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub organisation_id: OrganisationId,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowSource {
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: WorkflowId,
    pub project_id: ProjectId,
    pub name: String,
    pub source: WorkflowSource,
    pub enabled: bool,
    pub revision_id: Option<RevisionId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub id: RevisionId,
    pub workflow_id: WorkflowId,
    pub sha256: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRequest {
    pub r#ref: Option<String>,
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: AttemptId,
    pub run_id: RunId,
    pub number: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub queued_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Trusted,
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pool {
    pub id: PoolId,
    pub name: String,
    pub trust: TrustLevel,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Shell,
    Podman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationTokenCreated {
    pub id: RegistrationTokenId,
    pub pool_id: PoolId,
    pub token: String,
    pub reusable: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAgentRequest {
    pub token: String,
    pub name: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAgentResponse {
    pub agent_id: AgentId,
    pub credential: String,
}
