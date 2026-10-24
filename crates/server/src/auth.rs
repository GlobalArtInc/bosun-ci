use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bosun_types::actor::Actor;
use bosun_types::api::User;
use bosun_types::error::ErrorCode;
use bosun_types::id::TokenId;
use chrono::Utc;
use tracing::error;

use crate::AppState;
use crate::envelope::ApiError;
use crate::token::{PERSONAL, Token};

const BEARER: &str = "Bearer";

#[derive(Clone)]
pub(crate) struct Principal {
    pub(crate) user: User,
    pub(crate) token: TokenId,
}

impl Principal {
    pub(crate) fn actor(&self) -> Actor {
        Actor::Token {
            id: self.token,
            user_id: self.user.id,
            login: self.user.login.clone(),
        }
    }
}

pub(crate) enum Refusal {
    Unauthenticated(&'static str),
    Unchecked,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Refusal::Unauthenticated(message) => {
                let mut response =
                    ApiError::new(ErrorCode::Unauthenticated, message).into_response();
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static(BEARER));
                response
            }
            Refusal::Unchecked => {
                ApiError::new(ErrorCode::Internal, "the bearer token could not be checked")
                    .into_response()
            }
        }
    }
}

pub(crate) async fn owner(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let principal = match authenticate(&state, request.headers()).await {
        Ok(principal) => principal,
        Err(refusal) => return refusal.into_response(),
    };
    if !principal.user.owner {
        return ApiError::new(
            ErrorCode::Forbidden,
            format!(
                "`{}` is not an owner of this installation, and only owners may act until roles \
                 exist",
                principal.user.login
            ),
        )
        .into_response();
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Principal, Refusal> {
    let header = headers.get(AUTHORIZATION).ok_or(Refusal::Unauthenticated(
        "the request carries no bearer token",
    ))?;
    let credentials = header
        .to_str()
        .ok()
        .and_then(bearer)
        .ok_or(Refusal::Unauthenticated(
            "the Authorization header does not hold a bearer token",
        ))?;
    let token = Token::parse(PERSONAL, credentials).ok_or(Refusal::Unauthenticated(
        "the bearer token is not a well-formed personal token",
    ))?;
    let found = bosun_db::tokens::find_personal(&state.db, &token.sha256())
        .await
        .map_err(|failure| {
            error!(error = %failure, "a personal token could not be looked up");
            Refusal::Unchecked
        })?
        .ok_or(Refusal::Unauthenticated("the bearer token is not known"))?;
    if found.revoked_at.is_some() {
        return Err(Refusal::Unauthenticated("the bearer token was revoked"));
    }
    if found.expires_at <= Utc::now() {
        return Err(Refusal::Unauthenticated("the bearer token expired"));
    }
    Ok(Principal {
        user: found.user,
        token: found.id,
    })
}

pub(crate) fn bearer(value: &str) -> Option<&str> {
    let (scheme, credentials) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case(BEARER)
        .then(|| credentials.trim_matches(' '))
}
