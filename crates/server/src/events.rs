mod hub;

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::sse::{Event as Message, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use bosun_db::projects::{Holder, Reach};
use bosun_db::stream::{Cursor, Delivered, Filter};
use bosun_types::event::{Change, Event, RunEvent, StreamEvent};
use bosun_types::id::{ProjectId, RunId};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;

use crate::AppState;
use crate::access::{Caller, Subscribed};
use crate::envelope::ApiError;
use crate::openapi::{Api, Input, Output, Spec};
use crate::request::{database, invalid};

pub(crate) use hub::Hub;

const PROJECT: &str = "project";

const RUN: &str = "run";

const KIND: &str = "kind";

const LAST_EVENT_ID: &str = "Last-Event-ID";

const BATCH: u64 = 256;

const BUFFERED: usize = 64;

const POLL: Duration = Duration::from_secs(5);

const FIRST_RETRY: Duration = Duration::from_millis(50);

const LAST_RETRY: Duration = Duration::from_secs(1);

const RECONNECT: Duration = Duration::from_secs(1);

struct Asked {
    projects: Vec<String>,
    runs: Vec<String>,
    kinds: Vec<String>,
    last: Option<Cursor>,
}

struct Events(Response);

impl IntoResponse for Events {
    fn into_response(self) -> Response {
        self.0
    }
}

pub(crate) fn routes() -> Api {
    Api::new().get("/api/v1/events", stream)
}

async fn stream(
    State(state): State<AppState>,
    caller: Caller<Subscribed>,
    asked: Result<Asked, ApiError>,
) -> Result<Events, ApiError> {
    let asked = asked?;
    let reach = caller.reach();
    let mut projects = Vec::new();
    for reference in &asked.projects {
        projects.push(project(&state, &reach, reference).await?);
    }
    let mut runs = Vec::new();
    for reference in &asked.runs {
        runs.push(run(&state, &reach, reference).await?);
    }
    let filter = Filter {
        reach,
        installation: caller.installation(),
        projects,
        runs: runs.clone(),
        kinds: asked.kinds,
    };
    let subscription = state.events.subscribe(runs);
    let cursor = match asked.last {
        Some(cursor) => cursor,
        None => bosun_db::stream::start(&state.db).await.map_err(database)?,
    };
    let (sender, receiver) = mpsc::channel::<Result<Message, Infallible>>(BUFFERED);
    tokio::spawn(follow(state, filter, cursor, subscription, sender));
    Ok(Events(
        Sse::new(ReceiverStream::new(receiver))
            .keep_alive(KeepAlive::default())
            .into_response(),
    ))
}

async fn follow(
    state: AppState,
    filter: Filter,
    mut cursor: Cursor,
    mut subscription: hub::Subscription,
    sender: mpsc::Sender<Result<Message, Infallible>>,
) {
    let begun = Message::default().id(cursor.to_string()).retry(RECONNECT);
    if sender.send(Ok(begun)).await.is_err() {
        return;
    }
    let stopping = state.shutdown.reason();
    tokio::pin!(stopping);
    let ending = sleep(state.events.lifetime());
    tokio::pin!(ending);
    let mut retry = FIRST_RETRY;
    loop {
        let read = tokio::select! {
            biased;
            _ = &mut stopping => return,
            () = &mut ending => return,
            read = bosun_db::stream::read(&state.db, &filter, cursor, BATCH) => read,
        };
        let read = match read {
            Ok(read) => read,
            Err(failure) => {
                warn!(error = %failure, "the event stream could not be read");
                return;
            }
        };
        if read.events.is_empty() {
            let held = if read.held {
                let pause = retry;
                retry = (retry * 2).min(LAST_RETRY);
                Some(pause)
            } else {
                retry = FIRST_RETRY;
                None
            };
            let woken = async {
                match held {
                    Some(pause) => sleep(pause).await,
                    None => {
                        tokio::select! {
                            () = subscription.announced() => {}
                            () = sleep(POLL) => {}
                        }
                    }
                }
            };
            tokio::select! {
                () = woken => {}
                () = sender.closed() => return,
                _ = &mut stopping => return,
                () = &mut ending => return,
            }
            continue;
        }
        retry = FIRST_RETRY;
        for (position, delivered) in read.events {
            cursor = position;
            let message = Message::default().id(position.to_string());
            let message = match &delivered {
                Delivered::Run(event) => message.json_data(event),
                Delivered::Stream(event) => message.json_data(event),
            };
            let Ok(message) = message else {
                return;
            };
            if sender.send(Ok(message)).await.is_err() {
                return;
            }
        }
    }
}

async fn project(state: &AppState, reach: &Reach, reference: &str) -> Result<ProjectId, ApiError> {
    let absent = || invalid(format!("there is no project `{reference}`"));
    let found = match reference.split_once('/') {
        Some((organisation, slug)) => {
            bosun_db::projects::find_by_slugs(&state.db, organisation, slug)
                .await
                .map_err(database)?
        }
        None => match reference.parse() {
            Ok(id) => bosun_db::projects::find(&state.db, id)
                .await
                .map_err(database)?,
            Err(_) => None,
        },
    };
    found
        .filter(|project| reach.reaches(project))
        .map(|project| project.id)
        .ok_or_else(absent)
}

async fn run(state: &AppState, reach: &Reach, reference: &str) -> Result<RunId, ApiError> {
    let absent = || invalid(format!("there is no run `{reference}`"));
    let id: RunId = reference.parse().map_err(|_| absent())?;
    bosun_db::projects::holding(&state.db, Holder::Run(id))
        .await
        .map_err(database)?
        .filter(|project| reach.reaches(project))
        .map(|_| id)
        .ok_or_else(absent)
}

fn kinds() -> impl Iterator<Item = &'static str> {
    Event::KINDS.into_iter().chain(Change::KINDS)
}

