use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{Method, Uri};
use bosun_db::projects::{Holder, Reach};
use bosun_db::views::{RunFilter, StatusOrConclusion};
use bosun_types::api::{Execution, Job, Page, Run};
use bosun_types::error::ErrorCode;
use bosun_types::id::{ExecutionId, JobId, ProjectId, RunId, WorkflowId};
use bosun_types::status::{RunConclusion, RunStatus};
use serde::Serialize;
use serde_json::Value;

use crate::access::{Caller, Listed, OnProject, ProjectView};
use crate::envelope::{self, ApiError};
use crate::openapi::Api;
use crate::request::{Faults, Filters, Listing, Segment, database};
use crate::{AppState, projects};

const PROJECT: &str = "project";

const WORKFLOW: &str = "workflow";

const BRANCH: &str = "branch";

const EVENT: &str = "event";

const STATUS: &str = "status";

const ACTOR: &str = "actor";

pub(crate) struct RunFilters;

impl Filters for RunFilters {
    const NAMES: &'static [&'static str] = &[PROJECT, WORKFLOW, BRANCH, EVENT, STATUS, ACTOR];

    fn description(name: &str) -> String {
        match name {
            PROJECT => "only the runs of this project, its id or `<organisation>/<project>`",
            WORKFLOW => "only the runs of this workflow, its id",
            BRANCH => "only the runs whose ref is this branch, `refs/heads/<branch>`",
            EVENT => "only the runs of this event, such as `push` or `workflow_dispatch`",
            STATUS => "only the runs with this status, or with this conclusion",
            ACTOR => {
                "only the runs whose actor has this login: a user, the user of a token or an \
                 account of the forge"
            }
            _ => "only the runs that match this value",
        }
        .to_owned()
    }
}

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
    listing: Result<Listing<RunFilters>, ApiError>,
) -> Result<Json<Page<Run>>, ApiError> {
    let listing = listing?;
    let reach = caller.reach();
    let mut faults = Faults::default();
    let mut filter = RunFilter {
        branch: listing.filter(BRANCH),
        event: listing.filter(EVENT),
        actor: listing.filter(ACTOR),
        ..RunFilter::default()
    };
    if let Some(reference) = listing.filter(PROJECT) {
        filter.project = project(&state, &reach, reference).await?;
        if filter.project.is_none() {
            faults.add(PROJECT, projects::missing(reference));
        }
    }
    if let Some(reference) = listing.filter(WORKFLOW) {
        filter.workflow = workflow(&state, &reach, reference).await?;
        if filter.workflow.is_none() {
            faults.add(WORKFLOW, format!("there is no workflow `{reference}`"));
        }
    }
    if let Some(value) = listing.filter(STATUS) {
        filter.status = states()
            .find(|(name, _)| name == value)
            .map(|(_, state)| state);
        if filter.status.is_none() {
            let names: Vec<String> = states().map(|(name, _)| format!("`{name}`")).collect();
            faults.add(
                STATUS,
                format!(
                    "`{value}` is neither a status nor a conclusion of a run; they are {}",
                    names.join(", ")
                ),
            );
        }
    }
    faults.finish("the query is not valid")?;
    let runs = bosun_db::views::runs(
        &state.db,
        &reach,
        &filter,
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(runs, |run| run.id.as_uuid())))
}

async fn project(
    state: &AppState,
    reach: &Reach,
    reference: &str,
) -> Result<Option<ProjectId>, ApiError> {
    Ok(projects::find(&state.db, reference)
        .await?
        .filter(|project| reach.reaches(project))
        .map(|project| project.id))
}

async fn workflow(
    state: &AppState,
    reach: &Reach,
    reference: &str,
) -> Result<Option<WorkflowId>, ApiError> {
    let Ok(id) = reference.parse::<WorkflowId>() else {
        return Ok(None);
    };
    Ok(bosun_db::projects::holding(&state.db, Holder::Workflow(id))
        .await
        .map_err(database)?
        .filter(|project| reach.reaches(project))
        .map(|_| id))
}

fn states() -> impl Iterator<Item = (String, StatusOrConclusion)> {
    RunStatus::ALL
        .into_iter()
        .map(|status| (spelled(status), StatusOrConclusion::Status(status)))
        .chain(RunConclusion::ALL.into_iter().map(|conclusion| {
            (
                spelled(conclusion),
                StatusOrConclusion::Conclusion(conclusion),
            )
        }))
}

fn spelled(value: impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        _ => String::new(),
    }
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
