use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

const FALLBACK: &[u8] = br#"{"code":"internal","msg":"the artifact service failed"}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Code {
    Malformed,
    InvalidArgument,
    BadRoute,
    Unauthenticated,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    FailedPrecondition,
    Internal,
}

impl Code {
    fn name(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::InvalidArgument => "invalid_argument",
            Self::BadRoute => "bad_route",
            Self::Unauthenticated => "unauthenticated",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::FailedPrecondition => "failed_precondition",
            Self::Internal => "internal",
        }
    }

    fn status(self) -> StatusCode {
        match self {
            Self::Malformed | Self::InvalidArgument => StatusCode::BAD_REQUEST,
            Self::BadRoute | Self::NotFound => StatusCode::NOT_FOUND,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied => StatusCode::FORBIDDEN,
            Self::AlreadyExists => StatusCode::CONFLICT,
            Self::FailedPrecondition => StatusCode::PRECONDITION_FAILED,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TwirpError {
    pub(crate) code: Code,
    pub(crate) message: String,
}

impl TwirpError {
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn internal(context: &str, error: &dyn std::fmt::Display) -> Self {
        tracing::error!(%error, "{context}");
        Self::new(Code::Internal, "the artifact service failed")
    }
}

#[derive(Serialize)]
struct Body<'a> {
    code: &'a str,
    msg: &'a str,
}

impl IntoResponse for TwirpError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&Body {
            code: self.code.name(),
            msg: &self.message,
        })
        .unwrap_or_else(|_| FALLBACK.to_vec());
        json(self.code.status(), body)
    }
}

pub(crate) fn json(status: StatusCode, body: Vec<u8>) -> Response {
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}
