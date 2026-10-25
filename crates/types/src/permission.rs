use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum Permission {
    #[serde(rename = "project.view")]
    ProjectView,
    #[serde(rename = "run.dispatch")]
    RunDispatch,
    #[serde(rename = "run.cancel")]
    RunCancel,
    #[serde(rename = "run.rerun")]
    RunRerun,
    #[serde(rename = "run.control")]
    RunControl,
    #[serde(rename = "run.override")]
    RunOverride,
    #[serde(rename = "job.debug")]
    JobDebug,
    #[serde(rename = "workflow.edit")]
    WorkflowEdit,
    #[serde(rename = "workflow.commit")]
    WorkflowCommit,
    #[serde(rename = "workflow.manage")]
    WorkflowManage,
    #[serde(rename = "secret.manage")]
    SecretManage,
    #[serde(rename = "variable.manage")]
    VariableManage,
    #[serde(rename = "environment.manage")]
    EnvironmentManage,
    #[serde(rename = "environment.bypass")]
    EnvironmentBypass,
    #[serde(rename = "agent.manage")]
    AgentManage,
    #[serde(rename = "plugin.manage")]
    PluginManage,
    #[serde(rename = "project.manage")]
    ProjectManage,
    #[serde(rename = "organisation.manage")]
    OrganisationManage,
    #[serde(rename = "audit.view")]
    AuditView,
}

impl Permission {
    pub const ALL: [Permission; 19] = [
        Permission::ProjectView,
        Permission::RunDispatch,
        Permission::RunCancel,
        Permission::RunRerun,
        Permission::RunControl,
        Permission::RunOverride,
        Permission::JobDebug,
        Permission::WorkflowEdit,
        Permission::WorkflowCommit,
        Permission::WorkflowManage,
        Permission::SecretManage,
        Permission::VariableManage,
        Permission::EnvironmentManage,
        Permission::EnvironmentBypass,
        Permission::AgentManage,
        Permission::PluginManage,
        Permission::ProjectManage,
        Permission::OrganisationManage,
        Permission::AuditView,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Permission::ProjectView => "project.view",
            Permission::RunDispatch => "run.dispatch",
            Permission::RunCancel => "run.cancel",
            Permission::RunRerun => "run.rerun",
            Permission::RunControl => "run.control",
            Permission::RunOverride => "run.override",
            Permission::JobDebug => "job.debug",
            Permission::WorkflowEdit => "workflow.edit",
            Permission::WorkflowCommit => "workflow.commit",
            Permission::WorkflowManage => "workflow.manage",
            Permission::SecretManage => "secret.manage",
            Permission::VariableManage => "variable.manage",
            Permission::EnvironmentManage => "environment.manage",
            Permission::EnvironmentBypass => "environment.bypass",
            Permission::AgentManage => "agent.manage",
            Permission::PluginManage => "plugin.manage",
            Permission::ProjectManage => "project.manage",
            Permission::OrganisationManage => "organisation.manage",
            Permission::AuditView => "audit.view",
        }
    }

    pub fn from_key(key: &str) -> Option<Permission> {
        Permission::ALL
            .into_iter()
            .find(|permission| permission.key() == key)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}
