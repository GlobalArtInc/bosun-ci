use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{JobId, StepId};
use crate::permission::Permission;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "args")]
pub enum Command {
    #[serde(rename = "run.cancel")]
    RunCancel,
    #[serde(rename = "job.cancel")]
    JobCancel,
}

impl Command {
    pub fn permission(&self) -> Permission {
        match self {
            Command::RunCancel | Command::JobCancel => Permission::RunCancel,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    pub attempt: Option<u32>,
    pub job: Option<JobId>,
    pub step: Option<StepId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandRequest {
    #[serde(flatten)]
    pub command: Command,
    pub target: Target,
    pub reason: Option<String>,
}
