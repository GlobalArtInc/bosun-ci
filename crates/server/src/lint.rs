use axum::Json;
use bosun_types::api::{LintRequest, LintResponse};

use crate::access::{Authenticated, Caller};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::request::Body;

pub(crate) fn routes() -> Api {
    Api::new().post("/api/v1/lint", lint)
}

async fn lint(
    _: Caller<Authenticated>,
    body: Result<Body<LintRequest>, ApiError>,
) -> Result<Json<LintResponse>, ApiError> {
    let Body(request) = body?;
    let file = request.path.unwrap_or_default();
    Ok(Json(LintResponse {
        diagnostics: bosun_workflow::lint(&file, &request.source),
    }))
}
