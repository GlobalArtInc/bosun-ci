use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{Method, Uri};
use axum::routing::get;
use axum::{Json, Router};
use bosun_types::api::{Execution, Job, Page, Run};
use bosun_types::error::ErrorCode;
use bosun_types::id::{ExecutionId, JobId, RunId};

use crate::AppState;
use crate::envelope::{self, ApiError};
use crate::request::{Listing, Segment, database};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/runs/{run}", get(run))
        .route("/api/v1/runs/{run}/attempts/{attempt}/jobs", get(jobs))
        .route("/api/v1/jobs/{job}", get(job))
        .route("/api/v1/executions/{execution}", get(execution))
}

fn missing(what: &str, reference: &str) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("there is no {what} `{reference}`"),
    )
}

async fn run(
    State(state): State<AppState>,
    Segment(reference): Segment,
) -> Result<Json<Run>, ApiError> {
    let id: RunId = reference.parse().map_err(|_| missing("run", &reference))?;
    bosun_db::views::run(&state.db, id)
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(|| missing("run", &reference))
}

async fn jobs(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    path: Result<Path<(String, String)>, PathRejection>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<Page<Job>>, ApiError> {
    let Ok(Path((run, attempt))) = path else {
        return Err(envelope::absent(&method, &uri));
    };
    let absent = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no attempt `{attempt}` of run `{run}`"),
        )
    };
    let run_id: RunId = run.parse().map_err(|_| missing("run", &run))?;
    let number: u32 = attempt.parse().map_err(|_| absent())?;
    let listing = Listing::parse(query, &[])?;
    let Some(jobs) =
        bosun_db::views::jobs(&state.db, run_id, number, listing.cursor(), listing.fetch())
            .await
            .map_err(database)?
    else {
        return Err(
            match bosun_db::views::run(&state.db, run_id)
                .await
                .map_err(database)?
            {
                Some(_) => absent(),
                None => missing("run", &run),
            },
        );
    };
    Ok(Json(listing.page(jobs, |job| job.id.as_uuid())))
}

async fn job(
    State(state): State<AppState>,
    Segment(reference): Segment,
) -> Result<Json<Job>, ApiError> {
    let id: JobId = reference.parse().map_err(|_| missing("job", &reference))?;
    bosun_db::views::job(&state.db, id)
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(|| missing("job", &reference))
}

async fn execution(
    State(state): State<AppState>,
    Segment(reference): Segment,
) -> Result<Json<Execution>, ApiError> {
    let id: ExecutionId = reference
        .parse()
        .map_err(|_| missing("execution", &reference))?;
    bosun_db::views::execution(&state.db, id)
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(|| missing("execution", &reference))
}