impl<S: Send + Sync> FromRequestParts<S> for Asked {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Asked, ApiError> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| {
                invalid(format!(
                    "the query string could not be read: {}",
                    rejection.body_text()
                ))
            })?;
        let mut asked = Asked {
            projects: Vec::new(),
            runs: Vec::new(),
            kinds: Vec::new(),
            last: None,
        };
        for (name, value) in pairs {
            match name.as_str() {
                PROJECT => asked.projects.push(value),
                RUN => asked.runs.push(value),
                KIND => {
                    if !kinds().any(|kind| kind == value) {
                        return Err(invalid(format!(
                            "the event stream has no kind `{value}`; its kinds are {}",
                            kinds()
                                .map(|kind| format!("`{kind}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                    asked.kinds.push(value);
                }
                _ => {
                    return Err(invalid(format!(
                        "the event stream has no query parameter `{name}`; it takes `{PROJECT}`, \
                         `{RUN}` and `{KIND}`"
                    )));
                }
            }
        }
        if let Some(value) = parts.headers.get(LAST_EVENT_ID) {
            let text = String::from_utf8_lossy(value.as_bytes());
            asked.last = Some(
                text.parse()
                    .map_err(|error: bosun_db::stream::CursorError| invalid(error.to_string()))?,
            );
        }
        Ok(asked)
    }
}

impl Input for Asked {
    fn input(spec: &mut Spec<'_>) {
        let kinds: Vec<&str> = kinds().collect();
        spec.parameter(json!({
            "name": PROJECT,
            "in": "query",
            "description": "only the events of these projects, each its id or the slugs of its organisation and of itself joined by `/`",
            "schema": { "type": "array", "items": { "type": "string" } }
        }));
        spec.parameter(json!({
            "name": RUN,
            "in": "query",
            "description": "only the events of these runs",
            "schema": { "type": "array", "items": { "type": "string", "format": "uuid" } }
        }));
        spec.parameter(json!({
            "name": KIND,
            "in": "query",
            "description": "only the events of these kinds",
            "schema": { "type": "array", "items": { "type": "string", "enum": kinds } }
        }));
        spec.parameter(json!({
            "name": LAST_EVENT_ID,
            "in": "header",
            "description": "the id of the last message a stream sent, to resume after it",
            "schema": { "type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+$" }
        }));
    }
}

impl Output for Events {
    fn output(spec: &mut Spec<'_>) {
        let run = spec.schema::<RunEvent>();
        let stream = spec.schema::<StreamEvent>();
        spec.content(
            StatusCode::OK,
            json!({
                "text/event-stream": {
                    "schema": { "type": "string" },
                    "x-bosun-events": { "message": { "oneOf": [run, stream] } }
                }
            }),
        );
    }
}
