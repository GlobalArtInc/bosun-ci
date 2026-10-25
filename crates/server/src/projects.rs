use axum::Json;
use axum::extract::{FromRequestParts, Request, State};
use axum::handler::Handler;
use axum::http::request::Parts;
use axum::http::{Method, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bosun_db::installations::Visibility;
use bosun_types::api::{
    CreateProjectRequest, Organisation, Page, Project, Repository, UpdateProjectRequest,
};
use bosun_types::error::ErrorCode;
use bosun_types::id::ProjectId;
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::access::{Caller, Key, Listed, OnOrganisation, OnProject, ProjectManage, ProjectView};
use crate::envelope::{self, ApiError};
use crate::openapi::{Api, Input, Spec};
use crate::reply::Created;
use crate::request::{Body, Faults, Idempotent, Listing, Segment, database, failed};
use crate::{AppState, forge_events, idempotency, organisations, settings, slug, workflows};

const WORKFLOWS: &str = "workflows";

const SETTINGS: &str = "settings";

const EVENTS: &str = "events";

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/projects", list)
        .post("/api/v1/projects", create)
        .route(
            "/api/v1/projects/{*path}",
            get(read).post(write).patch(change),
        )
        .describe(Method::GET, "/api/v1/projects/{project}", show)
        .describe(Method::PATCH, "/api/v1/projects/{project}", update)
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
        .describe(
            Method::GET,
            "/api/v1/projects/{project}/settings",
            settings::show,
        )
        .describe(
            Method::PATCH,
            "/api/v1/projects/{project}/settings",
            settings::change,
        )
        .describe(
            Method::POST,
            "/api/v1/projects/{project}/events",
            forge_events::receive,
        )
}

enum Reference {
    Id(String),
    Slugs(String, String),
}

enum Below {
    Nothing,
    Workflows,
    Settings,
    Events,
}

