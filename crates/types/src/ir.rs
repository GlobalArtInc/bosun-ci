use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

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
    Expression(Expr),
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
                Segment::Expression(_) => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    Expression(Expr),
    Template(Template),
}

#[derive(Debug, Clone, Copy)]
pub struct Number(pub f64);

impl PartialEq for Number {
    fn eq(&self, other: &Number) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Number {}

impl Serialize for Number {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = self.0;
        if value.is_nan() {
            serializer.serialize_str("NaN")
        } else if value.is_infinite() {
            serializer.serialize_str(if value > 0.0 { "Infinity" } else { "-Infinity" })
        } else {
            serializer.serialize_f64(value)
        }
    }
}

struct NumberVisitor;

impl Visitor<'_> for NumberVisitor {
    type Value = Number;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a finite number, `NaN`, `Infinity` or `-Infinity`")
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Number, E> {
        Ok(Number(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Number, E> {
        Ok(Number(value as f64))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Number, E> {
        Ok(Number(value as f64))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Number, E> {
        match value {
            "NaN" => Ok(Number(f64::NAN)),
            "Infinity" => Ok(Number(f64::INFINITY)),
            "-Infinity" => Ok(Number(f64::NEG_INFINITY)),
            _ => Err(E::invalid_value(de::Unexpected::Str(value), &self)),
        }
    }
}

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Number, D::Error> {
        deserializer.deserialize_any(NumberVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    #[serde(rename = "==")]
    Equal,
    #[serde(rename = "!=")]
    NotEqual,
    #[serde(rename = "<")]
    Less,
    #[serde(rename = "<=")]
    LessOrEqual,
    #[serde(rename = ">")]
    Greater,
    #[serde(rename = ">=")]
    GreaterOrEqual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Expr {
    Null {
        span: Span,
    },
    Boolean {
        value: bool,
        span: Span,
    },
    Number {
        value: Number,
        span: Span,
    },
    String {
        value: String,
        span: Span,
    },
    Context {
        name: String,
        span: Span,
    },
    Property {
        target: Box<Expr>,
        name: String,
        span: Span,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Filter {
        target: Box<Expr>,
        span: Span,
    },
    Not {
        operand: Box<Expr>,
        span: Span,
    },
    Compare {
        operator: Comparison,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    And {
        operands: Vec<Expr>,
        span: Span,
    },
    Or {
        operands: Vec<Expr>,
        span: Span,
    },
    Call {
        function: String,
        arguments: Vec<Expr>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Null { span }
            | Expr::Boolean { span, .. }
            | Expr::Number { span, .. }
            | Expr::String { span, .. }
            | Expr::Context { span, .. }
            | Expr::Property { span, .. }
            | Expr::Index { span, .. }
            | Expr::Filter { span, .. }
            | Expr::Not { span, .. }
            | Expr::Compare { span, .. }
            | Expr::And { span, .. }
            | Expr::Or { span, .. }
            | Expr::Call { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Evaluable<T> {
    Literal(T),
    Template(Template),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub name: Option<String>,
    pub triggers: Vec<Trigger>,
    pub env: BTreeMap<String, Template>,
    pub jobs: Vec<JobTemplate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Push(Push),
    PullRequest(PullRequest),
    PullRequestTarget(PullRequest),
    MergeGroup(MergeGroup),
    Schedule(Vec<Schedule>),
    WorkflowDispatch(WorkflowDispatch),
    RepositoryDispatch(Activity),
    Generic(GenericEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Filter {
    Include(Vec<String>),
    Exclude(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Push {
    pub branches: Option<Filter>,
    pub tags: Option<Filter>,
    pub paths: Option<Filter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub types: Option<Vec<String>>,
    pub branches: Option<Filter>,
    pub paths: Option<Filter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeGroup {
    pub types: Option<Vec<String>>,
    pub branches: Option<Filter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub types: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenericEvent {
    pub name: String,
    pub types: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub cron: Cron,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cron {
    pub expression: String,
    pub minutes: Vec<u8>,
    pub hours: Vec<u8>,
    pub days_of_month: Vec<u8>,
    pub months: Vec<u8>,
    pub days_of_week: Vec<u8>,
    pub days: CronDays,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronDays {
    Both,
    Either,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDispatch {
    pub inputs: Vec<Input>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: InputKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum InputKind {
    String {
        default: Option<String>,
    },
    Boolean {
        default: Option<bool>,
    },
    Choice {
        options: Vec<String>,
        default: Option<String>,
    },
    Number {
        default: Option<f64>,
    },
    Environment {
        default: Option<String>,
    },
}

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
