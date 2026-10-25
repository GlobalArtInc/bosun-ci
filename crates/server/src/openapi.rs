use std::collections::BTreeMap;
use std::future::Future;

use axum::Json;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::handler::Handler;
use axum::http::{Method, StatusCode, Uri};
use axum::middleware;
use axum::routing::{MethodFilter, MethodRouter, on};
use bosun_types::error::ErrorEnvelope;
use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde_json::{Map, Value, json};

use crate::{AppState, auth};

const VERSION: &str = "3.1.0";

const SCHEMAS: &str = "/components/schemas";

const ERROR: &str = "#/components/responses/Error";

pub(crate) const LIMIT: &str = "#/components/parameters/Limit";

pub(crate) const CURSOR: &str = "#/components/parameters/Cursor";

pub(crate) const IDEMPOTENCY_KEY: &str = "#/components/parameters/IdempotencyKey";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Token,
    Public,
}

struct Operation {
    method: Method,
    path: &'static str,
    access: Access,
    document: fn(&mut Spec<'_>),
}

pub(crate) struct Api {
    access: Access,
    authenticated: Router<AppState>,
    public: Router<AppState>,
    operations: Vec<Operation>,
}

impl Api {
    pub(crate) fn new() -> Api {
        Api::with(Access::Token)
    }

    pub(crate) fn public() -> Api {
        Api::with(Access::Public)
    }

    fn with(access: Access) -> Api {
        Api {
            access,
            authenticated: Router::new(),
            public: Router::new(),
            operations: Vec::new(),
        }
    }

