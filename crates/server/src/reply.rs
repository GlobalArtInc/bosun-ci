use axum::Json;
use axum::http::header::ETAG;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::openapi::{self, Output, Spec};
use crate::request::failed;

pub(crate) enum Created<T> {
    New(T),
    Kept(Value),
}

impl<T: Serialize> IntoResponse for Created<T> {
    fn into_response(self) -> Response {
        match self {
            Created::New(value) => (StatusCode::CREATED, Json(value)).into_response(),
            Created::Kept(value) => (StatusCode::CREATED, Json(value)).into_response(),
        }
    }
}

impl<T: JsonSchema> Output for Created<T> {
    fn output(spec: &mut Spec<'_>) {
        spec.json::<T>(StatusCode::CREATED);
    }
}

pub(crate) enum Saved<T> {
    New(T),
    Again(T),
}

impl<T: Serialize> IntoResponse for Saved<T> {
    fn into_response(self) -> Response {
        match self {
            Saved::New(value) => (StatusCode::CREATED, Json(value)).into_response(),
            Saved::Again(value) => (StatusCode::OK, Json(value)).into_response(),
        }
    }
}

impl<T: JsonSchema> Output for Saved<T> {
    fn output(spec: &mut Spec<'_>) {
        spec.json::<T>(StatusCode::CREATED);
        spec.json::<T>(StatusCode::OK);
    }
}

pub(crate) fn etag(version: u64) -> String {
    format!("\"{version}\"")
}

pub(crate) struct Tagged<T> {
    pub(crate) version: u64,
    pub(crate) value: T,
}

impl<T: Serialize> IntoResponse for Tagged<T> {
    fn into_response(self) -> Response {
        match HeaderValue::try_from(etag(self.version)) {
            Ok(tag) => (StatusCode::OK, [(ETAG, tag)], Json(self.value)).into_response(),
            Err(error) => failed(error).into_response(),
        }
    }
}

impl<T: JsonSchema> Output for Tagged<T> {
    fn output(spec: &mut Spec<'_>) {
        spec.json::<T>(StatusCode::OK);
        spec.shared_header(StatusCode::OK, "ETag", openapi::ETAG);
    }
}
