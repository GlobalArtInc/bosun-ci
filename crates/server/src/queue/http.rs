use axum::Json;
use axum::extract::State;
use bosun_types::api::{Page, QueuedJob};

use crate::AppState;
use crate::access::{Caller, Listed, ProjectView};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::request::{Listing, database};

pub(crate) fn routes() -> Api {
    Api::new().get("/api/v1/queue", list)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<Listed<ProjectView>>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<QueuedJob>>, ApiError> {
    let listing = listing?;
    let entries = bosun_db::views::queue(
        &state.db,
        &caller.reach(),
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(entries, |entry| entry.job.id.as_uuid())))
}
