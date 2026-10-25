use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    InProgress,
    Waiting,
    Paused,
    Completed,
}

impl RunStatus {
    pub const ALL: [RunStatus; 5] = [
        RunStatus::Queued,
        RunStatus::InProgress,
        RunStatus::Waiting,
        RunStatus::Paused,
        RunStatus::Completed,
    ];

    pub fn can_transition(self, to: RunStatus) -> bool {
        use RunStatus::*;
        matches!(
            (self, to),
            (Queued, InProgress | Waiting | Paused | Completed)
                | (InProgress, Waiting | Paused | Completed)
                | (Waiting, Queued | InProgress | Paused | Completed)
                | (Paused, Queued | InProgress | Waiting | Completed)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Waiting,
    Queued,
    Assigned,
    InProgress,
    Paused,
    Held,
    Completed,
}

impl JobStatus {
    pub const ALL: [JobStatus; 8] = [
        JobStatus::Pending,
        JobStatus::Waiting,
        JobStatus::Queued,
        JobStatus::Assigned,
        JobStatus::InProgress,
        JobStatus::Paused,
        JobStatus::Held,
        JobStatus::Completed,
    ];

    pub fn can_transition(self, to: JobStatus) -> bool {
        use JobStatus::*;
        matches!(
            (self, to),
            (Pending, Waiting | Queued | Completed)
                | (Waiting, Queued | Completed)
                | (Queued, Waiting | Assigned | Completed)
                | (Assigned, Queued | InProgress | Completed)
                | (InProgress, Paused | Held | Queued | Completed)
                | (Paused, InProgress | Completed)
                | (Held, InProgress | Completed)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Assigned,
    InProgress,
    Paused,
    Held,
    Completed,
}

impl ExecutionStatus {
    pub const ALL: [ExecutionStatus; 5] = [
        ExecutionStatus::Assigned,
        ExecutionStatus::InProgress,
        ExecutionStatus::Paused,
        ExecutionStatus::Held,
        ExecutionStatus::Completed,
    ];

    pub fn can_transition(self, to: ExecutionStatus) -> bool {
        use ExecutionStatus::*;
        matches!(
            (self, to),
            (Assigned, InProgress | Completed)
                | (InProgress, Paused | Held | Completed)
                | (Paused | Held, InProgress | Completed)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Paused,
    Completed,
}

impl StepStatus {
    pub const ALL: [StepStatus; 4] = [
        StepStatus::Pending,
        StepStatus::InProgress,
        StepStatus::Paused,
        StepStatus::Completed,
    ];

    pub fn can_transition(self, to: StepStatus) -> bool {
        use StepStatus::*;
        matches!(
            (self, to),
            (Pending, InProgress | Paused | Completed)
                | (Paused, InProgress | Completed)
                | (InProgress, Completed)
                | (Completed, InProgress)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunConclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
    StartupFailure,
}

impl From<Conclusion> for RunConclusion {
    fn from(conclusion: Conclusion) -> RunConclusion {
        match conclusion {
            Conclusion::Success => RunConclusion::Success,
            Conclusion::Failure => RunConclusion::Failure,
            Conclusion::Cancelled => RunConclusion::Cancelled,
            Conclusion::Skipped => RunConclusion::Skipped,
            Conclusion::TimedOut => RunConclusion::TimedOut,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConclusionSource {
    Natural,
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitingReason {
    Untrusted,
    Approval,
    WaitTimer,
    Policy,
    Concurrency,
    MaxParallel,
    Paused,
}

impl WaitingReason {
    pub const ALL: [WaitingReason; 7] = [
        WaitingReason::Untrusted,
        WaitingReason::Approval,
        WaitingReason::WaitTimer,
        WaitingReason::Policy,
        WaitingReason::Concurrency,
        WaitingReason::MaxParallel,
        WaitingReason::Paused,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusReason {
    IfFalse,
    EvaluationFailed,
    Command,
    LeaseExpired,
    RejectedByAgent,
    NoAgent,
    AgentLost,
    AgentRemoved,
}

impl StatusReason {
    pub const ALL: [StatusReason; 8] = [
        StatusReason::IfFalse,
        StatusReason::EvaluationFailed,
        StatusReason::Command,
        StatusReason::LeaseExpired,
        StatusReason::RejectedByAgent,
        StatusReason::NoAgent,
        StatusReason::AgentLost,
        StatusReason::AgentRemoved,
    ];
}
