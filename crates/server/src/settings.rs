use axum::extract::State;
use bosun_db::projects::Settings;
use bosun_types::api::{Project, ProjectSettings};
use bosun_types::error::ErrorCode;
use sea_orm::ConnectionTrait;

use crate::AppState;
use crate::access::{Caller, OnProject, ProjectManage, ProjectView};
use crate::envelope::ApiError;
use crate::projects::ProjectPath;
use crate::reply::{Tagged, etag};
use crate::request::{Changes, IfMatch, database};

const WHAT: &str = "the settings";

pub(crate) async fn show(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    project: ProjectPath,
) -> Result<Tagged<ProjectSettings>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let stored = stored(&state.db, &project).await?;
    Ok(tagged(stored))
}

pub(crate) async fn change(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectManage>>,
    project: ProjectPath,
    condition: IfMatch,
    changes: Result<Changes<ProjectSettings>, ApiError>,
) -> Result<Tagged<ProjectSettings>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let stored = stored(&state.db, &project).await?;
    condition.check(&etag(stored.version), WHAT)?;
    let changed = changes?.apply(&stored.settings, "setting", "the settings are not valid")?;
    if changed == stored.settings {
        return Ok(tagged(stored));
    }
    let saved =
        bosun_db::projects::change_settings(&state.db, project.id, stored.version, &changed)
            .await
            .map_err(database)?;
    saved.map(tagged).ok_or_else(|| {
        ApiError::new(
            ErrorCode::PreconditionFailed,
            format!(
                "{WHAT} changed while this change was saved: read the current version and repeat \
                 the change"
            ),
        )
    })
}

async fn stored<C: ConnectionTrait>(db: &C, project: &Project) -> Result<Settings, ApiError> {
    bosun_db::projects::settings(db, project.id)
        .await
        .map_err(database)?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("there is no project `{}`", project.id),
            )
        })
}

fn tagged(stored: Settings) -> Tagged<ProjectSettings> {
    Tagged {
        version: stored.version,
        value: stored.settings,
    }
}
