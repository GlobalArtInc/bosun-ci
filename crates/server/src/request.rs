use std::collections::BTreeMap;
use std::fmt::Display;

use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::request::Parts;
use bosun_types::api::Page;
use bosun_types::error::{ErrorBody, ErrorCode, FieldError};
use serde::de::DeserializeOwned;
use tracing::error;
use uuid::Uuid;

use crate::envelope::{self, ApiError};
use crate::idempotency::Idempotency;

const DEFAULT_LIMIT: usize = 50;

const MAX_LIMIT: usize = 500;

const LIMIT: &str = "limit";

const CURSOR: &str = "cursor";

pub(crate) struct Body<T> {
    pub(crate) value: T,
    pub(crate) idempotency: Option<Idempotency>,
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Body<T>, ApiError> {
        let (parts, body) = request.into_parts();
        let pending = Idempotency::prepare(&parts)?;
        let bytes = Bytes::from_request(Request::from_parts(parts, body), state)
            .await
            .map_err(|rejection| {
                invalid(format!(
                    "the request body could not be read: {}",
                    rejection.body_text()
                ))
            })?;
        let value = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("the request body does not fit: {error}")))?;
        Ok(Body {
            value,
            idempotency: pending.map(|pending| pending.finish(&bytes)),
        })
    }
}

pub(crate) struct Segment(pub(crate) String);

impl<S: Send + Sync> FromRequestParts<S> for Segment {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Segment, ApiError> {
        match Path::<String>::from_request_parts(parts, state).await {
            Ok(Path(segment)) => Ok(Segment(segment)),
            Err(_) => Err(envelope::absent(&parts.method, &parts.uri)),
        }
    }
}

#[derive(Default)]
pub(crate) struct Faults(Vec<FieldError>);

impl Faults {
    pub(crate) fn add(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.0.push(FieldError {
            field: field.into(),
            message: message.into(),
        });
    }

    pub(crate) fn finish(self, message: &str) -> Result<(), ApiError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(self.refuse(message))
        }
    }

    pub(crate) fn refuse(self, message: &str) -> ApiError {
        let mut body = ErrorBody::new(ErrorCode::InvalidInput, message);
        body.fields = self.0;
        ApiError(body)
    }
}

pub(crate) struct Listing {
    limit: usize,
    cursor: Option<Uuid>,
    filters: BTreeMap<String, String>,
}

impl Listing {
    pub(crate) fn parse(
        query: Result<Query<Vec<(String, String)>>, QueryRejection>,
        filters: &[&str],
    ) -> Result<Listing, ApiError> {
        let Query(pairs) = query.map_err(|rejection| {
            invalid(format!(
                "the query string could not be read: {}",
                rejection.body_text()
            ))
        })?;
        let mut listing = Listing {
            limit: DEFAULT_LIMIT,
            cursor: None,
            filters: BTreeMap::new(),
        };
        for (name, value) in pairs {
            match name.as_str() {
                LIMIT => {
                    listing.limit = value
                        .parse()
                        .ok()
                        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                        .ok_or_else(|| {
                            invalid(format!(
                                "`limit` is `{value}`, and a page holds 1 to {MAX_LIMIT} items"
                            ))
                        })?;
                }
                CURSOR => {
                    listing.cursor = Some(Uuid::try_parse(&value).map_err(|_| {
                        invalid(format!(
                            "`cursor` is `{value}`, which is not the `next` of a page of this list"
                        ))
                    })?);
                }
                _ if filters.contains(&name.as_str()) => {
                    listing.filters.insert(name, value);
                }
                _ => {
                    let mut known = vec![LIMIT, CURSOR];
                    known.extend_from_slice(filters);
                    return Err(invalid(format!(
                        "this list has no query parameter `{name}`; it takes {}",
                        known
                            .iter()
                            .map(|known| format!("`{known}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        }
        Ok(listing)
    }

    pub(crate) fn filter(&self, name: &str) -> Option<&str> {
        self.filters.get(name).map(String::as_str)
    }

    pub(crate) fn cursor(&self) -> Option<Uuid> {
        self.cursor
    }

    pub(crate) fn fetch(&self) -> u64 {
        u64::try_from(self.limit + 1).unwrap_or(u64::MAX)
    }

    pub(crate) fn page<T>(&self, mut items: Vec<T>, id: impl Fn(&T) -> Uuid) -> Page<T> {
        let more = items.len() > self.limit;
        items.truncate(self.limit);
        let next = items
            .last()
            .filter(|_| more)
            .map(|last| id(last).to_string());
        Page { items, next }
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidInput, message)
}

pub(crate) fn database(failure: impl Into<bosun_db::Error>) -> ApiError {
    let failure = failure.into();
    match failure {
        bosun_db::Error::OrganisationTaken(_)
        | bosun_db::Error::ProjectTaken(_)
        | bosun_db::Error::WorkflowTaken(_) => {
            ApiError::new(ErrorCode::Conflict, failure.to_string())
        }
        _ => failed(failure),
    }
}

pub(crate) fn failed(failure: impl Display) -> ApiError {
    error!(error = %failure, "a request failed");
    ApiError::new(
        ErrorCode::Internal,
        "the request failed inside the server, whose log says why",
    )
}
