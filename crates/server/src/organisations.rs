use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use bosun_types::api::{CreateOrganisationRequest, Organisation};
use bosun_types::error::ErrorCode;
use bosun_types::id::OrganisationId;
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::AppState;
use crate::envelope::ApiError;
use crate::idempotency;
use crate::request::{Body, Faults, Segment, database};
use crate::slug;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/organisations", post(create))
        .route("/api/v1/organisations/{organisation}", get(show))
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
    body: Body<CreateOrganisationRequest>,
) -> Result<Response, ApiError> {
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
        StatusCode::CREATED,
        &organisation,
    )
    .await
}

async fn show(
    State(state): State<AppState>,
    Segment(reference): Segment,
) -> Result<Json<Organisation>, ApiError> {
    find(&state.db, &reference)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, missing(&reference)))
}
