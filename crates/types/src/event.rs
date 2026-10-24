use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::command::CommandRequest;
use crate::error::ErrorCode;
use crate::id::{AgentId, AttemptId, ExecutionId, JobId, RevisionId, RunId, StepId, WorkflowId};
use crate::key::JobKey;
use crate::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, RunConclusion, RunStatus,
    StatusReason, StepStatus, WaitingReason,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    pub seq: u64,
    pub run_id: RunId,
    pub attempt_id: Option<AttemptId>,
    pub job_id: Option<JobId>,
    pub execution_id: Option<ExecutionId>,
    pub step_id: Option<StepId>,
    pub actor: Actor,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Event {
    #[serde(rename = "run.created")]
    RunCreated(RunCreated),
    #[serde(rename = "attempt.created")]
    AttemptCreated(AttemptCreated),
    #[serde(rename = "job.created")]
    JobCreated(JobCreated),
    #[serde(rename = "execution.created")]
    ExecutionCreated(ExecutionCreated),
    #[serde(rename = "step.created")]
    StepCreated(StepCreated),
    #[serde(rename = "run.status")]
    RunStatusChanged(RunStatusChanged),
    #[serde(rename = "job.status")]
    JobStatusChanged(JobStatusChanged),
    #[serde(rename = "execution.status")]
    ExecutionStatusChanged(ExecutionStatusChanged),
    #[serde(rename = "step.status")]
    StepStatusChanged(StepStatusChanged),
    #[serde(rename = "command.accepted")]
    CommandAccepted(CommandAccepted),
    #[serde(rename = "command.refused")]
    CommandRefused(CommandRefused),
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::RunCreated(_) => "run.created",
            Event::AttemptCreated(_) => "attempt.created",
            Event::JobCreated(_) => "job.created",
            Event::ExecutionCreated(_) => "execution.created",
            Event::StepCreated(_) => "step.created",
            Event::RunStatusChanged(_) => "run.status",
            Event::JobStatusChanged(_) => "job.status",
            Event::ExecutionStatusChanged(_) => "execution.status",
            Event::StepStatusChanged(_) => "step.status",
            Event::CommandAccepted(_) => "command.accepted",
            Event::CommandRefused(_) => "command.refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCreated {
    pub workflow_id: WorkflowId,
    pub revision_id: RevisionId,
    pub number: u64,
    pub global_number: u64,
    pub event: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptCreated {
    pub number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCreated {
    pub key: JobKey,
    pub name: String,
    pub needs: Vec<JobKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCreated {
    pub number: u32,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCreated {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatusChanged {
    pub from: RunStatus,
    pub to: RunStatus,
    pub conclusion: Option<RunConclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatusChanged {
    pub from: JobStatus,
    pub to: JobStatus,
    pub waiting_reason: Option<WaitingReason>,
    pub conclusion: Option<Conclusion>,
    pub conclusion_source: Option<ConclusionSource>,
    pub reason: Option<StatusReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStatusChanged {
    pub from: ExecutionStatus,
    pub to: ExecutionStatus,
    pub conclusion: Option<Conclusion>,
    pub reason: Option<StatusReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepStatusChanged {
    pub from: StepStatus,
    pub to: StepStatus,
    pub outcome: Option<Conclusion>,
    pub conclusion: Option<Conclusion>,
    pub conclusion_source: Option<ConclusionSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAccepted {
    pub command: CommandRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRefused {
    pub command: CommandRequest,
    pub code: ErrorCode,
    pub message: String,
}
