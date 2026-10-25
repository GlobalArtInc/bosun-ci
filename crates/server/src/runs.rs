use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{Method, Uri};
use bosun_db::projects::Holder;
use bosun_types::api::{Execution, Job, Page, Run};
use bosun_types::error::ErrorCode;
use bosun_types::id::{ExecutionId, JobId, RunId};

use crate::AppState;
use crate::access::{Caller, Listed, OnProject, ProjectView};
use crate::envelope::{self, ApiError};
use crate::openapi::Api;
use crate::request::{Listing, Segment, database};

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/runs", list)
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

async fn list(
    State(state): State<AppState>,
    caller: Caller<Listed<ProjectView>>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<Run>>, ApiError> {
    let listing = listing?;
    let runs = bosun_db::views::runs(
        &state.db,
        &caller.reach(),
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(runs, |run| run.id.as_uuid())))
}

async fn run(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
) -> Result<Json<Run>, ApiError> {
    let absent = || missing("run", &reference);
    let id: RunId = reference.parse().map_err(|_| absent())?;
    caller.holding(&state.db, Holder::Run(id), absent).await?;
    bosun_db::views::run(&state.db, id)
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(absent)
}

async fn jobs(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
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
    let no_run = || missing("run", &run);
    let run_id: RunId = run.parse().map_err(|_| no_run())?;
    caller
        .holding(&state.db, Holder::Run(run_id), no_run)
        .await?;
    let number: u32 = attempt.parse().map_err(|_| absent())?;
    let listing = listing?;
    let Some(jobs) =
        bosun_db::views::jobs(&state.db, run_id, number, listing.cursor(), listing.fetch())
            .await
            .map_err(database)?
    else {
        return Err(absent());
    };
    Ok(Json(listing.page(jobs, |job| job.id.as_uuid())))
}

async fn job(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
) -> Result<Json<Job>, ApiError> {
    let absent = || missing("job", &reference);
    let id: JobId = reference.parse().map_err(|_| absent())?;
    caller.holding(&state.db, Holder::Job(id), absent).await?;
    bosun_db::views::job(&state.db, id)
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(absent)
}

async fn execution(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
) -> Result<Json<Execution>, ApiError> {
    let absent = || missing("execution", &reference);
    let id: ExecutionId = reference.parse().map_err(|_| absent())?;
    caller
        .holding(&state.db, Holder::Execution(id), absent)
        .await?;
    bosun_db::views::execution(&state.db, id)
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(absent)
}
