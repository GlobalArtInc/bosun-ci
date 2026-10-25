use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{Method, Uri};
use bosun_types::api::{Execution, Job, Page, Run};
use bosun_types::error::ErrorCode;
use bosun_types::id::{ExecutionId, JobId, RunId};

use crate::AppState;
use crate::envelope::{self, ApiError};
use crate::openapi::Api;
use crate::request::{Listing, Segment, database};

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/runs/{run}", run)
        .get("/api/v1/runs/{run}/attempts/{attempt}/jobs", jobs)
        .get("/api/v1/jobs/{job}", job)
        .get("/api/v1/executions/{execution}", execution)
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
    listing: Result<Listing, ApiError>,
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
    let listing = listing?;
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
