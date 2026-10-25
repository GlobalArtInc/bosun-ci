use axum::Json;
use axum::extract::{FromRequestParts, Request, State};
use axum::handler::Handler;
use axum::http::request::Parts;
use axum::http::{Method, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bosun_types::api::{CreateProjectRequest, Project};
use bosun_types::error::ErrorCode;
use bosun_types::id::ProjectId;
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::envelope::{self, ApiError};
use crate::openapi::{Api, Input, Spec};
use crate::reply::Created;
use crate::request::{Faults, Idempotent, Segment, database};
use crate::{AppState, idempotency, organisations, slug, workflows};

const WORKFLOWS: &str = "workflows";

pub(crate) fn routes() -> Api {
    Api::new()
        .post("/api/v1/projects", create)
        .route("/api/v1/projects/{*path}", get(read).post(write))
        .describe(Method::GET, "/api/v1/projects/{project}", show)
        .describe(
            Method::GET,
            "/api/v1/projects/{project}/workflows",
            workflows::list,
        )
        .describe(
            Method::POST,
            "/api/v1/projects/{project}/workflows",
            workflows::create,
        )
}

enum Reference {
    Id(String),
    Slugs(String, String),
}

enum Below {
    Nothing,
    Workflows,
}

fn parse(path: &str) -> Option<(Reference, Below)> {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let owned = |text: &str| text.to_owned();
    match segments.as_slice() {
        [id] => Some((Reference::Id(owned(id)), Below::Nothing)),
        [id, WORKFLOWS] if id.parse::<ProjectId>().is_ok() => {
            Some((Reference::Id(owned(id)), Below::Workflows))
        }
        [organisation, project] => Some((
            Reference::Slugs(owned(organisation), owned(project)),
            Below::Nothing,
        )),
        [organisation, project, WORKFLOWS] => Some((
            Reference::Slugs(owned(organisation), owned(project)),
            Below::Workflows,
        )),
        _ => None,
    }
}

pub(crate) struct ProjectPath(Reference);

impl<S: Send + Sync> FromRequestParts<S> for ProjectPath {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<ProjectPath, ApiError> {
        let Segment(path) = Segment::from_request_parts(parts, state).await?;
        let (reference, _) =
            parse(&path).ok_or_else(|| envelope::absent(&parts.method, &parts.uri))?;
        Ok(ProjectPath(reference))
    }
}

impl Input for ProjectPath {
    fn input(_: &mut Spec<'_>) {}
}

impl ProjectPath {
    pub(crate) async fn resolve<C: ConnectionTrait>(&self, db: &C) -> Result<Project, ApiError> {
        let (found, name) = match &self.0 {
            Reference::Id(id) => match id.parse::<ProjectId>() {
                Ok(id) => (bosun_db::projects::find(db, id).await, id.to_string()),
                Err(_) => (Ok(None), id.clone()),
            },
            Reference::Slugs(organisation, project) => (
                bosun_db::projects::find_by_slugs(db, organisation, project).await,
                format!("{organisation}/{project}"),
            ),
        };
        found.map_err(database)?.ok_or_else(|| {
            ApiError::new(ErrorCode::NotFound, format!("there is no project `{name}`"))
        })
    }
}

async fn create(
    State(state): State<AppState>,
    body: Idempotent<CreateProjectRequest>,
) -> Result<Created<Project>, ApiError> {
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
    idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), project).await
}

async fn show(
    State(state): State<AppState>,
    project: ProjectPath,
) -> Result<Json<Project>, ApiError> {
    project.resolve(&state.db).await.map(Json)
}

async fn read(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    Segment(path): Segment,
    request: Request,
) -> Response {
    match parse(&path) {
        Some((_, Below::Nothing)) => show.call(request, state).await,
        Some((_, Below::Workflows)) => workflows::list.call(request, state).await,
        None => envelope::absent(&method, &uri).into_response(),
    }
}

async fn write(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    Segment(path): Segment,
    request: Request,
) -> Response {
    match parse(&path) {
        Some((_, Below::Workflows)) => workflows::create.call(request, state).await,
        _ => envelope::absent(&method, &uri).into_response(),
    }
}
