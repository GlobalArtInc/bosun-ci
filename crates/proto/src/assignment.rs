use std::collections::BTreeMap;
use std::fmt;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_privileged: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub workspace_clean: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub untrusted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_podman: bool,
}

fn is_false(flag: &bool) -> bool {
    !*flag
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub image: String,
    pub credentials: Option<Credentials>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub username: Option<String>,
    pub password: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub token: String,
    pub runtime_url: String,
    pub results_url: String,
    pub cache_url: String,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("runtime_url", &self.runtime_url)
            .field("results_url", &self.results_url)
            .field("cache_url", &self.cache_url)
            .finish_non_exhaustive()
    }
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Container, Credentials, Evaluated};

    fn evaluated() -> Evaluated {
        Evaluated {
            env: Default::default(),
            shell: None,
            working_directory: None,
            timeout_minutes: 360,
            continue_on_error: false,
            runtime: None,
            container: None,
            allow_privileged: false,
            workspace_clean: false,
            untrusted: false,
            needs_podman: false,
        }
    }

    #[test]
    fn a_job_without_a_container_or_a_cleaning_carries_none_of_the_new_keys() {
        let written: Value = serde_json::to_value(evaluated()).expect("JSON");
        assert_eq!(
            written,
            json!({
                "env": {},
                "shell": null,
                "working_directory": null,
                "timeout_minutes": 360,
                "continue_on_error": false
            })
        );
        let read: Evaluated = serde_json::from_value(written).expect("a document");
        assert_eq!(read, evaluated());
    }

    #[test]
    fn the_container_and_the_flags_round_trip_and_the_password_stays_out_of_debug_output() {
        let with = Evaluated {
            container: Some(Container {
                image: "ghcr.io/acme/builder:1".to_owned(),
                credentials: Some(Credentials {
                    username: Some("ci".to_owned()),
                    password: Some("hunter2".to_owned()),
                }),
            }),
            allow_privileged: true,
            workspace_clean: true,
            untrusted: true,
            needs_podman: true,
            ..evaluated()
        };
        let written = serde_json::to_value(&with).expect("JSON");
        assert_eq!(written["container"]["credentials"]["password"], "hunter2");
        assert_eq!(
            [
                &written["allow_privileged"],
                &written["workspace_clean"],
                &written["untrusted"],
                &written["needs_podman"]
            ],
            [&json!(true); 4]
        );
        let read: Evaluated = serde_json::from_value(written.clone()).expect("a document");
        assert_eq!(read, with);
        assert!(!format!("{read:?}").contains("hunter2"));
        let mut unknown = written;
        unknown["container"]["entrypoint"] = json!("sh");
        assert!(serde_json::from_value::<Evaluated>(unknown).is_err());
    }
}
