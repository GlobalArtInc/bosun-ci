use std::collections::BTreeMap;

use bosun_types::ir::{ActionMetadata, ActionReference, JobTemplate, Shell};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::agent::v1::Assignment;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contexts {
    pub github: Map<String, Value>,
    pub needs: Map<String, Value>,
    pub strategy: Value,
    pub matrix: Value,
    pub inputs: Map<String, Value>,
    pub vars: Map<String, Value>,
    pub secrets: Map<String, Value>,
    pub bosun: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluated {
    pub env: BTreeMap<String, String>,
    pub shell: Option<Shell>,
    pub working_directory: Option<String>,
    pub timeout_minutes: u32,
    pub continue_on_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub uses: ActionReference,
    pub commit: String,
    pub download: String,
    pub metadata: ActionMetadata,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Documents {
    pub job: JobTemplate,
    pub contexts: Contexts,
    pub evaluated: Evaluated,
    pub actions: Vec<Action>,
}

impl Documents {
    pub fn read(assignment: &Assignment) -> Result<Documents, String> {
        Ok(Documents {
            job: document("job", &assignment.job)?,
            contexts: document("contexts", &assignment.contexts)?,
            evaluated: document("evaluated", &assignment.evaluated)?,
            actions: document("actions", &assignment.actions)?,
        })
    }

    pub fn write(&self, assignment: &mut Assignment) -> Result<(), serde_json::Error> {
        assignment.job = serde_json::to_string(&self.job)?;
        assignment.contexts = serde_json::to_string(&self.contexts)?;
        assignment.evaluated = serde_json::to_string(&self.evaluated)?;
        assignment.actions = serde_json::to_string(&self.actions)?;
        Ok(())
    }
}

fn document<T: DeserializeOwned>(name: &str, text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|error| {
        format!("the document `{name}` of the assignment is not one this agent reads: {error}")
    })
}
