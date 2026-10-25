use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bosun_types::actor::Actor;
use bosun_types::api::User;
use bosun_types::error::ErrorCode;
use bosun_types::id::{SessionId, TokenId};
use tracing::{error, warn};

use crate::AppState;
use crate::envelope::ApiError;
use crate::sessions::{self, IDLE, LIFETIME, TOUCH};
use crate::token::{PERSONAL, SESSION, Token};

const BEARER: &str = "Bearer";

pub(crate) const CSRF_HEADER: &str = "x-bosun-csrf";

#[derive(Clone)]
pub(crate) struct Principal {
    pub(crate) user: User,
    pub(crate) credential: Credential,
    pub(crate) password_change_required: bool,
}

#[derive(Clone)]
pub(crate) enum Credential {
    Token(TokenId),
    Session { id: SessionId, csrf: String },
}

impl Principal {
    pub(crate) fn actor(&self) -> Actor {
        match &self.credential {
            Credential::Token(id) => Actor::Token {
                id: *id,
                user_id: self.user.id,
                login: self.user.login.clone(),
            },
            Credential::Session { .. } => Actor::User {
                id: self.user.id,
                login: self.user.login.clone(),
            },
        }
    }

    pub(crate) fn session(&self) -> Option<SessionId> {
        match &self.credential {
            Credential::Token(_) => None,
            Credential::Session { id, .. } => Some(*id),
        }
    }

    pub(crate) fn csrf_token(&self) -> Option<String> {
        match &self.credential {
            Credential::Token(_) => None,
            Credential::Session { csrf, .. } => Some(csrf.clone()),
        }
    }
}

pub(crate) enum Refusal {
    Unauthenticated(&'static str),
    Forbidden(String),
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
            Refusal::Forbidden(message) => {
                ApiError::new(ErrorCode::Forbidden, message).into_response()
            }
            Refusal::Unchecked => {
                ApiError::new(ErrorCode::Internal, "the credentials could not be checked")
                    .into_response()
            }
        }
    }
}

pub(crate) async fn owner(state: State<AppState>, request: Request, next: Next) -> Response {
    guard(state, request, next, false).await
}

pub(crate) async fn interim(state: State<AppState>, request: Request, next: Next) -> Response {
    guard(state, request, next, true).await
}

async fn guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
    interim: bool,
) -> Response {
    let principal = match authenticate(&state, request.method(), request.headers()).await {
        Ok(principal) => principal,
        Err(refusal) => return refusal.into_response(),
    };
    if !interim && principal.password_change_required && principal.session().is_some() {
        return Refusal::Forbidden(format!(
            "`{}` must set a new password before anything else; POST /api/v1/auth/password \
             sets it",
            principal.user.login
        ))
        .into_response();
    }
    if !principal.user.owner {
        return Refusal::Forbidden(format!(
            "`{}` is not an owner of this installation, and only owners may act until roles \
             exist",
            principal.user.login
        ))
        .into_response();
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

async fn authenticate(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Principal, Refusal> {
    if let Some(header) = headers.get(AUTHORIZATION) {
        return bearer_principal(state, header).await;
    }
    match sessions::cookie(headers) {
        Some(cookie) => session_principal(state, method, headers, cookie).await,
        None => Err(Refusal::Unauthenticated(
            "the request carries neither a bearer token nor a session cookie",
        )),
    }
}

async fn bearer_principal(state: &AppState, header: &HeaderValue) -> Result<Principal, Refusal> {
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
    if found.expires_at <= state.clock.now() {
        return Err(Refusal::Unauthenticated("the bearer token expired"));
    }
    Ok(Principal {
        user: found.user,
        credential: Credential::Token(found.id),
        password_change_required: found.password_change_required,
    })
}

async fn session_principal(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    cookie: &str,
) -> Result<Principal, Refusal> {
    let token = Token::parse(SESSION, cookie).ok_or(Refusal::Unauthenticated(
        "the session cookie does not hold a well-formed session",
    ))?;
    let found = bosun_db::sessions::find(&state.db, &token.sha256())
        .await
        .map_err(|failure| {
            error!(error = %failure, "a session could not be looked up");
            Refusal::Unchecked
        })?
        .ok_or(Refusal::Unauthenticated(
            "the session is not known; it ended or never began",
        ))?;
    let now = state.clock.now();
    if found.created_at + LIFETIME <= now {
        return Err(Refusal::Unauthenticated(
            "the session ended 30 days after it began",
        ));
    }
    if found.last_seen_at + IDLE <= now {
        return Err(Refusal::Unauthenticated(
            "the session ended after 12 hours without a request",
        ));
    }
    let csrf = sessions::csrf(&token);
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        match headers.get(CSRF_HEADER) {
            None => {
                return Err(Refusal::Forbidden(
                    "a request that the session cookie authenticates and that changes \
                     something needs the header X-Bosun-CSRF with the token of GET /api/v1/me"
                        .to_owned(),
                ));
            }
            Some(given) if !sessions::same(given.as_bytes(), csrf.as_bytes()) => {
                return Err(Refusal::Forbidden(
                    "the header X-Bosun-CSRF does not hold the token of this session".to_owned(),
                ));
            }
            Some(_) => {}
        }
    }
    if now - found.last_seen_at >= TOUCH
        && let Err(failure) = bosun_db::sessions::touch(&state.db, found.id, now).await
    {
        warn!(error = %failure, session_id = %found.id, "the use of a session could not be recorded");
    }
    Ok(Principal {
        user: found.user,
        credential: Credential::Session { id: found.id, csrf },
        password_change_required: found.password_change_required,
    })
}

pub(crate) fn bearer(value: &str) -> Option<&str> {
    let (scheme, credentials) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case(BEARER)
        .then(|| credentials.trim_matches(' '))
}
