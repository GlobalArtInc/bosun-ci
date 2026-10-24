use axum::Json;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bosun_db::agents::{NewAgent, NewRegistrationToken, Registration};
use bosun_types::api::{
    Page, RegisterAgentRequest, RegisterAgentResponse, RegistrationTokenCreated,
    RegistrationTokenRequest,
};
use bosun_types::error::{ErrorBody, ErrorCode, FieldError};
use bosun_types::id::AgentId;
use chrono::{TimeDelta, Utc};
use serde::de::DeserializeOwned;
use tracing::{error, info};

use super::{LABEL_RULE, NAME_RULE, valid_label, valid_name};
use crate::AppState;
use crate::auth::Refusal;
use crate::envelope::ApiError;
use crate::token::{AGENT, REGISTRATION, Token};

const DEFAULT_LIFETIME: TimeDelta = TimeDelta::hours(24);

const LONGEST_LIFETIME: TimeDelta = TimeDelta::days(365);

const DEFAULT_LIMIT: u64 = 50;

const LARGEST_LIMIT: u64 = 500;

pub(crate) async fn register(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, Failure> {
    let request: RegisterAgentRequest = decode(&body, "an agent registration")?;
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
    Ok((
        StatusCode::CREATED,
        Json(RegisterAgentResponse {
            agent_id: agent.id,
            credential: credential.into_string(),
        }),
    )
        .into_response())
}

pub(crate) async fn create_token(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, Failure> {
    let request: RegistrationTokenRequest = decode(&body, "a registration token request")?;
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
    Ok((
        StatusCode::CREATED,
        Json(RegistrationTokenCreated {
            id: stored.id,
            pool_id: stored.pool_id,
            token: token.into_string(),
            reusable: stored.reusable,
            expires_at: stored.expires_at,
        }),
    )
        .into_response())
}

pub(crate) async fn list(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response, Failure> {
    let query = query.unwrap_or_default();
    let mut limit = DEFAULT_LIMIT;
    let mut cursor = None;
    let mut fields = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "limit" => match value.parse() {
                Ok(given) if (1..=LARGEST_LIMIT).contains(&given) => limit = given,
                _ => fields.push(field(
                    "limit",
                    &format!("is `{value}`, and a limit is a number from 1 to {LARGEST_LIMIT}"),
                )),
            },
            "cursor" => match value.parse::<AgentId>() {
                Ok(after) => cursor = Some(after),
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
    let mut items = bosun_db::agents::list(&state.db, cursor, limit + 1)
        .await
        .map_err(internal)?;
    let next = if items.len() as u64 > limit {
        items.truncate(limit as usize);
        items.last().map(|agent| agent.id.to_string())
    } else {
        None
    };
    Ok(Json(Page { items, next }).into_response())
}

pub(crate) enum Failure {
    Invalid(ApiError),
    Refused(Refusal),
    Internal,
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Failure::Invalid(error) => error.into_response(),
            Failure::Refused(refusal) => refusal.into_response(),
            Failure::Internal => {
                ApiError::new(ErrorCode::Internal, "the request could not be completed")
                    .into_response()
            }
        }
    }
}

fn decode<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T, Failure> {
    serde_json::from_slice(body).map_err(|failure| {
        Failure::Invalid(ApiError::new(
            ErrorCode::InvalidInput,
            format!("the body is not {what}: {failure}"),
        ))
    })
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
    Failure::Invalid(ApiError::from(body))
}

fn internal(failure: impl std::fmt::Display) -> Failure {
    error!(error = %failure, "a request about agents failed");
    Failure::Internal
}
