use axum::extract::{FromRequestParts, Query, State};
use axum::http::header::{LOCATION, SET_COOKIE, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bosun_types::error::ErrorCode;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::info;

use super::admission::{self, Refusal};
use super::state::{self, COOKIE, LIFETIME, Started, Unread};
use super::token::Expected;
use super::{Failure, Provider, described};
use crate::envelope::ApiError;
use crate::openapi::{Api, Input, Output, Spec};
use crate::request::{Faults, Segment, database, failed, invalid};
use crate::{AppState, sessions};

const QUERY: [(&str, &str); 5] = [
    (
        "code",
        "the authorization code that the identity provider returns the browser with",
    ),
    (
        "state",
        "the state of the sign-in, which must be the state the browser's cookie holds",
    ),
    (
        "error",
        "the error code of an identity provider that refused the sign-in",
    ),
    (
        "error_description",
        "the description of the error of an identity provider that refused the sign-in",
    ),
    (
        "iss",
        "the issuer of the identity provider that returns the browser, which must be the issuer of the provider the path names",
    ),
];

pub(crate) fn routes() -> Api {
    Api::public()
        .get("/api/v1/auth/oidc/{provider}/start", start)
        .get("/api/v1/auth/oidc/{provider}/callback", callback)
}

pub(crate) struct Returned {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    iss: Option<String>,
    headers: HeaderMap,
}

impl<S: Send + Sync> FromRequestParts<S> for Returned {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Returned, ApiError> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| {
                invalid(format!(
                    "the query string could not be read: {}",
                    rejection.body_text()
                ))
            })?;
        let mut returned = Returned {
            code: None,
            state: None,
            error: None,
            error_description: None,
            iss: None,
            headers: parts.headers.clone(),
        };
        for (name, value) in pairs {
            let slot = match name.as_str() {
                "code" => &mut returned.code,
                "state" => &mut returned.state,
                "error" => &mut returned.error,
                "error_description" => &mut returned.error_description,
                "iss" => &mut returned.iss,
                _ => continue,
            };
            slot.get_or_insert(value);
        }
        Ok(returned)
    }
}

impl Input for Returned {
    fn input(spec: &mut Spec<'_>) {
        for (name, description) in QUERY {
            spec.parameter(json!({
                "name": name,
                "in": "query",
                "description": description,
                "schema": { "type": "string" }
            }));
        }
        spec.parameter(json!({
            "name": COOKIE,
            "in": "cookie",
            "description": "the state of the sign-in, which `GET /api/v1/auth/oidc/{provider}/start` set in this browser",
            "schema": { "type": "string" }
        }));
    }
}

pub(crate) struct Begun {
    location: HeaderValue,
    cookie: HeaderValue,
}

impl IntoResponse for Begun {
    fn into_response(self) -> Response {
        redirect(self.location, [self.cookie])
    }
}

impl Output for Begun {
    fn output(spec: &mut Spec<'_>) {
        spec.empty(StatusCode::SEE_OTHER);
        spec.header(
            StatusCode::SEE_OTHER,
            "Location",
            "the authorization endpoint of the identity provider with the request of the sign-in",
        );
        spec.header(
            StatusCode::SEE_OTHER,
            "Set-Cookie",
            "the cookie `__Host-bosun_oidc`, which holds the state, the nonce and the PKCE \
             verifier of the sign-in for 10 minutes",
        );
    }
}

pub(crate) struct Entered {
    location: HeaderValue,
    session: HeaderValue,
}

impl IntoResponse for Entered {
    fn into_response(self) -> Response {
        redirect(self.location, [self.session, state::cleared()])
    }
}

impl Output for Entered {
    fn output(spec: &mut Spec<'_>) {
        spec.empty(StatusCode::SEE_OTHER);
        spec.header(
            StatusCode::SEE_OTHER,
            "Location",
            "the web interface at `server.public_url`",
        );
        spec.header(
            StatusCode::SEE_OTHER,
            "Set-Cookie",
            "the session cookie `__Host-bosun_session`, and the cookie `__Host-bosun_oidc` \
             emptied",
        );
    }
}

pub(crate) enum Refused {
    Api(ApiError),
    Unauthenticated(String),
}

impl From<ApiError> for Refused {
    fn from(error: ApiError) -> Refused {
        Refused::Api(error)
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Refused::Api(error) => error.into_response(),
            Refused::Unauthenticated(message) => {
                let mut response =
                    ApiError::new(ErrorCode::Unauthenticated, message).into_response();
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                response
            }
        }
    }
}

impl Output for Refused {
    fn output(spec: &mut Spec<'_>) {
        spec.error();
    }
}

async fn start(State(state): State<AppState>, Segment(name): Segment) -> Result<Begun, Refused> {
    let provider = find(&state, &name)?;
    let metadata = provider
        .metadata()
        .await
        .map_err(|failure| unusable(provider, failure))?;
    let started = Started::new(provider.name(), state.clock.now()).map_err(failed)?;
    let location = provider.authorization(
        &metadata,
        &started.state,
        &started.nonce,
        &started.challenge(),
    );
    Ok(Begun {
        location: HeaderValue::from_str(location.as_str()).map_err(failed)?,
        cookie: started
            .cookie(&state.keys)
            .ok_or_else(|| failed("the state of a sign-in could not be written to its cookie"))?,
    })
}

