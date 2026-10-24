use axum::Json;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bosun_types::error::{ErrorBody, ErrorCode, ErrorEnvelope};

pub(crate) struct ApiError(ErrorBody);

impl ApiError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> ApiError {
        ApiError(ErrorBody::new(code, message))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(ErrorEnvelope { error: self.0 })).into_response()
    }
}

pub(crate) async fn no_route(method: Method, uri: Uri) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("there is no {method} {}", uri.path()),
    )
}
