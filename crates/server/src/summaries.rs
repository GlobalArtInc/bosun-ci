use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bosun_blob::{Key, Store as _};
use bosun_db::projects::Holder;
use bosun_db::results::StoredSummary;
use bosun_types::error::ErrorCode;
use bosun_types::id::{ExecutionId, StepId};
use serde_json::json;
use tokio_util::io::ReaderStream;

use crate::AppState;
use crate::access::{Caller, OnProject, ProjectView};
use crate::envelope::{self, ApiError};
use crate::openapi::{Api, Output, Spec};
use crate::request::{database, failed};

const MARKDOWN: &str = "text/markdown; charset=utf-8";

const CHUNK: usize = 64 * 1024;

pub(crate) struct Markdown(Response);

impl IntoResponse for Markdown {
    fn into_response(self) -> Response {
        self.0
    }
}

impl Output for Markdown {
    fn output(spec: &mut Spec<'_>) {
        spec.content(
            StatusCode::OK,
            json!({ MARKDOWN: { "schema": { "type": "string" } } }),
        );
    }
}

pub(crate) fn routes() -> Api {
    Api::new().get("/api/v1/executions/{execution}/steps/{step}/summary", show)
}

async fn show(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    method: Method,
    uri: Uri,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Markdown, ApiError> {
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
    caller
        .holding(&state.db, Holder::Execution(execution_id), missing)
        .await?;
    let expired = || {
        ApiError::new(
            ErrorCode::Expired,
            format!(
                "retention removed the summary of step `{step}` of execution `{execution}`; the \
                 execution's `logs_expired_at` says when"
            ),
        )
    };
    let summary = match bosun_db::results::summary(&state.db, execution_id, step_id)
        .await
        .map_err(database)?
        .ok_or_else(missing)?
    {
        StoredSummary::Kept(summary) => summary,
        StoredSummary::Expired => return Err(expired()),
        StoredSummary::Absent => {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!("step `{step}` of execution `{execution}` has no summary"),
            ));
        }
    };
    let key = Key::new(summary.key).map_err(failed)?;
    let object = match state.store.get(&key, ..).await {
        Ok(object) => object,
        Err(bosun_blob::Error::NotFound(_)) => return Err(expired()),
        Err(error) => return Err(failed(error)),
    };
    Ok(Markdown(
        (
            [
                (CONTENT_TYPE, MARKDOWN.to_owned()),
                (CONTENT_LENGTH, summary.size.to_string()),
            ],
            Body::from_stream(ReaderStream::with_capacity(object, CHUNK)),
        )
            .into_response(),
    ))
}
