use serde::{Deserialize, Serialize};

use crate::diagnostic::Diagnostic;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthenticated,
    Forbidden,
    NotFound,
    Conflict,
    InvalidState,
    PreconditionFailed,
    InvalidInput,
    RateLimited,
    Internal,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 9] = [
        ErrorCode::Unauthenticated,
        ErrorCode::Forbidden,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::InvalidState,
        ErrorCode::PreconditionFailed,
        ErrorCode::InvalidInput,
        ErrorCode::RateLimited,
        ErrorCode::Internal,
    ];

    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::Unauthenticated => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::Conflict | ErrorCode::InvalidState => 409,
            ErrorCode::PreconditionFailed => 412,
            ErrorCode::InvalidInput => 422,
            ErrorCode::RateLimited => 429,
            ErrorCode::Internal => 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub fields: Vec<FieldError>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
        ErrorBody {
            code,
            message: message.into(),
            fields: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}
