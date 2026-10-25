use axum::Json;
use axum::extract::{FromRequestParts, RawQuery, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use bosun_db::agents::{NewAgent, NewRegistrationToken, Registration};
use bosun_types::api::{
    Agent, Page, RegisterAgentRequest, RegisterAgentResponse, RegistrationTokenCreated,
    RegistrationTokenRequest, UpdateAgentRequest,
};
use bosun_types::error::{ErrorBody, ErrorCode, FieldError};
use bosun_types::id::AgentId;
use chrono::{TimeDelta, Utc};
use tracing::{error, info, warn};

use super::progress;
use super::{LABEL_RULE, NAME_RULE, valid_label, valid_name};
use crate::AppState;
use crate::access::{AgentManage, Caller, OnInstallation};
use crate::auth::Refusal;
use crate::envelope::ApiError;
use crate::openapi::{self, Input, Output, Spec};
use crate::reply::{Created, Deleted};
use crate::request::{Body, Segment};
use crate::token::{AGENT, REGISTRATION, Token};

const DEFAULT_LIFETIME: TimeDelta = TimeDelta::hours(24);

const LONGEST_LIFETIME: TimeDelta = TimeDelta::days(365);

const DEFAULT_LIMIT: u64 = 50;

const LARGEST_LIMIT: u64 = 500;

pub(crate) async fn register(
    State(state): State<AppState>,
    Body(request): Body<RegisterAgentRequest>,
) -> Result<Created<RegisterAgentResponse>, Failure> {
    let token = Token::parse(REGISTRATION, &request.token).ok_or(Failure::Refused(
        Refusal::Unauthenticated("the token is not a well-formed registration token"),
    ))?;
    let mut fields = Vec::new();
    if !valid_name(&request.name) {
        fields.push(field("name", NAME_RULE));
    }
    for (index, label) in request.labels.iter().enumerate() {
        if !valid_label(label) {
            fields.push(field(&format!("labels.{index}"), LABEL_RULE));
        }
    }
    if !fields.is_empty() {
        return Err(invalid("the agent registration is not valid", fields));
    }
    let credential = Token::generate(AGENT).map_err(internal)?;
    let mut labels: Vec<String> = Vec::new();
    for label in request.labels {
        let label = label.to_lowercase();
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let registration = bosun_db::agents::register(
        &state.db,
        &token.sha256(),
        Utc::now(),
        NewAgent {
            name: request.name,
            labels,
            credential_sha256: credential.sha256(),
        },
    )
    .await
    .map_err(internal)?;
    let agent = match registration {
        Registration::Registered(agent) => agent,
        Registration::UnknownToken => {
            return Err(Failure::Refused(Refusal::Unauthenticated(
                "the registration token is not known",
            )));
        }
        Registration::ExpiredToken => {
            return Err(Failure::Refused(Refusal::Unauthenticated(
                "the registration token expired",
            )));
        }
        Registration::UsedToken => {
            return Err(Failure::Refused(Refusal::Unauthenticated(
                "the registration token was used already, and it registers one agent only",
            )));
        }
    };
    info!(agent_id = %agent.id, name = agent.name, pool_id = %agent.pool_id, "registered an agent");
    Ok(Created::New(RegisterAgentResponse {
        agent_id: agent.id,
        credential: credential.into_string(),
    }))
}

pub(crate) async fn create_token(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    body: Result<Body<RegistrationTokenRequest>, ApiError>,
) -> Result<Created<RegistrationTokenCreated>, Failure> {
    caller.installation().map_err(Failure::Api)?;
    let Body(request) = body.map_err(Failure::Api)?;
    let now = Utc::now();
    let expires_at = request.expires_at.unwrap_or(now + DEFAULT_LIFETIME);
    let mut fields = Vec::new();
    if expires_at <= now {
        fields.push(field("expires_at", "lies in the past"));
    } else if expires_at > now + LONGEST_LIFETIME {
        fields.push(field(
            "expires_at",
            "lies more than a year ahead, and a registration token lives a year at most",
        ));
    }
    let pool = bosun_db::pools::named(&state.db, &request.pool)
        .await
        .map_err(internal)?;
    if pool.is_none() {
        fields.push(field(
            "pool",
            &format!("there is no pool `{}`", request.pool),
        ));
    }
    let (Some(pool), true) = (pool, fields.is_empty()) else {
        return Err(invalid(
            "the registration token request is not valid",
            fields,
        ));
    };
    let token = Token::generate(REGISTRATION).map_err(internal)?;
    let stored = bosun_db::agents::create_registration_token(
        &state.db,
        NewRegistrationToken {
            pool_id: pool.id,
            sha256: token.sha256(),
            reusable: request.reusable,
            expires_at,
        },
    )
    .await
    .map_err(internal)?;
    info!(
        registration_token_id = %stored.id,
        pool = pool.name,
        reusable = stored.reusable,
        expires_at = %stored.expires_at,
        "created a registration token"
    );
    Ok(Created::New(RegistrationTokenCreated {
        id: stored.id,
        pool_id: stored.pool_id,
        token: token.into_string(),
        reusable: stored.reusable,
        expires_at: stored.expires_at,
    }))
}

pub(crate) struct Paging {
    limit: u64,
    cursor: Option<AgentId>,
}

impl<S: Send + Sync> FromRequestParts<S> for Paging {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Paging, Failure> {
        let Ok(RawQuery(query)) = RawQuery::from_request_parts(parts, state).await;
        let query = query.unwrap_or_default();
        let mut paging = Paging {
            limit: DEFAULT_LIMIT,
            cursor: None,
        };
        let mut fields = Vec::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name {
                "limit" => match value.parse() {
                    Ok(given) if (1..=LARGEST_LIMIT).contains(&given) => paging.limit = given,
                    _ => fields.push(field(
                        "limit",
                        &format!("is `{value}`, and a limit is a number from 1 to {LARGEST_LIMIT}"),
                    )),
                },
                "cursor" => match value.parse::<AgentId>() {
                    Ok(after) => paging.cursor = Some(after),
                    Err(_) => fields.push(field(
                        "cursor",
                        &format!("is `{value}`, which is not the `next` of a page"),
                    )),
                },
                _ => {}
            }
        }
        if !fields.is_empty() {
            return Err(invalid("the query is not valid", fields));
        }
        Ok(paging)
    }
}

impl Input for Paging {
    fn input(spec: &mut Spec<'_>) {
        spec.reference(openapi::LIMIT);
        spec.reference(openapi::CURSOR);
    }
}

pub(crate) async fn list(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    paging: Result<Paging, Failure>,
) -> Result<Json<Page<Agent>>, Failure> {
    caller.installation().map_err(Failure::Api)?;
    let paging = paging?;
    let mut items = bosun_db::agents::list(&state.db, paging.cursor, paging.limit + 1)
        .await
        .map_err(internal)?;
    let next = if items.len() as u64 > paging.limit {
        items.truncate(paging.limit as usize);
        items.last().map(|agent| agent.id.to_string())
    } else {
        None
    };
    Ok(Json(Page { items, next }))
}

pub(crate) async fn update(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    Segment(reference): Segment,
    body: Result<Body<UpdateAgentRequest>, ApiError>,
) -> Result<Json<Agent>, Failure> {
    caller.installation().map_err(Failure::Api)?;
    let agent = named(&reference)?;
    let current = bosun_db::agents::find(&state.db, agent)
        .await
        .map_err(internal)?
        .ok_or_else(|| absent(&reference))?;
    let Body(UpdateAgentRequest { drain }) = body.map_err(Failure::Api)?;
    let Some(drain) = drain else {
        return Ok(Json(current));
    };
    let changed = bosun_db::agents::drain(&state.db, agent, drain)
        .await
        .map_err(internal)?
        .ok_or_else(|| absent(&reference))?;
    if changed.drain != current.drain {
        let login = &caller.principal.user.login;
        if drain {
            info!(agent_id = %agent, name = changed.name, login, "drained an agent");
        } else {
            info!(agent_id = %agent, name = changed.name, login, "lifted the drain of an agent");
        }
    }
    Ok(Json(changed))
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    Segment(reference): Segment,
) -> Result<Deleted, Failure> {
    caller.installation().map_err(Failure::Api)?;
    let agent = named(&reference)?;
    if !bosun_db::agents::delete(&state.db, agent)
        .await
        .map_err(internal)?
    {
        return Err(absent(&reference));
    }
    info!(
        agent_id = %agent,
        login = caller.principal.user.login,
        "deleted an agent and revoked its credential"
    );
    let actor = caller.principal.actor();
    let held = bosun_db::agents::held(&state.db, agent)
        .await
        .unwrap_or_else(|failure| {
            warn!(
                agent_id = %agent,
                error = %failure,
                "the executions of a deleted agent could not be read, so the sweep ends them"
            );
            Vec::new()
        });
    for execution_id in held {
        match progress::remove(&state.db, agent, execution_id, &actor, &state.site).await {
            Ok(true) => info!(
                agent_id = %agent,
                %execution_id,
                "ended an execution of a deleted agent"
            ),
            Ok(false) => {}
            Err(failure) => warn!(
                agent_id = %agent,
                %execution_id,
                error = %failure,
                "an execution of a deleted agent could not be ended, so the sweep ends it"
            ),
        }
    }
    if let Err(failure) = bosun_db::agents::announce(&state.db, agent).await {
        warn!(
            agent_id = %agent,
            error = %failure,
            "the replica holding a deleted agent could not be told, so its next heartbeat ends \
             its stream"
        );
    }
    Ok(Deleted)
}

fn named(reference: &str) -> Result<AgentId, Failure> {
    reference.parse().map_err(|_| absent(reference))
}

fn absent(reference: &str) -> Failure {
    Failure::Api(ApiError::new(
        ErrorCode::NotFound,
        format!("there is no agent `{reference}`"),
    ))
}

pub(crate) enum Failure {
    Api(ApiError),
    Refused(Refusal),
    Internal,
}

impl Output for Failure {
    fn output(spec: &mut Spec<'_>) {
        spec.error();
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Failure::Api(error) => error.into_response(),
            Failure::Refused(refusal) => refusal.into_response(),
            Failure::Internal => {
                ApiError::new(ErrorCode::Internal, "the request could not be completed")
                    .into_response()
            }
        }
    }
}

fn field(name: &str, message: &str) -> FieldError {
    FieldError {
        field: name.to_owned(),
        message: message.to_owned(),
    }
}

fn invalid(message: &str, fields: Vec<FieldError>) -> Failure {
    let mut body = ErrorBody::new(ErrorCode::InvalidInput, message);
    body.fields = fields;
    Failure::Api(ApiError::from(body))
}

fn internal(failure: impl std::fmt::Display) -> Failure {
    error!(error = %failure, "a request about agents failed");
    Failure::Internal
}
