use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use bosun_db::workflows::NewWorkflow;
use bosun_types::api::{
    CreateWorkflowRequest, Page, Project, Revision, UpdateSourceRequest, Workflow,
};
use bosun_types::diagnostic::Severity;
use bosun_types::error::{ErrorBody, ErrorCode, FieldError};
use bosun_types::id::WorkflowId;
use sea_orm::{ConnectionTrait, TransactionTrait};
use sha2::{Digest, Sha256};

use crate::envelope::ApiError;
use crate::request::{Body, Faults, Listing, Segment, database};
use crate::{AppState, dispatch, idempotency, slug};

const NAME: &str = "name";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/workflows/{workflow}", get(show))
        .route("/api/v1/workflows/{workflow}/source", put(save))
        .route("/api/v1/workflows/{workflow}/revisions", get(revisions))
        .route(
            "/api/v1/workflows/{workflow}/dispatch",
            post(dispatch::dispatch),
        )
}

pub(crate) async fn find<C: ConnectionTrait>(
    db: &C,
    reference: &str,
) -> Result<Workflow, ApiError> {
    let found = match reference.parse::<WorkflowId>() {
        Ok(id) => bosun_db::workflows::find(db, id).await.map_err(database)?,
        Err(_) => None,
    };
    found.ok_or_else(|| {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no workflow `{reference}`"),
        )
    })
}

fn validate(name: &str, source: &str) -> Result<(), ApiError> {
    bosun_workflow::parse(name, source)
        .map(drop)
        .map_err(|diagnostics| {
            let errors = diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .count();
            let plural = if errors == 1 { "" } else { "s" };
            ApiError(ErrorBody {
                code: ErrorCode::InvalidInput,
                message: "the workflow is not valid".to_owned(),
                fields: vec![FieldError {
                    field: "source".to_owned(),
                    message: format!("the workflow has {errors} error{plural}"),
                }],
                diagnostics,
            })
        })
}

pub(crate) async fn create(
    state: &AppState,
    project: &Project,
    body: Body<CreateWorkflowRequest>,
) -> Result<Response, ApiError> {
    let request = &body.value;
    let mut faults = Faults::default();
    slug::check_slug(&mut faults, NAME, "a workflow name", &request.name);
    faults.finish("the workflow is not valid")?;
    validate(&request.name, &request.source)?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let workflow = bosun_db::workflows::create(
        &transaction,
        NewWorkflow {
            project_id: project.id,
            name: &request.name,
            source: &request.source,
            sha256: sha256(&request.source),
        },
    )
    .await
    .map_err(database)?;
    idempotency::commit(
        &state.db,
        transaction,
        body.idempotency.as_ref(),
        StatusCode::CREATED,
        &workflow,
    )
    .await
}

pub(crate) async fn list(
    state: &AppState,
    project: &Project,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<Page<Workflow>>, ApiError> {
    let listing = Listing::parse(query, &[NAME])?;
    let workflows = bosun_db::workflows::list(
        &state.db,
        project.id,
        listing.filter(NAME),
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(
        listing.page(workflows, |workflow| workflow.id.as_uuid()),
    ))
}

async fn show(
    State(state): State<AppState>,
    Segment(reference): Segment,
) -> Result<Json<Workflow>, ApiError> {
    find(&state.db, &reference).await.map(Json)
}

async fn save(
    State(state): State<AppState>,
    Segment(reference): Segment,
    body: Result<Body<UpdateSourceRequest>, ApiError>,
) -> Result<Response, ApiError> {
    let workflow = find(&state.db, &reference).await?;
    let source = body?.value.source;
    validate(&workflow.name, &source)?;
    let transaction = state.db.begin().await.map_err(database)?;
    let saved = bosun_db::workflows::save(&transaction, workflow.id, &source, sha256(&source))
        .await
        .map_err(database)?;
    transaction.commit().await.map_err(database)?;
    let status = if saved.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(saved.revision)).into_response())
}

async fn revisions(
    State(state): State<AppState>,
    Segment(reference): Segment,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<Page<Revision>>, ApiError> {
    let workflow = find(&state.db, &reference).await?;
    let listing = Listing::parse(query, &[])?;
    let revisions =
        bosun_db::workflows::revisions(&state.db, workflow.id, listing.cursor(), listing.fetch())
            .await
            .map_err(database)?;
    Ok(Json(
        listing.page(revisions, |revision| revision.id.as_uuid()),
    ))
}

fn sha256(source: &str) -> Vec<u8> {
    Sha256::digest(source.as_bytes()).to_vec()
}
