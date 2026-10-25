use axum::Json;
use axum::extract::State;
use bosun_types::api::{AddMemberRequest, CreateTeamRequest, Page, Team, User};
use bosun_types::error::ErrorCode;
use bosun_types::id::{TeamId, UserId};
use sea_orm::{ConnectionTrait, TransactionTrait};
use tracing::info;

use crate::access::{Caller, Key, OnOrganisation, OrganisationManage};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::organisations::{self, InOrganisation};
use crate::reply::{Created, Removed, Saved};
use crate::request::{Body, Faults, Idempotent, Listing, Pair, Segment, database};
use crate::{AppState, idempotency, slug, users};

const SLUG_REASON: &str = "a team's slug may not, since a role binding names a team by either";

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/teams", list)
        .post("/api/v1/teams", create)
        .delete("/api/v1/teams/{team}", delete)
        .get("/api/v1/teams/{team}/members", members)
        .post("/api/v1/teams/{team}/members", add_member)
        .delete("/api/v1/teams/{team}/members/{user}", remove_member)
}

fn missing(reference: &str) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("there is no team `{reference}`"),
    )
}

async fn checked<C: ConnectionTrait, K: Key>(
    db: &C,
    caller: &Caller<OnOrganisation<K>>,
    reference: &str,
) -> Result<Team, ApiError> {
    let absent = || missing(reference);
    let id: TeamId = reference.parse().map_err(|_| absent())?;
    let team = bosun_db::teams::find(db, id)
        .await
        .map_err(database)?
        .ok_or_else(absent)?;
    organisations::of(db, caller, team.organisation_id, absent).await?;
    Ok(team)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    listing: Result<Listing<InOrganisation>, ApiError>,
) -> Result<Json<Page<Team>>, ApiError> {
    let listing = listing?;
    let organisation = organisations::named(
        &state.db,
        &caller,
        listing.required(organisations::ORGANISATION)?,
        "the query is not valid",
    )
    .await?;
    let teams = bosun_db::teams::list(
        &state.db,
        organisation.id,
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(teams, |team| team.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    body: Idempotent<CreateTeamRequest>,
) -> Result<Created<Team>, ApiError> {
    let request = &body.value;
    let organisation = organisations::named(
        &state.db,
        &caller,
        &request.organisation,
        "the team is not valid",
    )
    .await?;
    let mut faults = Faults::default();
    slug::check_unlike_id(&mut faults, "slug", "a slug", &request.slug, SLUG_REASON);
    slug::check_name(&mut faults, "name", &request.name);
    faults.finish("the team is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let team = bosun_db::teams::create(&transaction, organisation.id, &request.slug, &request.name)
        .await
        .map_err(database)?;
    let id = team.id;
    let created =
        idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), team).await?;
    info!(team_id = %id, slug = request.slug, organisation = organisation.slug, "created a team");
    Ok(created)
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Segment(reference): Segment,
) -> Result<Removed, ApiError> {
    let team = checked(&state.db, &caller, &reference).await?;
    if !bosun_db::teams::delete(&state.db, team.id)
        .await
        .map_err(database)?
    {
        return Err(missing(&reference));
    }
    info!(team_id = %team.id, slug = team.slug, "deleted a team");
    Ok(Removed)
}

async fn members(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Segment(reference): Segment,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<User>>, ApiError> {
    let team = checked(&state.db, &caller, &reference).await?;
    let listing = listing?;
    let members = bosun_db::teams::members(&state.db, team.id, listing.cursor(), listing.fetch())
        .await
        .map_err(database)?;
    Ok(Json(listing.page(members, |user| user.id.as_uuid())))
}

async fn add_member(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Segment(reference): Segment,
    body: Result<Body<AddMemberRequest>, ApiError>,
) -> Result<Saved<User>, ApiError> {
    let team = checked(&state.db, &caller, &reference).await?;
    let Body(request) = body?;
    let user = users::find(&state.db, &request.user)
        .await?
        .ok_or_else(|| {
            let mut faults = Faults::default();
            faults.add("user", users::missing(&request.user));
            faults.refuse("the member is not valid")
        })?;
    let added = bosun_db::teams::add_member(&state.db, team.id, user.id)
        .await
        .map_err(database)?;
    if !added {
        return Ok(Saved::Again(user));
    }
    info!(team_id = %team.id, login = user.login, "added a member to a team");
    Ok(Saved::New(user))
}

async fn remove_member(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Pair(reference, member): Pair,
) -> Result<Removed, ApiError> {
    let team = checked(&state.db, &caller, &reference).await?;
    let absent = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no member `{member}` in team `{}`", team.slug),
        )
    };
    let user: UserId = member.parse().map_err(|_| absent())?;
    if !bosun_db::teams::remove_member(&state.db, team.id, user)
        .await
        .map_err(database)?
    {
        return Err(absent());
    }
    info!(team_id = %team.id, user_id = %user, "removed a member from a team");
    Ok(Removed)
}
