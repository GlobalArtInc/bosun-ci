use axum::Json;
use axum::extract::State;
use bosun_db::pools::{Access, NewPool};
use bosun_types::api::{CreatePoolRequest, Page, Pool};
use sea_orm::TransactionTrait;
use tracing::info;

use crate::access::{AgentManage, Caller, OnInstallation};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::reply::Created;
use crate::request::{Faults, Idempotent, Listing, database};
use crate::{AppState, idempotency, organisations, projects, slug};

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/pools", list)
        .post("/api/v1/pools", create)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<Pool>>, ApiError> {
    caller.installation()?;
    let listing = listing?;
    let pools = bosun_db::pools::list(&state.db, listing.cursor(), listing.fetch())
        .await
        .map_err(database)?;
    Ok(Json(listing.page(pools, |pool| pool.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    body: Result<Idempotent<CreatePoolRequest>, ApiError>,
) -> Result<Created<Pool>, ApiError> {
    caller.installation()?;
    let body = body?;
    let request = &body.value;
    let mut faults = Faults::default();
    slug::check_slug(&mut faults, "name", "a pool name", &request.name);
    let mut access = Vec::new();
    if request.access.all_projects {
        access.push(Access::AllProjects);
    }
    for (index, reference) in request.access.organisations.iter().enumerate() {
        match organisations::find(&state.db, reference).await? {
            Some(organisation) => access.push(Access::Organisation(organisation.id)),
            None => faults.add(
                format!("access.organisations.{index}"),
                organisations::missing(reference),
            ),
        }
    }
    for (index, reference) in request.access.projects.iter().enumerate() {
        match projects::find(&state.db, reference).await? {
            Some(project) => access.push(Access::Project(project.organisation_id, project.id)),
            None => faults.add(
                format!("access.projects.{index}"),
                projects::missing(reference),
            ),
        }
    }
    if !request.access.all_projects
        && request.access.organisations.is_empty()
        && request.access.projects.is_empty()
    {
        faults.add(
            "access",
            "opens the pool to no project; a pool is open to all projects, or to at least one \
             organisation or project",
        );
    }
    faults.finish("the pool is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let pool = bosun_db::pools::create(
        &transaction,
        NewPool {
            name: &request.name,
            trust: request.trust,
            access: &access,
        },
    )
    .await
    .map_err(database)?;
    let (id, trust) = (pool.id, pool.trust);
    let created =
        idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), pool).await?;
    info!(pool_id = %id, name = request.name, ?trust, "created a pool");
    Ok(created)
}
