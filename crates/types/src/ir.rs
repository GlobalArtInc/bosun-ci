use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::key::{JobKey, StepKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Segment {
    Text(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Template(Vec<Segment>);

impl Template {
    pub fn new(segments: Vec<Segment>) -> Template {
        Template(segments)
    }

    pub fn text(value: impl Into<String>) -> Template {
        Template(vec![Segment::Text(value.into())])
    }

    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    pub fn as_literal(&self) -> Option<String> {
        self.0
            .iter()
            .map(|segment| match segment {
                Segment::Text(text) => Some(text.as_str()),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Literal {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Expr {
    Literal(Literal),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Evaluable<T> {
    Literal(T),
    Template(Template),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub name: Option<String>,
    pub triggers: Vec<Trigger>,
    pub env: BTreeMap<String, Template>,
    pub jobs: Vec<JobTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    WorkflowDispatch(WorkflowDispatch),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDispatch {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobTemplate {
    pub key: JobKey,
    pub name: Option<Template>,
    pub needs: Vec<JobKey>,
    pub runs_on: Evaluable<RunsOn>,
    pub env: BTreeMap<String, Template>,
    pub kind: JobKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunsOn {
    pub group: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum JobKind {
    Steps(Vec<Step>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub key: Option<StepKey>,
    pub name: Option<Template>,
    pub action: Action,
    pub shell: Option<Shell>,
    pub working_directory: Option<Template>,
    pub env: BTreeMap<String, Template>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Run { script: Template },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    Bash,
    Sh,
    Pwsh,
    Python,
    Custom(String),
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Shell::Bash => "bash",
            Shell::Sh => "sh",
            Shell::Pwsh => "pwsh",
            Shell::Python => "python",
            Shell::Custom(template) => template,
        })
    }
}

impl FromStr for Shell {
    type Err = ShellError;

    fn from_str(text: &str) -> Result<Shell, ShellError> {
        match text {
            "bash" => Ok(Shell::Bash),
            "sh" => Ok(Shell::Sh),
            "pwsh" => Ok(Shell::Pwsh),
            "python" => Ok(Shell::Python),
            _ if text.contains("{0}") => Ok(Shell::Custom(text.to_owned())),
            _ => Err(ShellError(text.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellError(String);

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}`, which is neither `bash`, `sh`, `pwsh` nor `python` and has no `{{0}}` for the script",
            self.0
        )
    }
}

impl std::error::Error for ShellError {}