fn parse(path: &str) -> Option<(Reference, Below)> {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let owned = |text: &str| text.to_owned();
    let below = |segment: &str| match segment {
        WORKFLOWS => Some(Below::Workflows),
        SETTINGS => Some(Below::Settings),
        EVENTS => Some(Below::Events),
        _ => None,
    };
    match segments.as_slice() {
        [id] => Some((Reference::Id(owned(id)), Below::Nothing)),
        [id, segment] if id.parse::<ProjectId>().is_ok() && below(segment).is_some() => {
            Some((Reference::Id(owned(id)), below(segment)?))
        }
        [organisation, project] => Some((
            Reference::Slugs(owned(organisation), owned(project)),
            Below::Nothing,
        )),
        [organisation, project, segment] => Some((
            Reference::Slugs(owned(organisation), owned(project)),
            below(segment)?,
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
    pub(crate) async fn resolve<C: ConnectionTrait, K: Key>(
        &self,
        db: &C,
        caller: &Caller<OnProject<K>>,
    ) -> Result<Project, ApiError> {
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
        let missing = || ApiError::new(ErrorCode::NotFound, missing(&name));
        let project = found.map_err(database)?.ok_or_else(missing)?;
        caller.project(&project, missing)?;
        Ok(project)
    }
}

pub(crate) async fn find<C: ConnectionTrait>(
    db: &C,
    reference: &str,
) -> Result<Option<Project>, ApiError> {
    if let Ok(id) = reference.parse::<ProjectId>() {
        return bosun_db::projects::find(db, id).await.map_err(database);
    }
    match reference.split_once('/') {
        Some((organisation, project)) => {
            bosun_db::projects::find_by_slugs(db, organisation, project)
                .await
                .map_err(database)
        }
        None => Ok(None),
    }
}

pub(crate) fn missing(reference: &str) -> String {
    format!("there is no project `{reference}`")
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<Listed<ProjectView>>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<Project>>, ApiError> {
    let listing = listing?;
    let projects = bosun_db::projects::list(
        &state.db,
        &caller.reach(),
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(projects, |project| project.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<ProjectManage>>,
    body: Idempotent<CreateProjectRequest>,
) -> Result<Created<Project>, ApiError> {
    let request = &body.value;
    let absent = || {
        let mut faults = Faults::default();
        faults.add(
            "organisation",
            organisations::missing(&request.organisation),
        );
        faults.refuse("the project is not valid")
    };
    let organisation = organisations::find(&state.db, &request.organisation)
        .await?
        .ok_or_else(absent)?;
    caller.organisation(&organisation, absent)?;
    let mut faults = Faults::default();
    slug::check_slug(&mut faults, "slug", "a slug", &request.slug);
    slug::check_name(&mut faults, "name", &request.name);
    let repository = match &request.repository {
        Some(full_name) => visible(&state.db, &organisation, full_name, &mut faults).await?,
        None => None,
    };
    faults.finish("the project is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let mut project =
        bosun_db::projects::create(&transaction, organisation.id, &request.slug, &request.name)
            .await
            .map_err(database)?;
    if let Some(repository) = &repository {
        project = bosun_db::projects::bind(&transaction, project.id, Some(repository))
            .await
            .map_err(database)?
            .ok_or_else(|| failed("a project vanished while it was created"))?;
    }
    idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), project).await
}

async fn update(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectManage>>,
    project: ProjectPath,
    body: Result<Body<UpdateProjectRequest>, ApiError>,
) -> Result<Json<Project>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let Body(UpdateProjectRequest { repository }) = body?;
    let Some(repository) = repository else {
        return Ok(Json(project));
    };
    let repository = match repository {
        Some(full_name) => {
            let organisation = bosun_db::organisations::find(&state.db, project.organisation_id)
                .await
                .map_err(database)?
                .ok_or_else(|| failed("the organisation of a project vanished"))?;
            let mut faults = Faults::default();
            let found = visible(&state.db, &organisation, &full_name, &mut faults).await?;
            faults.finish("the project is not valid")?;
            found
        }
        None => None,
    };
    bosun_db::projects::bind(&state.db, project.id, repository.as_ref())
        .await
        .map_err(database)?
        .map(Json)
        .ok_or_else(|| failed("a project vanished while it was changed"))
}

async fn visible<C: ConnectionTrait>(
    db: &C,
    organisation: &Organisation,
    full_name: &str,
    faults: &mut Faults,
) -> Result<Option<Repository>, ApiError> {
    let Some((owner, name)) = full_name
        .split_once('/')
        .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
    else {
        faults.add(
            "repository",
            format!("`{full_name}` is not the full name of a repository, `<owner>/<name>`"),
        );
        return Ok(None);
    };
    let visibility = bosun_db::installations::visibility(db, organisation.id, owner, name)
        .await
        .map_err(database)?;
    match visibility {
        Visibility::Visible(repository) => return Ok(Some(repository)),
        Visibility::NoInstallation => faults.add(
            "repository",
            format!(
                "the organisation `{}` has no installation of the GitHub App, so its projects \
                 bind no repository",
                organisation.slug
            ),
        ),
        Visibility::Hidden { account } => faults.add(
            "repository",
            format!(
                "the installation of the GitHub App on `{account}` cannot see the repository \
                 `{full_name}`"
            ),
        ),
    }
    Ok(None)
}

async fn show(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    project: ProjectPath,
) -> Result<Json<Project>, ApiError> {
    project.resolve(&state.db, &caller).await.map(Json)
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
        Some((_, Below::Settings)) => settings::show.call(request, state).await,
        Some((_, Below::Events)) | None => envelope::absent(&method, &uri).into_response(),
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
        Some((_, Below::Events)) => forge_events::receive.call(request, state).await,
        _ => envelope::absent(&method, &uri).into_response(),
    }
}

async fn change(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    Segment(path): Segment,
    request: Request,
) -> Response {
    match parse(&path) {
        Some((_, Below::Nothing)) => update.call(request, state).await,
        Some((_, Below::Settings)) => settings::change.call(request, state).await,
        _ => envelope::absent(&method, &uri).into_response(),
    }
}
