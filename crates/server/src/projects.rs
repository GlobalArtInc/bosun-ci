use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bosun_types::api::{CreateProjectRequest, CreateWorkflowRequest, Project};
use bosun_types::error::ErrorCode;
use bosun_types::id::ProjectId;
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::envelope::{self, ApiError};
use crate::request::{Body, Faults, Segment, database};
use crate::{AppState, idempotency, organisations, slug, workflows};

const WORKFLOWS: &str = "workflows";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects", post(create))
        .route("/api/v1/projects/{*path}", get(read).post(write))
}

enum Reference<'a> {
    Id(&'a str),
    Slugs(&'a str, &'a str),
}

enum Below {
    Nothing,
    Workflows,
}

fn parse(path: &str) -> Option<(Reference<'_>, Below)> {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    match segments.as_slice() {
        [id] => Some((Reference::Id(id), Below::Nothing)),
        [id, WORKFLOWS] if id.parse::<ProjectId>().is_ok() => {
            Some((Reference::Id(id), Below::Workflows))
        }
        [organisation, project] => Some((Reference::Slugs(organisation, project), Below::Nothing)),
        [organisation, project, WORKFLOWS] => {
            Some((Reference::Slugs(organisation, project), Below::Workflows))
        }
        _ => None,
    }
}

async fn resolve<C: ConnectionTrait>(
    db: &C,
    reference: &Reference<'_>,
) -> Result<Project, ApiError> {
    let (found, name) = match reference {
        Reference::Id(id) => match id.parse::<ProjectId>() {
            Ok(id) => (bosun_db::projects::find(db, id).await, id.to_string()),
            Err(_) => (Ok(None), (*id).to_owned()),
        },
        Reference::Slugs(organisation, project) => (
            bosun_db::projects::find_by_slugs(db, organisation, project).await,
            format!("{organisation}/{project}"),
        ),
    };
    found
        .map_err(database)?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("there is no project `{name}`")))
}

async fn create(
    State(state): State<AppState>,
    body: Body<CreateProjectRequest>,
) -> Result<Response, ApiError> {
    let request = &body.value;
    let mut faults = Faults::default();
    slug::check_slug(&mut faults, "slug", "a slug", &request.slug);
    slug::check_name(&mut faults, "name", &request.name);
    faults.finish("the project is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let Some(organisation) = organisations::find(&transaction, &request.organisation).await? else {
        let mut faults = Faults::default();
        faults.add(
            "organisation",
            organisations::missing(&request.organisation),
        );
        return Err(faults.refuse("the project is not valid"));
    };
    let project =
        bosun_db::projects::create(&transaction, organisation.id, &request.slug, &request.name)
            .await
            .map_err(database)?;
    idempotency::commit(
        &state.db,
        transaction,
        body.idempotency.as_ref(),
        StatusCode::CREATED,
        &project,
    )
    .await
}

async fn read(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    Segment(path): Segment,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, ApiError> {
    let (reference, below) = parse(&path).ok_or_else(|| envelope::absent(&method, &uri))?;
    let project = resolve(&state.db, &reference).await?;
    match below {
        Below::Nothing => Ok(Json(project).into_response()),
        Below::Workflows => Ok(workflows::list(&state, &project, query)
            .await?
            .into_response()),
    }
}

async fn write(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    Segment(path): Segment,
    body: Result<Body<CreateWorkflowRequest>, ApiError>,
) -> Result<Response, ApiError> {
    let Some((reference, Below::Workflows)) = parse(&path) else {
        return Err(envelope::absent(&method, &uri));
    };
    let project = resolve(&state.db, &reference).await?;
    workflows::create(&state, &project, body?).await
}