    pub(crate) fn get<H, T>(self, path: &'static str, handler: H) -> Api
    where
        H: Handler<T, AppState> + Documented<T>,
        T: 'static,
    {
        self.on(Method::GET, MethodFilter::GET, path, handler)
    }

    pub(crate) fn post<H, T>(self, path: &'static str, handler: H) -> Api
    where
        H: Handler<T, AppState> + Documented<T>,
        T: 'static,
    {
        self.on(Method::POST, MethodFilter::POST, path, handler)
    }

    pub(crate) fn put<H, T>(self, path: &'static str, handler: H) -> Api
    where
        H: Handler<T, AppState> + Documented<T>,
        T: 'static,
    {
        self.on(Method::PUT, MethodFilter::PUT, path, handler)
    }

    fn on<H, T>(self, method: Method, filter: MethodFilter, path: &'static str, handler: H) -> Api
    where
        H: Handler<T, AppState> + Documented<T>,
        T: 'static,
    {
        self.describe(method, path, handler.clone())
            .route(path, on(filter, handler))
    }

    pub(crate) fn describe<H, T>(mut self, method: Method, path: &'static str, _: H) -> Api
    where
        H: Handler<T, AppState> + Documented<T>,
        T: 'static,
    {
        self.operations.push(Operation {
            method,
            path,
            access: self.access,
            document: H::document,
        });
        self
    }

    pub(crate) fn route(mut self, path: &str, router: MethodRouter<AppState>) -> Api {
        match self.access {
            Access::Token => self.authenticated = self.authenticated.route(path, router),
            Access::Public => self.public = self.public.route(path, router),
        }
        self
    }

    pub(crate) fn merge(mut self, other: Api) -> Api {
        self.authenticated = self.authenticated.merge(other.authenticated);
        self.public = self.public.merge(other.public);
        self.operations.extend(other.operations);
        self
    }

    pub(crate) fn into_router(self, state: &AppState) -> Router<AppState> {
        self.authenticated
            .route_layer(middleware::from_fn_with_state(state.clone(), auth::owner))
            .merge(self.public)
    }

    pub(crate) fn document(&self) -> Value {
        let mut generator = SchemaSettings::draft2020_12()
            .with(|settings| {
                settings.definitions_path = SCHEMAS.into();
                settings.meta_schema = None;
            })
            .for_serialize()
            .into_generator();
        let mut paths: BTreeMap<&str, Map<String, Value>> = BTreeMap::new();
        for operation in &self.operations {
            let mut spec = Spec {
                generator: &mut generator,
                parameters: operation_parameters(operation.path),
                body: None,
                responses: Map::new(),
            };
            if operation.access == Access::Token {
                spec.error();
            }
            (operation.document)(&mut spec);
            let mut object = spec.finish();
            if operation.access == Access::Public {
                object.insert("security".to_owned(), json!([]));
            }
            let method = operation.method.as_str().to_lowercase();
            let item = paths.entry(operation.path).or_default();
            assert!(
                item.insert(method, Value::Object(object)).is_none(),
                "two handlers describe {} {}",
                operation.method,
                operation.path
            );
        }
        let envelope = generator.subschema_for::<ErrorEnvelope>();
        json!({
            "openapi": VERSION,
            "info": { "title": "Bosun", "version": "v1" },
            "security": [{ "bearer": [] }],
            "paths": paths,
            "components": {
                "schemas": generator.take_definitions(true),
                "responses": {
                    "Error": {
                        "description": "an error, whose `code` says which",
                        "content": { "application/json": { "schema": envelope } }
                    }
                },
                "parameters": {
                    "Limit": {
                        "name": "limit",
                        "in": "query",
                        "description": "the number of items on the page",
                        "schema": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                    },
                    "Cursor": {
                        "name": "cursor",
                        "in": "query",
                        "description": "the `next` of the previous page",
                        "schema": { "type": "string" }
                    },
                    "IdempotencyKey": {
                        "name": "Idempotency-Key",
                        "in": "header",
                        "description": "a key of the caller's choosing, so that a repeated request with the same key and body returns the first response",
                        "schema": { "type": "string", "minLength": 1, "maxLength": 255, "pattern": "^[!-~]+$" }
                    }
                },
                "securitySchemes": {
                    "bearer": {
                        "type": "http",
                        "scheme": "bearer",
                        "description": "a personal token, `bst_…`"
                    }
                }
            }
        })
    }
}

pub(crate) fn render(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(&sorted(document))
        .unwrap_or_else(|error| panic!("the OpenAPI document cannot be written: {error}"));
    text.push('\n');
    text
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let ordered: BTreeMap<&String, Value> = object
                .iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect();
            Value::Object(
                ordered
                    .into_iter()
                    .map(|(key, value)| (key.clone(), value))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

fn operation_parameters(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| {
            let (description, schema) = path_parameter(name);
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "description": description,
                "schema": schema
            })
        })
        .collect()
}

fn path_parameter(name: &str) -> (&'static str, Value) {
    let id = json!({ "type": "string", "format": "uuid" });
    match name {
        "organisation" => ("the organisation's id or slug", json!({ "type": "string" })),
        "project" => (
            "the project's id, or the slugs of its organisation and of itself joined by `/`",
            json!({ "type": "string" }),
        ),
        "workflow" => ("the workflow's id", id),
        "run" => ("the run's id", id),
        "attempt" => (
            "the number of an attempt of the run, counted from 1",
            json!({ "type": "integer", "minimum": 1 }),
        ),
        "job" => ("the job's id", id),
        "execution" => ("the execution's id", id),
        "step" => ("the id of a step of the execution", id),
        _ => {
            panic!("the path parameter `{name}` has no description in crates/server/src/openapi.rs")
        }
    }
}

pub(crate) struct Spec<'a> {
    generator: &'a mut SchemaGenerator,
    parameters: Vec<Value>,
    body: Option<Value>,
    responses: Map<String, Value>,
}

impl Spec<'_> {
    pub(crate) fn schema<T: JsonSchema>(&mut self) -> Value {
        self.generator.subschema_for::<T>().to_value()
    }

    pub(crate) fn parameter(&mut self, parameter: Value) {
        self.parameters.push(parameter);
    }

    pub(crate) fn reference(&mut self, component: &str) {
        self.parameter(json!({ "$ref": component }));
    }

    pub(crate) fn body<T: JsonSchema>(&mut self) {
        let schema = self.schema::<T>();
        self.body = Some(json!({
            "required": true,
            "content": { "application/json": { "schema": schema } }
        }));
    }

    pub(crate) fn json<T: JsonSchema>(&mut self, status: StatusCode) {
        let schema = self.schema::<T>();
        self.content(status, json!({ "application/json": { "schema": schema } }));
    }

    pub(crate) fn content(&mut self, status: StatusCode, content: Value) {
        self.responses.insert(
            status.as_str().to_owned(),
            json!({
                "description": status.canonical_reason().unwrap_or(status.as_str()),
                "content": content
            }),
        );
    }

    pub(crate) fn error(&mut self) {
        self.responses
            .insert("default".to_owned(), json!({ "$ref": ERROR }));
    }

    fn finish(self) -> Map<String, Value> {
        let mut object = Map::new();
        if !self.parameters.is_empty() {
            object.insert("parameters".to_owned(), Value::Array(self.parameters));
        }
        if let Some(body) = self.body {
            object.insert("requestBody".to_owned(), body);
        }
        object.insert("responses".to_owned(), Value::Object(self.responses));
        object
    }
}

pub(crate) trait Input {
    fn input(spec: &mut Spec<'_>);
}

pub(crate) trait Output {
    fn output(spec: &mut Spec<'_>);
}

pub(crate) trait Documented<T> {
    fn document(spec: &mut Spec<'_>);
}

macro_rules! documented {
    ($($input:ident),+) => {
        impl<F, Fut, R, M, $($input,)+> Documented<(M, $($input,)+)> for F
        where
            F: FnOnce($($input,)+) -> Fut,
            Fut: Future<Output = R>,
            R: Output,
            $($input: Input,)+
        {
            fn document(spec: &mut Spec<'_>) {
                $(<$input as Input>::input(spec);)+
                <R as Output>::output(spec);
            }
        }
    };
}

documented!(T1);
documented!(T1, T2);
documented!(T1, T2, T3);
documented!(T1, T2, T3, T4);
documented!(T1, T2, T3, T4, T5);

impl<S> Input for State<S> {
    fn input(_: &mut Spec<'_>) {}
}

impl<T> Input for Extension<T> {
    fn input(_: &mut Spec<'_>) {}
}

impl<T> Input for Path<T> {
    fn input(_: &mut Spec<'_>) {}
}

impl Input for Method {
    fn input(_: &mut Spec<'_>) {}
}

impl Input for Uri {
    fn input(_: &mut Spec<'_>) {}
}

impl<T: Input, E> Input for Result<T, E> {
    fn input(spec: &mut Spec<'_>) {
        T::input(spec);
    }
}

impl<T: JsonSchema> Output for Json<T> {
    fn output(spec: &mut Spec<'_>) {
        spec.json::<T>(StatusCode::OK);
    }
}

impl<T: Output, E: Output> Output for Result<T, E> {
    fn output(spec: &mut Spec<'_>) {
        T::output(spec);
        E::output(spec);
    }
}