async fn callback(
    State(state): State<AppState>,
    Segment(name): Segment,
    returned: Returned,
) -> Result<Entered, Refused> {
    let provider = find(&state, &name)?;
    let now = state.clock.now();
    let refused = |message: String| {
        info!(provider = %name, %message, "a sign-in through an identity provider was refused");
        Refused::Unauthenticated(message)
    };
    let started =
        Started::read(&returned.headers, &state.keys, provider.name(), now).map_err(|unread| {
            refused(match unread {
                Unread::Missing => format!(
                    "the sign-in through `{name}` did not start in this browser; start it again"
                ),
                Unread::Forged => {
                    "the cookie of the sign-in is not one that Bosun wrote; start the sign-in \
                     again"
                        .to_owned()
                }
                Unread::Elsewhere => format!(
                    "the sign-in that this browser started is not through `{name}`; start it \
                     again"
                ),
                Unread::Expired => format!(
                    "the sign-in through `{name}` started more than {} minutes ago; start it \
                     again",
                    LIFETIME.num_minutes()
                ),
            })
        })?;
    match returned.state.as_deref() {
        Some(given) if sessions::same(given.as_bytes(), started.state.as_bytes()) => {}
        _ => {
            return Err(refused(
                "the state of the callback is not the state of the sign-in that this browser \
                 started"
                    .to_owned(),
            ));
        }
    }
    let spent = bosun_db::identities::spend_state(
        &state.db,
        &Sha256::digest(started.state.as_bytes()),
        started.expires_at(),
    )
    .await
    .map_err(database)?;
    if !spent {
        return Err(refused(
            "the state of the callback was used already; start the sign-in again".to_owned(),
        ));
    }
    if let Some(error) = &returned.error {
        return Err(refused(format!(
            "the identity provider `{name}` refused the sign-in: {}",
            described(error, returned.error_description.as_deref())
        )));
    }
    if let Some(issuer) = returned
        .iss
        .as_deref()
        .filter(|issuer| *issuer != provider.config.issuer)
    {
        return Err(refused(format!(
            "the callback names the issuer `{issuer}`, and `{name}` is `{}`",
            provider.config.issuer
        )));
    }
    let Some(code) = returned.code else {
        let mut faults = Faults::default();
        faults.add(
            "code",
            "is missing, and the callback exchanges it for the ID token",
        );
        return Err(faults.refuse("the callback is not valid").into());
    };
    let answered = |failure: Failure| match failure {
        Failure::Refused(message) => refused(message),
        unreachable => unusable(provider, unreachable),
    };
    let metadata = provider.metadata().await.map_err(answered)?;
    let token = provider
        .exchange(&metadata, &code, &started.verifier)
        .await
        .map_err(answered)?;
    let claims = provider
        .verify(
            &metadata,
            &token,
            &Expected {
                issuer: &provider.config.issuer,
                client_id: &provider.config.client_id,
                nonce: &started.nonce,
                now,
            },
        )
        .await
        .map_err(answered)?;
    let not_admitted = |refusal: Refusal| -> Refused {
        match refusal {
            Refusal::Outside(message) => {
                info!(provider = %name, %message, "a sign-in through an identity provider was refused");
                ApiError::new(ErrorCode::Forbidden, message).into()
            }
            Refusal::Taken(message) => {
                info!(provider = %name, %message, "a sign-in through an identity provider was refused");
                ApiError::new(ErrorCode::Conflict, message).into()
            }
            Refusal::Database(error) => database(error).into(),
        }
    };
    let person = admission::person(&claims, &provider.config).map_err(not_admitted)?;
    if !admission::admitted(&person, &provider.config) {
        return Err(not_admitted(Refusal::Outside(format!(
            "`{}` is in no group, and has no verified email address of a domain, that `{name}` \
             admits to Bosun",
            person.login
        ))));
    }
    let user = admission::admit(&state.db, &provider.config, &person)
        .await
        .map_err(not_admitted)?;
    let session = sessions::start(&state.db, user.id, now).await?;
    info!(
        provider = %name,
        login = %user.login,
        session_id = %session.id,
        "signed in through an identity provider"
    );
    Ok(Entered {
        location: HeaderValue::from_str(&provider.home).map_err(failed)?,
        session: sessions::set_cookie(&session.token)?,
    })
}

fn find<'a>(state: &'a AppState, name: &str) -> Result<&'a Provider, ApiError> {
    state.oidc.find(name).ok_or_else(|| {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no identity provider `{name}`"),
        )
    })
}

fn unusable(provider: &Provider, failure: Failure) -> Refused {
    failure.log(provider.name());
    ApiError::new(
        ErrorCode::Internal,
        format!(
            "the identity provider `{}` cannot be used now; the server's log says why",
            provider.name()
        ),
    )
    .into()
}

fn redirect<const N: usize>(location: HeaderValue, cookies: [HeaderValue; N]) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(LOCATION, location);
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
    (StatusCode::SEE_OTHER, headers).into_response()
}
