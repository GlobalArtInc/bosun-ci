use axum::Json;
use axum::extract::State;
use bosun_db::projects::Holder;
use bosun_db::workflows::NewWorkflow;
use bosun_types::api::{CreateWorkflowRequest, Page, Revision, UpdateSourceRequest, Workflow};
use bosun_types::diagnostic::Severity;
use bosun_types::error::{ErrorBody, ErrorCode, FieldError};
use bosun_types::id::WorkflowId;
use sea_orm::{ConnectionTrait, TransactionTrait};
use sha2::{Digest, Sha256};

use crate::access::{Caller, Key, OnProject, ProjectView, WorkflowEdit};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::projects::ProjectPath;
use crate::reply::{Created, Saved};
use crate::request::{Body, Faults, Filters, Idempotent, Listing, Segment, database};
use crate::{AppState, dispatch, idempotency, slug};

const NAME: &str = "name";

pub(crate) struct ByName;

impl Filters for ByName {
    const NAMES: &'static [&'static str] = &[NAME];
}

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/workflows/{workflow}", show)
        .put("/api/v1/workflows/{workflow}/source", save)
        .get("/api/v1/workflows/{workflow}/revisions", revisions)
        .post("/api/v1/workflows/{workflow}/dispatch", dispatch::dispatch)
}

pub(crate) async fn find<C: ConnectionTrait, K: Key>(
    db: &C,
    caller: &Caller<OnProject<K>>,
    reference: &str,
) -> Result<Workflow, ApiError> {
    let missing = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no workflow `{reference}`"),
        )
    };
    let id: WorkflowId = reference.parse().map_err(|_| missing())?;
    caller.holding(db, Holder::Workflow(id), missing).await?;
    bosun_db::workflows::find(db, id)
        .await
        .map_err(database)?
        .ok_or_else(missing)
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
    State(state): State<AppState>,
    caller: Caller<OnProject<WorkflowEdit>>,
    project: ProjectPath,
    body: Result<Idempotent<CreateWorkflowRequest>, ApiError>,
) -> Result<Created<Workflow>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let body = body?;
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
    idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), workflow).await
}

pub(crate) async fn list(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    project: ProjectPath,
    listing: Result<Listing<ByName>, ApiError>,
) -> Result<Json<Page<Workflow>>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let listing = listing?;
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
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
) -> Result<Json<Workflow>, ApiError> {
    find(&state.db, &caller, &reference).await.map(Json)
}

async fn save(
    State(state): State<AppState>,
    caller: Caller<OnProject<WorkflowEdit>>,
    Segment(reference): Segment,
    body: Result<Body<UpdateSourceRequest>, ApiError>,
) -> Result<Saved<Revision>, ApiError> {
    let workflow = find(&state.db, &caller, &reference).await?;
    let Body(UpdateSourceRequest { source }) = body?;
    validate(&workflow.name, &source)?;
    let transaction = state.db.begin().await.map_err(database)?;
    let saved = bosun_db::workflows::save(&transaction, workflow.id, &source, sha256(&source))
        .await
        .map_err(database)?;
    transaction.commit().await.map_err(database)?;
    Ok(if saved.created {
        Saved::New(saved.revision)
    } else {
        Saved::Again(saved.revision)
    })
}

async fn revisions(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<Revision>>, ApiError> {
    let workflow = find(&state.db, &caller, &reference).await?;
    let listing = listing?;
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
