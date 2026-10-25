use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use bosun_types::api::{CreateRoleRequest, Page, Role, RoleDefinition};
use bosun_types::error::ErrorCode;
use bosun_types::id::RoleId;
use bosun_types::permission::Permission;
use sea_orm::{ConnectionTrait, TransactionTrait};
use tracing::info;

use crate::access::{Caller, OnOrganisation, OrganisationManage};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::organisations::{self, InOrganisation};
use crate::reply::{Created, Removed};
use crate::request::{Changes, Faults, Idempotent, Listing, Segment, database};
use crate::{AppState, idempotency, slug};

const NAME_REASON: &str = "a role's name may not, since a role binding names a role by either";

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/roles", list)
        .post("/api/v1/roles", create)
        .patch("/api/v1/roles/{role}", change)
        .delete("/api/v1/roles/{role}", delete)
}

pub(crate) fn permission_keys(
    faults: &mut Faults,
    field: &str,
    keys: &[String],
) -> BTreeSet<Permission> {
    let mut found = BTreeSet::new();
    for (index, key) in keys.iter().enumerate() {
        match Permission::from_key(key) {
            Some(permission) => {
                found.insert(permission);
            }
            None => faults.add(
                format!("{field}.{index}"),
                format!(
                    "`{key}` is not a permission key; the keys are those of the permission table"
                ),
            ),
        }
    }
    found
}

fn missing(reference: &str) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("there is no role `{reference}`"),
    )
}

async fn check_name<C: ConnectionTrait>(
    db: &C,
    faults: &mut Faults,
    name: &str,
) -> Result<(), ApiError> {
    slug::check_unlike_id(faults, "name", "a role name", name, NAME_REASON);
    if bosun_db::roles::builtin(db, name)
        .await
        .map_err(database)?
        .is_some()
    {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("the role `{name}` is built in, and a custom role takes another name"),
        ));
    }
    Ok(())
}

async fn custom<C: ConnectionTrait>(
    db: &C,
    caller: &Caller<OnOrganisation<OrganisationManage>>,
    reference: &str,
    action: &str,
) -> Result<Role, ApiError> {
    let absent = || missing(reference);
    let id: RoleId = reference.parse().map_err(|_| absent())?;
    let role = bosun_db::roles::find(db, id)
        .await
        .map_err(database)?
        .ok_or_else(absent)?;
    let Some(organisation) = role.organisation_id else {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "the role `{}` is built in, and only a custom role of an organisation can be {action}",
                role.name
            ),
        ));
    };
    organisations::of(db, caller, organisation, absent).await?;
    Ok(role)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    listing: Result<Listing<InOrganisation>, ApiError>,
) -> Result<Json<Page<Role>>, ApiError> {
    let listing = listing?;
    let organisation = organisations::named(
        &state.db,
        &caller,
        listing.required(organisations::ORGANISATION)?,
        "the query is not valid",
    )
    .await?;
    let roles = bosun_db::roles::list(
        &state.db,
        organisation.id,
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(roles, |role| role.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    body: Idempotent<CreateRoleRequest>,
) -> Result<Created<Role>, ApiError> {
    let request = &body.value;
    let organisation = organisations::named(
        &state.db,
        &caller,
        &request.organisation,
        "the role is not valid",
    )
    .await?;
    let mut faults = Faults::default();
    check_name(&state.db, &mut faults, &request.name).await?;
    let permissions = permission_keys(&mut faults, "permissions", &request.permissions);
    faults.finish("the role is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let role = bosun_db::roles::create(&transaction, organisation.id, &request.name, &permissions)
        .await
        .map_err(database)?;
    let id = role.id;
    let created =
        idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), role).await?;
    info!(role_id = %id, name = request.name, organisation = organisation.slug, "created a role");
    Ok(created)
}

async fn change(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Segment(reference): Segment,
    changes: Result<Changes<RoleDefinition>, ApiError>,
) -> Result<Json<Role>, ApiError> {
    let role = custom(&state.db, &caller, &reference, "changed").await?;
    let current = RoleDefinition {
        name: role.name.clone(),
        permissions: role
            .permissions
            .iter()
            .map(|permission| permission.key().to_owned())
            .collect(),
    };
    let changed = changes?.apply(&current, "field", "the role is not valid")?;
    let mut faults = Faults::default();
    if changed.name != role.name {
        check_name(&state.db, &mut faults, &changed.name).await?;
    }
    let permissions = permission_keys(&mut faults, "permissions", &changed.permissions);
    faults.finish("the role is not valid")?;
    if changed.name == role.name && permissions == role.permissions {
        return Ok(Json(role));
    }
    let saved = bosun_db::roles::change(&state.db, role.id, &changed.name, &permissions)
        .await
        .map_err(database)?
        .ok_or_else(|| missing(&reference))?;
    info!(role_id = %saved.id, name = saved.name, "changed a role");
    Ok(Json(saved))
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnOrganisation<OrganisationManage>>,
    Segment(reference): Segment,
) -> Result<Removed, ApiError> {
    let role = custom(&state.db, &caller, &reference, "deleted").await?;
    if !bosun_db::roles::delete(&state.db, role.id)
        .await
        .map_err(database)?
    {
        return Err(missing(&reference));
    }
    info!(role_id = %role.id, name = role.name, "deleted a role and its bindings");
    Ok(Removed)
}
