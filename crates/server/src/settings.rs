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
use crate::request::{Changes, Faults, IfMatch, database};
use crate::site::Retention;

const WHAT: &str = "the settings";

const REFUSED: &str = "the settings are not valid";

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
    let changed = changes?.apply(&stored.settings, "setting", REFUSED)?;
    bounded(&changed, &stored.settings, state.site.retention())?;
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

fn bounded(
    changed: &ProjectSettings,
    stored: &ProjectSettings,
    retention: Retention,
) -> Result<(), ApiError> {
    let mut faults = Faults::default();
    if changed.log_retention != stored.log_retention && changed.log_retention > retention.max_logs {
        faults.add(
            "log_retention",
            format!(
                "`log_retention` is {} days, and this installation keeps logs and step summaries \
                 for at most {} days",
                changed.log_retention, retention.max_logs
            ),
        );
    }
    if changed.run_retention != stored.run_retention
        && let (Some(days), Some(max)) = (changed.run_retention, retention.max_runs)
        && days > max
    {
        faults.add(
            "run_retention",
            format!(
                "`run_retention` is {days} days, and this installation keeps runs for at most \
                 {max} days"
            ),
        );
    }
    faults.finish(REFUSED)
}

fn tagged(stored: Settings) -> Tagged<ProjectSettings> {
    Tagged {
        version: stored.version,
        value: stored.settings,
    }
}
