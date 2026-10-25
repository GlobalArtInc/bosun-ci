use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ir::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PathSegment {
    Key(String),
    Index(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub file: String,
    pub span: Span,
    pub path: Vec<PathSegment>,
}
