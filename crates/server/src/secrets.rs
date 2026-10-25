use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use bosun_db::secrets::{Scope, Stored};
use bosun_types::api::{Page, Secret, SetSecretRequest};
use bosun_types::error::ErrorCode;
use bosun_types::id::{OrganisationId, ProjectId};
use bosun_types::ir::SecretReferences;
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use tracing::info;

use crate::access::{Caller, OnScope, ProjectView, SecretManage};
use crate::envelope::ApiError;
use crate::keys::{Keyring, Unreadable};
use crate::openapi::Api;
use crate::organisations::ORGANISATION;
use crate::reply::{Removed, Saved};
use crate::request::{Body, Faults, Listing, Segment, database, failed};
use crate::scope::{self, InScope, PROJECT, Scoped};
use crate::{AppState, Error, names};

const BATCH: u64 = 100;

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/secrets", list)
        .post("/api/v1/secrets", set)
        .delete("/api/v1/secrets/{name}", delete)
}

pub(crate) fn binding(scope: &Scope, name: &str) -> Vec<u8> {
    let mut binding = Uuid::from(scope.organisation_id).as_bytes().to_vec();
    binding.extend_from_slice(scope.project_id.map_or(Uuid::nil(), Uuid::from).as_bytes());
    binding.extend_from_slice(name.as_bytes());
    binding
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnScope<ProjectView>>,
    listing: Result<Listing<InScope>, ApiError>,
) -> Result<Json<Page<Secret>>, ApiError> {
    let listing = listing?;
    let place = scope::resolve(
        &state.db,
        &caller,
        listing.filter(ORGANISATION),
        listing.filter(PROJECT),
    )
    .await?;
    let secrets =
        bosun_db::secrets::list(&state.db, place.scope, listing.cursor(), listing.fetch())
            .await
            .map_err(database)?;
    Ok(Json(listing.page(secrets, |secret| secret.id.as_uuid())))
}

async fn set(
    State(state): State<AppState>,
    caller: Caller<OnScope<SecretManage>>,
    scoped: Result<Scoped, ApiError>,
    body: Result<Body<SetSecretRequest>, ApiError>,
) -> Result<Saved<Secret>, ApiError> {
    let scoped = scoped?;
    let place = scope::resolve(&state.db, &caller, scoped.organisation(), scoped.project()).await?;
    let Body(request) = body?;
    let mut faults = Faults::default();
    let name = names::check_name(&mut faults, "name", &request.name);
    names::check_value(&mut faults, "value", &request.value);
    faults.finish("the secret is not valid")?;
    let name = name.ok_or_else(|| failed("a secret's name was refused without a fault"))?;
    let sealed = state
        .keys
        .seal(&binding(&place.scope, &name), request.value.as_bytes())
        .map_err(failed)?;
    let set = bosun_db::secrets::set(&state.db, place.scope, &name, &sealed)
        .await
        .map_err(database)?;
    info!(
        secret = name,
        scope = place.name,
        kek_id = sealed.kek_id,
        created = set.created,
        "set a secret"
    );
    Ok(if set.created {
        Saved::New(set.secret)
    } else {
        Saved::Again(set.secret)
    })
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnScope<SecretManage>>,
    Segment(name): Segment,
    scoped: Result<Scoped, ApiError>,
) -> Result<Removed, ApiError> {
    let scoped = scoped?;
    let place = scope::resolve(&state.db, &caller, scoped.organisation(), scoped.project()).await?;
    let stored = name.to_ascii_uppercase();
    if !bosun_db::secrets::delete(&state.db, place.scope, &stored)
        .await
        .map_err(database)?
    {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!("there is no secret `{stored}` in {}", place.name),
        ));
    }
    info!(secret = stored, scope = place.name, "deleted a secret");
    Ok(Removed)
}

pub(crate) struct Unopened {
    pub(crate) name: String,
    pub(crate) of_project: bool,
    pub(crate) reason: Unreadable,
}

pub(crate) async fn read<C: ConnectionTrait>(
    db: &C,
    keys: &Keyring,
    organisation: OrganisationId,
    project: ProjectId,
    references: &SecretReferences,
) -> Result<Result<BTreeMap<String, String>, Unopened>, bosun_db::Error> {
    let names = match references {
        SecretReferences::Named(names) if names.is_empty() => return Ok(Ok(BTreeMap::new())),
        SecretReferences::Named(names) => Some(names),
        SecretReferences::All => None,
    };
    let mut values = BTreeMap::new();
    for secret in bosun_db::secrets::in_scope(db, organisation, project, names).await? {
        match open(keys, &secret) {
            Ok(value) => {
                values.insert(secret.name, value);
            }
            Err(reason) => {
                return Ok(Err(Unopened {
                    name: secret.name,
                    of_project: secret.scope.project_id.is_some(),
                    reason,
                }));
            }
        }
    }
    Ok(Ok(values))
}

fn open(keys: &Keyring, secret: &Stored) -> Result<String, Unreadable> {
    let bytes = keys.open(&binding(&secret.scope, &secret.name), &secret.sealed)?;
    String::from_utf8(bytes).map_err(|_| Unreadable::Refused(secret.sealed.kek_id.clone()))
}

pub(crate) async fn rewrap(db: &DatabaseConnection, keys: &Keyring) -> Result<u64, Error> {
    let mut after = None;
    let mut rewrapped = 0;
    let mut failures = Vec::new();
    loop {
        let batch = bosun_db::secrets::under_another_kek(db, keys.current(), after, BATCH)
            .await
            .map_err(Error::Rewrap)?;
        let Some(last) = batch.last() else {
            break;
        };
        after = Some(last.id);
        for secret in batch {
            let described = format!("the secret `{}` ({})", secret.name, secret.id);
            match keys.rewrap(&binding(&secret.scope, &secret.name), &secret.sealed) {
                Ok(data_key) => {
                    if bosun_db::secrets::rewrap(db, &secret, data_key, keys.current())
                        .await
                        .map_err(Error::Rewrap)?
                    {
                        rewrapped += 1;
                    } else {
                        failures.push(format!(
                            "{described} changed while its data key was re-encrypted; run the \
                             command again"
                        ));
                    }
                }
                Err(reason) => failures.push(format!("{described}: {reason}")),
            }
        }
    }
    if failures.is_empty() {
        Ok(rewrapped)
    } else {
        Err(Error::Unrewrapped {
            rewrapped,
            failures,
        })
    }
}
