use serde::{Deserialize, Serialize};

use crate::id::{JobId, StepId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "args")]
pub enum Command {
    #[serde(rename = "run.cancel")]
    RunCancel,
    #[serde(rename = "job.cancel")]
    JobCancel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub attempt: Option<u32>,
    pub job: Option<JobId>,
    pub step: Option<StepId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRequest {
    #[serde(flatten)]
    pub command: Command,
    pub target: Target,
    pub reason: Option<String>,
}
