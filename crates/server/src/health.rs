use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::time::timeout;
use tracing::warn;

use crate::{AppState, store};

const ANSWER: Duration = Duration::from_secs(1);

const DATABASE: &str = "the database does not answer";

const STORE: &str = "the object store does not answer";

pub(crate) async fn alive() -> Response {
    Json(json!({ "status": "alive" })).into_response()
}

pub(crate) async fn ready(State(state): State<AppState>) -> Response {
    match timeout(ANSWER, state.db.ping()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(%error, "the database does not answer");
            return unready(DATABASE);
        }
        Err(_) => {
            warn!(
                timeout_ms = ANSWER.as_millis(),
                "the database does not answer in time"
            );
            return unready(DATABASE);
        }
    }
    match timeout(ANSWER, store::answers(&state.store)).await {
        Ok(Ok(())) => Json(json!({ "status": "ready" })).into_response(),
        Ok(Err(error)) => {
            warn!(%error, "the object store does not answer");
            unready(STORE)
        }
        Err(_) => {
            warn!(
                timeout_ms = ANSWER.as_millis(),
                "the object store does not answer in time"
            );
            unready(STORE)
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
