use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use bosun_db::tokens::{NewToken, Scope};
use bosun_types::api::{CreatePersonalTokenRequest, Page, PersonalToken, PersonalTokenCreated};
use bosun_types::error::ErrorCode;
use bosun_types::id::TokenId;
use bosun_types::permission::Permission;
use chrono::TimeDelta;
use tracing::info;

use crate::access::{Authenticated, Caller};
use crate::auth::Credential;
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::reply::{Created, Removed};
use crate::request::{Body, Faults, Listing, Segment, database, failed};
use crate::token::{PERSONAL, Token};
use crate::{AppState, organisations, projects, roles, slug};

const LONGEST_LIFETIME: TimeDelta = TimeDelta::days(365);

const REFUSAL: &str = "the personal token is not valid";

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/tokens", list)
        .post("/api/v1/tokens", create)
        .delete("/api/v1/tokens/{token}", revoke)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<Authenticated>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<PersonalToken>>, ApiError> {
    let listing = listing?;
    let tokens = bosun_db::tokens::list(
        &state.db,
        caller.principal.user.id,
        listing.cursor(),
        listing.fetch(),
    )
    .await
    .map_err(database)?;
    Ok(Json(listing.page(tokens, |token| token.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<Authenticated>,
    body: Result<Body<CreatePersonalTokenRequest>, ApiError>,
) -> Result<Created<PersonalTokenCreated>, ApiError> {
    let Body(request) = body?;
    let principal = &caller.principal;
    let held = &principal.permissions;
    let login = &principal.user.login;
    let now = state.clock.now();
    let mut faults = Faults::default();
    slug::check_name(&mut faults, "name", &request.name);
    if request.permissions.is_empty() {
        faults.add(
            "permissions",
            "names no permission, and a token carries at least one",
        );
    }
    let keys = roles::permission_keys(&mut faults, "permissions", &request.permissions);

    let restricted = !request.organisations.is_empty() || !request.projects.is_empty();
    let mut unreached = false;
    let mut scopes = Vec::new();
    let mut reach: BTreeSet<Permission> = BTreeSet::new();
    for (index, reference) in request.organisations.iter().enumerate() {
        let found = organisations::find(&state.db, reference)
            .await?
            .map(|organisation| (held.on_organisation(organisation.id), organisation))
            .filter(|(there, _)| !there.is_empty());
        match found {
            Some((there, organisation)) => {
                reach.extend(there);
                scopes.push(Scope {
                    organisation_id: organisation.id,
                    project_id: None,
                });
            }
            None => {
                unreached = true;
                faults.add(
                    format!("organisations.{index}"),
                    organisations::missing(reference),
                );
            }
        }
    }
    for (index, reference) in request.projects.iter().enumerate() {
        let found = projects::find(&state.db, reference)
            .await?
            .map(|project| {
                (
                    held.on_project(project.organisation_id, project.id),
                    project,
                )
            })
            .filter(|(there, _)| !there.is_empty());
        match found {
            Some((there, project)) => {
                reach.extend(there);
                scopes.push(Scope {
                    organisation_id: project.organisation_id,
                    project_id: Some(project.id),
                });
            }
            None => {
                unreached = true;
                faults.add(format!("projects.{index}"), projects::missing(reference));
            }
        }
    }
    if !restricted {
        reach.extend(&held.installation);
        reach.extend(held.organisations.values().flatten());
        reach.extend(held.projects.values().flatten());
    }
    for (index, key) in request.permissions.iter().enumerate() {
        let Some(permission) = Permission::from_key(key) else {
            continue;
        };
        if !unreached && !reach.contains(&permission) {
            let place = if restricted {
                "on any organisation or project the token is restricted to"
            } else {
                "anywhere"
            };
            faults.add(
                format!("permissions.{index}"),
                format!(
                    "`{login}` does not hold `{permission}` {place}, and a token carries only \
                     what its creator holds"
                ),
            );
        }
    }
    let parent = match &principal.credential {
        Credential::Token {
            expires_at,
            restricted,
            ..
        } => Some((*expires_at, *restricted)),
        Credential::Session { .. } => None,
    };
    if parent.is_some_and(|(_, within)| within) && !restricted {
        faults.add(
            "organisations",
            "names no organisation and no project, and a token restricted to organisations or \
             projects makes only tokens restricted to some of them",
        );
    }
    if request.expires_at <= now {
        faults.add("expires_at", "lies in the past");
    } else if request.expires_at > now + LONGEST_LIFETIME {
        faults.add(
            "expires_at",
            "lies more than a year ahead, and a personal token lives a year at most",
        );
    } else if let Some((expires_at, _)) = parent
        && request.expires_at > expires_at
    {
        faults.add(
            "expires_at",
            format!(
                "lies after {}, when the token of this request expires, and a token makes only \
                 tokens that expire no later than itself",
                expires_at.to_rfc3339()
            ),
        );
    }
    faults.finish(REFUSAL)?;

    let token = Token::generate(PERSONAL).map_err(failed)?;
    let personal = bosun_db::tokens::create_personal(
        &state.db,
        principal.user.id,
        NewToken {
            name: request.name,
            sha256: token.sha256(),
            expires_at: request.expires_at,
        },
        &keys,
        restricted.then_some(scopes.as_slice()),
    )
    .await
    .map_err(database)?;
    info!(
        token_id = %personal.id,
        %login,
        restricted,
        expires_at = %personal.expires_at,
        "created a personal token"
    );
    Ok(Created::New(PersonalTokenCreated {
        personal,
        token: token.into_string(),
    }))
}

async fn revoke(
    State(state): State<AppState>,
    caller: Caller<Authenticated>,
    Segment(reference): Segment,
) -> Result<Removed, ApiError> {
    let absent = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no token `{reference}`"),
        )
    };
    let id: TokenId = reference.parse().map_err(|_| absent())?;
    let user = &caller.principal.user;
    if !bosun_db::tokens::revoke(&state.db, user.id, id, state.clock.now())
        .await
        .map_err(database)?
    {
        return Err(absent());
    }
    info!(token_id = %id, login = user.login, "revoked a personal token");
    Ok(Removed)
}
