use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::time::timeout;
use tracing::warn;

use crate::AppState;

const DATABASE_ANSWER: Duration = Duration::from_secs(1);

pub(crate) async fn alive() -> Response {
    Json(json!({ "status": "alive" })).into_response()
}

pub(crate) async fn ready(State(state): State<AppState>) -> Response {
    match timeout(DATABASE_ANSWER, state.db.ping()).await {
        Ok(Ok(())) => Json(json!({ "status": "ready" })).into_response(),
        Ok(Err(error)) => {
            warn!(%error, "the database does not answer");
            unready("the database does not answer")
        }
        Err(_) => {
            warn!(
                timeout_ms = DATABASE_ANSWER.as_millis(),
                "the database does not answer in time"
            );
            unready("the database does not answer")
        }
    }
}

fn unready(reason: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "status": "unready", "reason": reason })),
    )
        .into_response()
}
