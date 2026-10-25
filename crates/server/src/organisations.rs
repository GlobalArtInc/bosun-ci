use axum::Json;
use axum::extract::State;
use bosun_db::installations::Bound;
use bosun_types::api::{CreateOrganisationRequest, Organisation, UpdateOrganisationRequest};
use bosun_types::error::ErrorCode;
use bosun_types::id::OrganisationId;
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::AppState;
use crate::access::{Caller, OnInstallation, OnOrganisation, OrganisationManage, ProjectView};
use crate::envelope::ApiError;
use crate::idempotency;
use crate::openapi::Api;
use crate::reply::Created;
use crate::request::{Body, Faults, Idempotent, Segment, database};
use crate::slug;

pub(crate) fn routes() -> Api {
    Api::new()
        .post("/api/v1/organisations", create)
        .get("/api/v1/organisations/{organisation}", show)
        .patch("/api/v1/organisations/{organisation}", update)
}

pub(crate) async fn find<C: ConnectionTrait>(
    db: &C,
    reference: &str,
) -> Result<Option<Organisation>, ApiError> {
    match reference.parse::<OrganisationId>() {
        Ok(id) => bosun_db::organisations::find(db, id).await,
        Err(_) => bosun_db::organisations::find_by_slug(db, reference).await,
    }
    .map_err(database)
}

pub(crate) fn missing(reference: &str) -> String {
    format!("there is no organisation `{reference}`")
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<OrganisationManage>>,
    body: Result<Idempotent<CreateOrganisationRequest>, ApiError>,
) -> Result<Created<Organisation>, ApiError> {
    caller.installation()?;
    let body = body?;
    let request = &body.value;
    let mut faults = Faults::default();
    if slug::check_slug(&mut faults, "slug", "a slug", &request.slug)
        && slug::reads_as_id(&request.slug)
    {
        faults.add(
            "slug",
            format!(
                "`{}` reads as an id, and an organisation's slug may not, since a path names an \
                 organisation by either",
                request.slug
            ),
        );
    }
    slug::check_name(&mut faults, "name", &request.name);
    faults.finish("the organisation is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let organisation = bosun_db::organisations::create(&transaction, &request.slug, &request.name)
        .await
        .map_err(database)?;
    idempotency::commit(
        &state.db,
        transaction,
        body.idempotency.as_ref(),
        organisation,
    )
    .await
}

async fn show(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<ProjectView>>,
    Segment(reference): Segment,
) -> Result<Json<Organisation>, ApiError> {
    let absent = || ApiError::new(ErrorCode::NotFound, missing(&reference));
    let organisation = find(&state.db, &reference).await?.ok_or_else(absent)?;
    caller.organisation(&organisation, absent)?;
    Ok(Json(organisation))
}

async fn update(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<OrganisationManage>>,
    Segment(reference): Segment,
    body: Result<Body<UpdateOrganisationRequest>, ApiError>,
) -> Result<Json<Organisation>, ApiError> {
    caller.installation()?;
    let absent = || ApiError::new(ErrorCode::NotFound, missing(&reference));
    let organisation = find(&state.db, &reference).await?.ok_or_else(absent)?;
    let Body(UpdateOrganisationRequest { installation }) = body?;
    let Some(installation) = installation else {
        return Ok(Json(organisation));
    };
    let transaction = state.db.begin().await.map_err(database)?;
    let bound = bosun_db::installations::bind(&transaction, organisation.id, installation)
        .await
        .map_err(database)?;
    match bound {
        Bound::Bound => {}
        Bound::Unknown(installation) => {
            let mut faults = Faults::default();
            faults.add(
                "installation",
                format!(
                    "there is no installation `{installation}` of the GitHub App; Bosun learns of \
                     an installation from its webhooks"
                ),
            );
            return Err(faults.refuse("the organisation is not valid"));
        }
        Bound::Taken {
            installation,
            organisation: other,
        } => {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!(
                    "the installation `{installation}` of the GitHub App belongs to the \
                     organisation `{other}`"
                ),
            ));
        }
    }
    let organisation = bosun_db::organisations::find(&transaction, organisation.id)
        .await
        .map_err(database)?
        .ok_or_else(absent)?;
    transaction.commit().await.map_err(database)?;
    Ok(Json(organisation))
}
