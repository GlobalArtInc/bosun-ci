use std::time::Instant;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use tracing::{Instrument, info, info_span};
use uuid::Uuid;

const REQUEST_ID: &str = "x-request-id";

const REQUEST_ID_LENGTH: usize = 128;

pub(crate) async fn request(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|value| acceptable(value))
        .map_or_else(|| Uuid::now_v7().to_string(), str::to_owned);
    let span = info_span!(
        "request",
        request_id = %id,
        method = %request.method(),
        path = %request.uri().path(),
    );
    let started = Instant::now();
    let mut response = next.run(request).instrument(span.clone()).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    span.in_scope(|| {
        info!(
            status = response.status().as_u16(),
            duration_ms = started.elapsed().as_secs_f64() * 1e3,
            "request"
        );
    });
    response
}

fn acceptable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= REQUEST_ID_LENGTH
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
