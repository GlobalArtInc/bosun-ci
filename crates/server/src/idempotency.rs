use axum::Json;
use axum::http::request::Parts;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bosun_db::idempotency::Stored;
use bosun_types::error::ErrorCode;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::auth::Principal;
use crate::envelope::ApiError;
use crate::request::{database, failed, invalid};

const HEADER: &str = "idempotency-key";

const KEY_LENGTH: usize = 255;

pub(crate) struct Pending {
    principal: String,
    key: String,
    method: Method,
    uri: Uri,
}

pub(crate) struct Idempotency {
    principal: String,
    key: String,
    sha256: Vec<u8>,
}

impl Idempotency {
    pub(crate) fn prepare(parts: &Parts) -> Result<Option<Pending>, ApiError> {
        let Some(value) = parts.headers.get(HEADER) else {
            return Ok(None);
        };
        let key = value
            .to_str()
            .ok()
            .filter(|key| {
                !key.is_empty()
                    && key.len() <= KEY_LENGTH
                    && key.bytes().all(|byte| byte.is_ascii_graphic())
            })
            .ok_or_else(|| {
                invalid(format!(
                    "the Idempotency-Key header must be 1 to {KEY_LENGTH} visible ASCII characters"
                ))
            })?;
        let principal = parts
            .extensions
            .get::<Principal>()
            .ok_or_else(|| failed("an Idempotency-Key reached a route without a principal"))?;
        Ok(Some(Pending {
            principal: format!("user:{}", principal.user.id),
            key: key.to_owned(),
            method: parts.method.clone(),
            uri: parts.uri.clone(),
        }))
    }
}

impl Pending {
    pub(crate) fn finish(self, body: &[u8]) -> Idempotency {
        let target = self
            .uri
            .path_and_query()
            .map_or_else(|| self.uri.path(), |target| target.as_str());
        let mut hash = Sha256::new();
        hash.update(self.method.as_str().as_bytes());
        hash.update(b" ");
        hash.update(target.as_bytes());
        hash.update(b"\n");
        hash.update(body);
        Idempotency {
            principal: self.principal,
            key: self.key,
            sha256: hash.finalize().to_vec(),
        }
    }
}

pub(crate) async fn replay<C: ConnectionTrait>(
    db: &C,
    idempotency: Option<&Idempotency>,
) -> Result<Option<Response>, ApiError> {
    let Some(idempotency) = idempotency else {
        return Ok(None);
    };
    let Some(stored) = bosun_db::idempotency::find(db, &idempotency.principal, &idempotency.key)
        .await
        .map_err(database)?
    else {
        return Ok(None);
    };
    if stored.request_sha256 != idempotency.sha256 {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!(
                "the Idempotency-Key `{}` came with a different request within the last 24 hours",
                idempotency.key
            ),
        ));
    }
    let status = StatusCode::from_u16(stored.status)
        .map_err(|error| failed(format!("a stored response has no valid status: {error}")))?;
    Ok(Some((status, Json(stored.response)).into_response()))
}

pub(crate) async fn commit<T: Serialize>(
    db: &DatabaseConnection,
    transaction: DatabaseTransaction,
    idempotency: Option<&Idempotency>,
    status: StatusCode,
    body: &T,
) -> Result<Response, ApiError> {
    let response = serde_json::to_value(body)
        .map_err(|error| failed(format!("a response could not be encoded: {error}")))?;
    if let Some(idempotency) = idempotency {
        let recorded = bosun_db::idempotency::record(
            &transaction,
            &idempotency.principal,
            &idempotency.key,
            Stored {
                request_sha256: idempotency.sha256.clone(),
                status: status.as_u16(),
                response: response.clone(),
            },
        )
        .await
        .map_err(database)?;
        if !recorded {
            transaction.rollback().await.map_err(database)?;
            return replay(db, Some(idempotency))
                .await?
                .ok_or_else(|| failed("a concurrent request kept no response for its key"));
        }
    }
    transaction.commit().await.map_err(database)?;
    Ok((status, Json(response)).into_response())
}
