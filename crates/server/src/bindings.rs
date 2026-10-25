use axum::Json;
use axum::extract::State;
use bosun_db::roles::{NewBinding, Subject};
use bosun_types::api::{CreateRoleBindingRequest, Organisation, Page, Role, RoleBinding, Team};
use bosun_types::error::ErrorCode;
use bosun_types::id::{RoleBindingId, RoleId, TeamId};
use sea_orm::{ConnectionTrait, TransactionTrait};
use tracing::info;

use crate::access::{Caller, OnOrganisation, OrganisationManage};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::organisations::{self, InOrganisation};
use crate::reply::{Created, Removed};
use crate::request::{Faults, Idempotent, Listing, Segment, database, failed};
use crate::{AppState, idempotency, projects, users};

const REFUSAL: &str = "the role binding is not valid";

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/role-bindings", list)
        .post("/api/v1/role-bindings", create)
        .delete("/api/v1/role-bindings/{binding}", delete)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    listing: Result<Listing<InOrganisation>, ApiError>,
) -> Result<Json<Page<RoleBinding>>, ApiError> {
    let listing = listing?;
    let organisation = organisations::named(
        &state.db,
        &caller,
        listing.required(organisations::ORGANISATION)?,
        "the query is not valid",
    )
    .await?;
    let bindings = bosun_db::roles::bindings(
        &state.db,
        organisation.id,
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(bindings, |binding| binding.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    body: Idempotent<CreateRoleBindingRequest>,
) -> Result<Created<RoleBinding>, ApiError> {
    let request = &body.value;
    let organisation =
        organisations::named(&state.db, &caller, &request.organisation, REFUSAL).await?;
    let mut faults = Faults::default();
    let role = role(&state.db, &organisation, &request.role).await?;
    if role.is_none() {
        faults.add(
            "role",
            format!(
                "there is no role `{}` in organisation `{}`",
                request.role, organisation.slug
            ),
        );
    }
    let subject = match (&request.user, &request.team) {
        (Some(login), None) => {
            let user = users::find(&state.db, login).await?;
            if user.is_none() {
                faults.add("user", users::missing(login));
            }
            user.map(|user| Subject::User(user.id))
        }
        (None, Some(slug)) => {
            let team = team(&state.db, &organisation, slug).await?;
            if team.is_none() {
                faults.add(
                    "team",
                    format!(
                        "there is no team `{slug}` in organisation `{}`",
                        organisation.slug
                    ),
                );
            }
            team.map(|team| Subject::Team(team.id))
        }
        (Some(login), Some(_)) => {
            faults.add(
                "team",
                format!(
                    "names a team next to the user `{login}`, and a binding is for a user or for \
                     a team"
                ),
            );
            None
        }
        (None, None) => {
            faults.add(
                "user",
                "names neither a user nor a team, and a binding is for one of them",
            );
            None
        }
    };
    let project = match &request.project {
        Some(reference) => {
            let project = projects::find(&state.db, reference)
                .await?
                .filter(|project| project.organisation_id == organisation.id);
            if project.is_none() {
                faults.add(
                    "project",
                    format!(
                        "there is no project `{reference}` in organisation `{}`",
                        organisation.slug
                    ),
                );
            }
            project.map(|project| project.id)
        }
        None => None,
    };
    faults.finish(REFUSAL)?;
    let (Some(role), Some(subject)) = (role, subject) else {
        return Err(failed(
            "a role binding passed its checks without a role or a subject",
        ));
    };

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let binding = bosun_db::roles::bind(
        &transaction,
        NewBinding {
            role_id: role.id,
            subject,
            organisation_id: organisation.id,
            project_id: project,
        },
    )
    .await
    .map_err(database)?;
    let id = binding.id;
    let created =
        idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), binding).await?;
    info!(
        binding_id = %id,
        role = role.name,
        organisation = organisation.slug,
        "bound a role"
    );
    Ok(created)
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Segment(reference): Segment,
) -> Result<Removed, ApiError> {
    let absent = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no role binding `{reference}`"),
        )
    };
    let id: RoleBindingId = reference.parse().map_err(|_| absent())?;
    let binding = bosun_db::roles::find_binding(&state.db, id)
        .await
        .map_err(database)?
        .ok_or_else(absent)?;
    organisations::of(&state.db, &caller, binding.organisation_id, absent).await?;
    if !bosun_db::roles::unbind(&state.db, id)
        .await
        .map_err(database)?
    {
        return Err(absent());
    }
    info!(binding_id = %id, role = binding.role.name, "removed a role binding");
    Ok(Removed)
}

async fn role<C: ConnectionTrait>(
    db: &C,
    organisation: &Organisation,
    reference: &str,
) -> Result<Option<Role>, ApiError> {
    match reference.parse::<RoleId>() {
        Ok(id) => Ok(bosun_db::roles::find(db, id)
            .await
            .map_err(database)?
            .filter(|role| {
                role.organisation_id
                    .is_none_or(|owner| owner == organisation.id)
            })),
        Err(_) => bosun_db::roles::named(db, organisation.id, reference)
            .await
            .map_err(database),
    }
}

async fn team<C: ConnectionTrait>(
    db: &C,
    organisation: &Organisation,
    reference: &str,
) -> Result<Option<Team>, ApiError> {
    match reference.parse::<TeamId>() {
        Ok(id) => Ok(bosun_db::teams::find(db, id)
            .await
            .map_err(database)?
            .filter(|team| team.organisation_id == organisation.id)),
        Err(_) => bosun_db::teams::find_by_slug(db, organisation.id, reference)
            .await
            .map_err(database),
    }
}
