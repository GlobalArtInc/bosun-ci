use std::collections::BTreeMap;
use std::fmt::Display;
use std::marker::PhantomData;

use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::HeaderValue;
use axum::http::header::IF_MATCH;
use axum::http::request::Parts;
use bosun_types::api::Page;
use bosun_types::error::{ErrorBody, ErrorCode, FieldError};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tracing::error;
use uuid::Uuid;

use crate::envelope::{self, ApiError};
use crate::idempotency::Idempotency;
use crate::openapi::{self, Input, Spec};

const DEFAULT_LIMIT: usize = 50;

const MAX_LIMIT: usize = 500;

const LIMIT: &str = "limit";

const CURSOR: &str = "cursor";

pub(crate) struct Body<T>(pub(crate) T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Body<T>, ApiError> {
        let bytes = bytes(request, state).await?;
        decode(&bytes).map(Body)
    }
}

impl<T: JsonSchema> Input for Body<T> {
    fn input(spec: &mut Spec<'_>) {
        spec.body::<T>();
    }
}

pub(crate) struct Idempotent<T> {
    pub(crate) value: T,
    pub(crate) idempotency: Option<Idempotency>,
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Idempotent<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Idempotent<T>, ApiError> {
        let (parts, body) = request.into_parts();
        let pending = Idempotency::prepare(&parts)?;
        let bytes = bytes(Request::from_parts(parts, body), state).await?;
        Ok(Idempotent {
            value: decode(&bytes)?,
            idempotency: pending.map(|pending| pending.finish(&bytes)),
        })
    }
}

impl<T: JsonSchema> Input for Idempotent<T> {
    fn input(spec: &mut Spec<'_>) {
        spec.reference(openapi::IDEMPOTENCY_KEY);
        spec.body::<T>();
    }
}

pub(crate) struct Changes<T> {
    fields: Map<String, Value>,
    shape: PhantomData<fn() -> T>,
}

impl<S: Send + Sync, T> FromRequest<S> for Changes<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Changes<T>, ApiError> {
        let bytes = bytes(request, state).await?;
        Ok(Changes {
            fields: decode(&bytes)?,
            shape: PhantomData,
        })
    }
}

impl<T: JsonSchema> Input for Changes<T> {
    fn input(spec: &mut Spec<'_>) {
        spec.changes::<T>();
    }
}

impl<T: Serialize + DeserializeOwned> Changes<T> {
    pub(crate) fn apply(self, current: &T, noun: &str, refusal: &str) -> Result<T, ApiError> {
        let document = match serde_json::to_value(current).map_err(failed)? {
            Value::Object(document) => document,
            _ => return Err(failed("a document to change is not a JSON object")),
        };
        if self.fields.is_empty() {
            return Err(invalid(format!(
                "the request changes nothing; it names each {noun} to change with its new value"
            )));
        }
        let mut changed = document.clone();
        let mut faults = Faults::default();
        for (name, value) in self.fields {
            if !document.contains_key(&name) {
                let known: Vec<String> = document.keys().map(|key| format!("`{key}`")).collect();
                faults.add(
                    name.as_str(),
                    format!(
                        "there is no {noun} `{name}`; the {noun}s are {}",
                        known.join(", ")
                    ),
                );
                continue;
            }
            let mut alone = document.clone();
            alone.insert(name.clone(), value.clone());
            match serde_json::from_value::<T>(Value::Object(alone)) {
                Ok(_) => {
                    changed.insert(name, value);
                }
                Err(error) => faults.add(
                    name.as_str(),
                    format!("`{name}` cannot be {value}: {error}"),
                ),
            }
        }
        faults.finish(refusal)?;
        serde_json::from_value(Value::Object(changed)).map_err(failed)
    }
}

pub(crate) struct IfMatch(Option<HeaderValue>);

impl<S: Send + Sync> FromRequestParts<S> for IfMatch {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<IfMatch, ApiError> {
        Ok(IfMatch(parts.headers.get(IF_MATCH).cloned()))
    }
}

impl Input for IfMatch {
    fn input(spec: &mut Spec<'_>) {
        spec.reference(openapi::IF_MATCH);
    }
}

impl IfMatch {
    pub(crate) fn check(&self, current: &str, what: &str) -> Result<(), ApiError> {
        let refused = |message: String| ApiError::new(ErrorCode::PreconditionFailed, message);
        let Some(value) = &self.0 else {
            return Err(refused(format!(
                "`If-Match` is missing; a change of {what} sends the `ETag` of the version it was \
                 made from"
            )));
        };
        let text = String::from_utf8_lossy(value.as_bytes());
        let text = text.trim();
        if text.split(',').map(str::trim).any(|tag| tag == current) {
            return Ok(());
        }
        if text == "*" {
            return Err(refused(format!(
                "`If-Match` is `*`, which names no version; a change of {what} sends the `ETag` of \
                 the version it was made from"
            )));
        }
        Err(refused(format!(
            "`If-Match` is `{text}`, and the current version of {what} is `{current}`: read the \
             current version and repeat the change"
        )))
    }
}

async fn bytes<S: Send + Sync>(request: Request, state: &S) -> Result<Bytes, ApiError> {
    Bytes::from_request(request, state)
        .await
        .map_err(|rejection| {
            invalid(format!(
                "the request body could not be read: {}",
                rejection.body_text()
            ))
        })
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("the request body does not fit: {error}")))
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

impl Input for Segment {
    fn input(_: &mut Spec<'_>) {}
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

pub(crate) trait Filters {
    const NAMES: &'static [&'static str];
}

pub(crate) struct Unfiltered;

impl Filters for Unfiltered {
    const NAMES: &'static [&'static str] = &[];
}

pub(crate) struct Listing<F = Unfiltered> {
    limit: usize,
    cursor: Option<Uuid>,
    filters: BTreeMap<String, String>,
    filtered: PhantomData<F>,
}

impl<S: Send + Sync, F: Filters> FromRequestParts<S> for Listing<F> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Listing<F>, ApiError> {
        let query = Query::<Vec<(String, String)>>::from_request_parts(parts, state).await;
        Listing::parse(query)
    }
}

impl<F: Filters> Input for Listing<F> {
    fn input(spec: &mut Spec<'_>) {
        spec.reference(openapi::LIMIT);
        spec.reference(openapi::CURSOR);
        for name in F::NAMES {
            spec.parameter(json!({
                "name": name,
                "in": "query",
                "description": format!("only the items whose `{name}` is this value"),
                "schema": { "type": "string" }
            }));
        }
    }
}

impl<F: Filters> Listing<F> {
    fn parse(
        query: Result<Query<Vec<(String, String)>>, QueryRejection>,
    ) -> Result<Listing<F>, ApiError> {
        let filters = F::NAMES;
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
            filtered: PhantomData,
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
        | bosun_db::Error::WorkflowTaken(_)
        | bosun_db::Error::PoolTaken(_)
        | bosun_db::Error::RepositoryBound(_) => {
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
