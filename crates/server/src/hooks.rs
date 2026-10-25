mod installations;
mod worker;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bosun_db::deliveries::NewDelivery;
use bosun_types::error::ErrorCode;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::AppState;
use crate::envelope::ApiError;
use crate::request::database;

pub(crate) use worker::work;

const LARGEST_PAYLOAD: usize = 25 * 1024 * 1024;

const SIGNATURE: &str = "x-hub-signature-256";

const DELIVERY: &str = "x-github-delivery";

const EVENT: &str = "x-github-event";

const LONGEST_NAME: usize = 64;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(
        "/hooks/github",
        post(receive).layer(DefaultBodyLimit::max(LARGEST_PAYLOAD)),
    )
}

async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let Some(forge) = state.forge.as_deref() else {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            "this server has no GitHub App, so it takes no deliveries at /hooks/github",
        ));
    };
    let Some(signature) = headers.get(SIGNATURE) else {
        warn!("refused a webhook delivery without an X-Hub-Signature-256");
        return Err(ApiError::new(
            ErrorCode::Unauthenticated,
            "the delivery carries no X-Hub-Signature-256",
        ));
    };
    if !forge.signed(signature.as_bytes(), &body) {
        warn!("refused a webhook delivery whose X-Hub-Signature-256 does not match its body");
        return Err(ApiError::new(
            ErrorCode::Unauthenticated,
            "the X-Hub-Signature-256 of the delivery does not match its body under the webhook \
             secret",
        ));
    }
    let delivery = header(&headers, DELIVERY, "X-GitHub-Delivery", |byte| {
        byte.is_ascii_alphanumeric() || byte == b'-'
    })?;
    let event = header(&headers, EVENT, "X-GitHub-Event", |byte| {
        byte.is_ascii_lowercase() || byte == b'_'
    })?;
    let payload: Value = serde_json::from_slice(&body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidInput,
                "the payload of the delivery is not a JSON object",
            )
        })?;
    let stored = bosun_db::deliveries::store(
        &state.db,
        NewDelivery {
            delivery,
            event,
            payload: &payload,
        },
    )
    .await
    .map_err(database)?;
    info!(delivery, event, stored, "received a webhook delivery");
    let status = if stored {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(json!({ "delivery": delivery, "stored": stored })),
    )
        .into_response())
}

fn header<'a>(
    headers: &'a HeaderMap,
    name: &str,
    shown: &str,
    allowed: impl Fn(u8) -> bool,
) -> Result<&'a str, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| (1..=LONGEST_NAME).contains(&value.len()) && value.bytes().all(&allowed))
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidInput,
                format!(
                    "the delivery carries no {shown}, or one that is not 1 to {LONGEST_NAME} \
                     characters of its form"
                ),
            )
        })
}
