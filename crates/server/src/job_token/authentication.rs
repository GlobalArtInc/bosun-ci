#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the routes of the runtime services mount this layer from step 6 of M2"
    )
)]

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bosun_services::{JobToken, KeySet, TokenError};
use bosun_types::status::ExecutionStatus;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use tracing::warn;

use crate::AppState;
use crate::auth::{self, Refusal};
use crate::signing_key::Failure;

pub(crate) async fn authenticated(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    match authenticate(&state.db, state.clock.now(), request.headers()).await {
        Ok(token) => {
            request.extensions_mut().insert(token);
            next.run(request).await
        }
        Err(refusal) => refusal.into_response(),
    }
}

pub(crate) async fn authenticate(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    headers: &HeaderMap,
) -> Result<JobToken, Refusal> {
    let refused = Refusal::Unauthenticated;
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let header = match (values.next(), values.next()) {
        (Some(header), None) => header,
        (None, _) => return Err(refused("the request carries no job token")),
        (Some(_), Some(_)) => {
            return Err(refused(
                "the request carries more than one authorization header",
            ));
        }
    };
    let token = header
        .to_str()
        .ok()
        .and_then(auth::bearer)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| refused("the authorization header does not hold a bearer token"))?;
    if !well_formed(token) {
        return Err(refused("the job token is malformed"));
    }
    let keys = verifying_keys(db).await.map_err(unchecked)?;
    let verified = keys.verify(token, now).map_err(|error| {
        refused(match error {
            TokenError::Malformed(_) => "the job token is malformed",
            TokenError::Expired => "the job token has expired",
            _ => "the job token is not valid",
        })
    })?;
    let identity = verified.identity();
    let status = bosun_db::executions::status_in_attempt(db, identity.attempt, identity.execution)
        .await
        .map_err(|error| unchecked(error.into()))?;
    match status {
        None => Err(refused("the execution of the job token does not exist")),
        Some(ExecutionStatus::Completed) => {
            Err(refused("the execution of the job token has completed"))
        }
        Some(_) => Ok(verified),
    }
}

const MAX_TOKEN_BYTES: usize = 4096;

fn well_formed(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    token.len() <= MAX_TOKEN_BYTES
        && parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(is_base64url))
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

pub(crate) async fn verifying_keys(db: &DatabaseConnection) -> Result<KeySet, Failure> {
    let mut keys = KeySet::new();
    for public_key in bosun_db::signing_keys::public_keys(db).await? {
        keys.insert(public_key);
    }
    Ok(keys)
}

fn unchecked(failure: Failure) -> Refusal {
    warn!(error = %failure, "a job token could not be checked");
    Refusal::Unchecked
}
