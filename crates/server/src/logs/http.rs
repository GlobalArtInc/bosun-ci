use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bosun_types::api::{LogEnd, LogLine};
use bosun_types::error::ErrorCode;
use bosun_types::id::{ExecutionId, StepId};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;

use super::followers::Subscription;
use super::reader::{Failure, Read, Reader};
use crate::AppState;
use crate::envelope::{self, ApiError};
use crate::request::{failed, invalid};

const OFFSET: &str = "offset";

const LIMIT: &str = "limit";

const FOLLOW: &str = "follow";

const LAST_EVENT_ID: &str = "last-event-id";

const POLL: Duration = Duration::from_secs(1);

const BUFFERED: usize = 64;

struct Asked {
    offset: u64,
    limit: Option<u64>,
    follow: bool,
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/executions/{execution}/steps/{step}/log", get(show))
}

async fn show(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    path: Result<Path<(String, String)>, PathRejection>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Ok(Path((execution, step))) = path else {
        return Err(envelope::absent(&method, &uri));
    };
    let missing = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no step `{step}` in execution `{execution}`"),
        )
    };
    let execution_id: ExecutionId = execution.parse().map_err(|_| missing())?;
    let step_id: StepId = step.parse().map_err(|_| missing())?;
    let asked = parse(query, &headers)?;
    let subscription = asked
        .follow
        .then(|| state.followers.subscribe(execution_id));
    let mut reader = Reader::new(
        state.db.clone(),
        Arc::clone(&state.store),
        execution_id,
        step_id,
        asked.offset,
    );
    let first = match reader.read().await {
        Ok(first) => first,
        Err(Failure::Missing) => return Err(missing()),
        Err(failure) => return Err(failed(failure)),
    };
    Ok(match subscription {
        Some(subscription) => follow(state, reader, first, subscription),
        None => range(reader, first, asked.limit),
    })
}

fn parse(
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
    headers: &HeaderMap,
) -> Result<Asked, ApiError> {
    let Query(pairs) = query.map_err(|rejection| {
        invalid(format!(
            "the query string could not be read: {}",
            rejection.body_text()
        ))
    })?;
    let mut asked = Asked {
        offset: 0,
        limit: None,
        follow: false,
    };
    for (name, value) in pairs {
        match name.as_str() {
            OFFSET => {
                asked.offset = value.parse().map_err(|_| {
                    invalid(format!(
                        "`{OFFSET}` is `{value}`, which is not a number of lines"
                    ))
                })?;
            }
            LIMIT => {
                asked.limit = Some(value.parse().ok().filter(|limit| *limit > 0).ok_or_else(
                    || {
                        invalid(format!(
                            "`{LIMIT}` is `{value}`, and a range holds at least one line"
                        ))
                    },
                )?);
            }
            FOLLOW => {
                asked.follow = value.parse().map_err(|_| {
                    invalid(format!(
                        "`{FOLLOW}` is `{value}`, which is neither `true` nor `false`"
                    ))
                })?;
            }
            _ => {
                return Err(invalid(format!(
                    "a log has no query parameter `{name}`; it takes `{OFFSET}`, `{LIMIT}` and \
                     `{FOLLOW}`"
                )));
            }
        }
    }
    if asked.follow {
        if asked.limit.is_some() {
            return Err(invalid(format!(
                "`{LIMIT}` does not apply to a followed log, which ends with its step"
            )));
        }
        if let Some(value) = headers.get(LAST_EVENT_ID) {
            asked.offset = value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| {
                    invalid("the Last-Event-ID header is not the number of a line of the log")
                })?;
        }
    }
    Ok(asked)
}

fn range(mut reader: Reader, first: Read, limit: Option<u64>) -> Response {
    let (sender, receiver) = mpsc::channel::<Result<Bytes, io::Error>>(BUFFERED);
    tokio::spawn(async move {
        let mut remaining = limit.unwrap_or(u64::MAX);
        let mut next: Result<Read, Failure> = Ok(first);
        loop {
            let lines = match next {
                Ok(Read::Lines(lines)) => lines,
                Ok(Read::Waiting | Read::End(_)) => return,
                Err(failure) => {
                    warn!(error = %failure, "a log could not be read to its end");
                    let _ = sender
                        .send(Err(io::Error::other(failure.to_string())))
                        .await;
                    return;
                }
            };
            let mut body = Vec::new();
            for line in lines
                .into_iter()
                .take(usize::try_from(remaining).unwrap_or(usize::MAX))
            {
                body.extend_from_slice(&line.text);
                body.push(b'\n');
                remaining -= 1;
            }
            if !body.is_empty() && sender.send(Ok(Bytes::from(body))).await.is_err() {
                return;
            }
            if remaining == 0 {
                return;
            }
            next = reader.read().await;
        }
    });
    (
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        Body::from_stream(ReceiverStream::new(receiver)),
    )
        .into_response()
}

fn follow(
    state: AppState,
    mut reader: Reader,
    first: Read,
    mut subscription: Subscription,
) -> Response {
    let (sender, receiver) = mpsc::channel::<Result<Event, Infallible>>(BUFFERED);
    tokio::spawn(async move {
        let stopping = state.shutdown.reason();
        tokio::pin!(stopping);
        let mut next: Result<Read, Failure> = Ok(first);
        loop {
            match next {
                Ok(Read::Lines(lines)) => {
                    for line in lines {
                        let event =
                            Event::default()
                                .id(line.number.to_string())
                                .json_data(LogLine {
                                    number: line.number,
                                    line: String::from_utf8_lossy(&line.text).into_owned(),
                                });
                        let Ok(event) = event else {
                            return;
                        };
                        if sender.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Ok(Read::End(lines)) => {
                    if let Ok(event) = Event::default().event("end").json_data(LogEnd { lines }) {
                        let _ = sender.send(Ok(event)).await;
                    }
                    return;
                }
                Ok(Read::Waiting) => {
                    tokio::select! {
                        () = subscription.announced() => {}
                        () = sleep(POLL) => {}
                        () = sender.closed() => return,
                        _ = &mut stopping => return,
                    }
                }
                Err(failure) => {
                    warn!(error = %failure, "a followed log could not be read");
                    return;
                }
            }
            next = tokio::select! {
                biased;
                _ = &mut stopping => return,
                next = reader.read() => next,
            };
        }
    });
    Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response()
}
