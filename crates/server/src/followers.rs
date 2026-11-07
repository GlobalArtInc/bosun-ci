use axum::Json;
use axum::extract::State;
use bosun_types::api::{Follower, Page, ProjectRef};
use bosun_types::error::ErrorCode;
use tracing::info;

use crate::AppState;
use crate::access::{Caller, OnProject, ProjectManage};
use crate::envelope::ApiError;
use crate::projects::{self, FollowerPath, ProjectPath};
use crate::reply::{Removed, Saved};
use crate::request::{Faults, Listing, database};

pub(crate) async fn list(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectManage>>,
    project: ProjectPath,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<Follower>>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let listing = listing?;
    let followers =
        bosun_db::triggers::followers(&state.db, project.id, listing.cursor(), listing.fetch())
            .await
            .map_err(database)?;
    let page = listing.page(followers, |follower| follower.id.as_uuid());
    Ok(Json(Page {
        items: page
            .items
            .into_iter()
            .map(|follower| {
                let visible = caller.views(follower.organisation_id, follower.id);
                Follower {
                    id: follower.id,
                    organisation_id: visible.then_some(follower.organisation_id),
                    path: visible.then_some(follower.path),
                }
            })
            .collect(),
        next: page.next,
    }))
}

pub(crate) async fn add(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectManage>>,
    project: ProjectPath,
    follower: FollowerPath,
) -> Result<Saved<ProjectRef>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let absent = || ApiError::new(ErrorCode::NotFound, projects::missing(&follower.name()));
    let follower = follower
        .find(&state.db)
        .await?
        .filter(|found| caller.views(found.organisation_id, found.id))
        .ok_or_else(absent)?;
    if follower.id == project.id {
        let mut faults = Faults::default();
        faults.add(
            "follower",
            "a project follows its own runs without being listed",
        );
        return Err(faults.refuse("the follower is not valid"));
    }
    let shown = ProjectRef {
        id: follower.id,
        organisation_id: follower.organisation_id,
        path: follower.path.clone(),
    };
    let added = match bosun_db::triggers::follow(&state.db, project.id, follower.id).await {
        Ok(added) => added,
        Err(bosun_db::Error::Vanished(_)) => return Err(absent()),
        Err(error) => return Err(database(error)),
    };
    if !added {
        return Ok(Saved::Again(shown));
    }
    info!(project_id = %project.id, follower_id = %follower.id, "a project may follow the runs of another");
    Ok(Saved::New(shown))
}

pub(crate) async fn remove(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectManage>>,
    project: ProjectPath,
    follower: FollowerPath,
) -> Result<Removed, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    if let Some(follower) = follower.find(&state.db).await?
        && bosun_db::triggers::unfollow(&state.db, project.id, follower.id)
            .await
            .map_err(database)?
    {
        info!(project_id = %project.id, follower_id = %follower.id, "a project may follow the runs of another no more");
    }
    Ok(Removed)
}
