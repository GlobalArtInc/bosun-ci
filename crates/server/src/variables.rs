use axum::Json;
use axum::extract::State;
use bosun_types::api::{Page, SetVariableRequest, Variable};
use bosun_types::error::ErrorCode;
use tracing::info;

use crate::access::{Caller, OnScope, ProjectView, VariableManage};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::organisations::ORGANISATION;
use crate::reply::{Removed, Saved};
use crate::request::{Body, Faults, Listing, Segment, database, failed};
use crate::scope::{self, InScope, PROJECT, Scoped};
use crate::{AppState, names};

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/variables", list)
        .post("/api/v1/variables", set)
        .delete("/api/v1/variables/{name}", delete)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnScope<ProjectView>>,
    listing: Result<Listing<InScope>, ApiError>,
) -> Result<Json<Page<Variable>>, ApiError> {
    let listing = listing?;
    let place = scope::resolve(
        &state.db,
        &caller,
        listing.filter(ORGANISATION),
        listing.filter(PROJECT),
    )
    .await?;
    let variables =
        bosun_db::variables::list(&state.db, place.scope, listing.cursor(), listing.fetch())
            .await
            .map_err(database)?;
    Ok(Json(
        listing.page(variables, |variable| variable.id.as_uuid()),
    ))
}

async fn set(
    State(state): State<AppState>,
    caller: Caller<OnScope<VariableManage>>,
    scoped: Result<Scoped, ApiError>,
    body: Result<Body<SetVariableRequest>, ApiError>,
) -> Result<Saved<Variable>, ApiError> {
    let scoped = scoped?;
    let place = scope::resolve(&state.db, &caller, scoped.organisation(), scoped.project()).await?;
    let Body(request) = body?;
    let mut faults = Faults::default();
    let name = names::check_name(&mut faults, "name", &request.name);
    names::check_value(&mut faults, "value", &request.value);
    faults.finish("the variable is not valid")?;
    let name = name.ok_or_else(|| failed("a variable's name was refused without a fault"))?;
    let set = bosun_db::variables::set(&state.db, place.scope, &name, &request.value)
        .await
        .map_err(database)?;
    info!(
        variable = name,
        scope = place.name,
        created = set.created,
        "set a variable"
    );
    Ok(if set.created {
        Saved::New(set.variable)
    } else {
        Saved::Again(set.variable)
    })
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnScope<VariableManage>>,
    Segment(name): Segment,
    scoped: Result<Scoped, ApiError>,
) -> Result<Removed, ApiError> {
    let scoped = scoped?;
    let place = scope::resolve(&state.db, &caller, scoped.organisation(), scoped.project()).await?;
    let stored = name.to_ascii_uppercase();
    if !bosun_db::variables::delete(&state.db, place.scope, &stored)
        .await
        .map_err(database)?
    {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!("there is no variable `{stored}` in {}", place.name),
        ));
    }
    info!(variable = stored, scope = place.name, "deleted a variable");
    Ok(Removed)
}
