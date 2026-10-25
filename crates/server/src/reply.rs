use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::openapi::{Output, Spec};

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
