use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use axum::Json;
use axum::extract::{ConnectInfo, FromRequest, Request, State};
use axum::http::header::{CONTENT_TYPE, RETRY_AFTER, SET_COOKIE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bosun_db::sign_in::{Granted, Kind, Lockout, Reservation, Subject};
use bosun_types::api::{Me, PasswordChangeRequest, SignInMethods, SignInRequest};
use bosun_types::error::ErrorCode;
use chrono::{DateTime, TimeDelta, Utc};
use ipnet::IpNet;
use schemars::JsonSchema;
use sea_orm::TransactionTrait;
use serde::de::DeserializeOwned;
use tracing::info;

use crate::access::{self, Authenticated, Caller};
use crate::auth::Refusal;
use crate::envelope::ApiError;
use crate::openapi::{Api, Input, Output, Spec};
use crate::password::{self, Hasher};
use crate::request::{Body, Faults, database, failed, invalid};
use crate::{AppState, address, owner, sessions};

pub(crate) const FORGET: TimeDelta = TimeDelta::days(1);

const FIRST_LOCKOUT: TimeDelta = TimeDelta::minutes(1);

const LONGEST_LOCKOUT: TimeDelta = TimeDelta::days(1);

const JSON: &str = "application/json";

pub(crate) struct Settings {
    pub(crate) hasher: Hasher,
    pub(crate) trusted_proxies: Vec<IpNet>,
    pub(crate) lockout: Lockout,
    pub(crate) data_dir: PathBuf,
}

impl Settings {
    pub(crate) fn new(
        trusted_proxies: Vec<IpNet>,
        failed_sign_ins: u32,
        data_dir: PathBuf,
    ) -> Settings {
        Settings {
            hasher: Hasher::new(),
            trusted_proxies,
            lockout: Lockout {
                failures: failed_sign_ins,
                first: FIRST_LOCKOUT,
                longest: LONGEST_LOCKOUT,
                forget: FORGET,
            },
            data_dir,
        }
    }
}

pub(crate) fn routes() -> Api {
    Api::public()
        .post("/api/v1/auth/login", login)
        .get("/api/v1/auth/methods", methods)
        .merge(
            Api::interim()
                .post("/api/v1/auth/logout", logout)
                .post("/api/v1/auth/password", change_password),
        )
}

pub(crate) struct Attempt<T> {
    body: T,
    address: IpAddr,
}

impl<T: DeserializeOwned> FromRequest<AppState> for Attempt<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Attempt<T>, ApiError> {
        let typed = request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|media| media.trim().eq_ignore_ascii_case(JSON));
        if !typed {
            return Err(invalid(format!(
                "a request that checks a password has a JSON body with the content type {JSON}"
            )));
        }
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(peer)| peer.ip())
            .ok_or_else(|| failed("a request that checks a password came without its peer"))?;
        let address = address::client(peer, request.headers(), &state.sign_in.trusted_proxies);
        let Body(body) = Body::<T>::from_request(request, state).await?;
        Ok(Attempt { body, address })
    }
}

impl<T: JsonSchema> Input for Attempt<T> {
    fn input(spec: &mut Spec<'_>) {
        spec.body::<T>();
    }
}

pub(crate) struct SignedIn {
    cookie: HeaderValue,
    me: Me,
}

impl IntoResponse for SignedIn {
    fn into_response(self) -> Response {
        (StatusCode::OK, [(SET_COOKIE, self.cookie)], Json(self.me)).into_response()
    }
}

impl Output for SignedIn {
    fn output(spec: &mut Spec<'_>) {
        spec.json::<Me>(StatusCode::OK);
        spec.header(
            StatusCode::OK,
            "Set-Cookie",
            "the session cookie `__Host-bosun_session`",
        );
    }
}

pub(crate) struct SignedOut;

impl IntoResponse for SignedOut {
    fn into_response(self) -> Response {
        (
            StatusCode::NO_CONTENT,
            [(SET_COOKIE, sessions::clear_cookie())],
        )
            .into_response()
    }
}

impl Output for SignedOut {
    fn output(spec: &mut Spec<'_>) {
        spec.empty(StatusCode::NO_CONTENT);
        spec.header(
            StatusCode::NO_CONTENT,
            "Set-Cookie",
            "the session cookie `__Host-bosun_session`, emptied",
        );
    }
}

pub(crate) enum Failure {
    Api(ApiError),
    Refused(Refusal),
    Locked { message: String, retry_after: i64 },
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Failure {
        Failure::Api(error)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        match self {
            Failure::Api(error) => error.into_response(),
            Failure::Refused(refusal) => refusal.into_response(),
            Failure::Locked {
                message,
                retry_after,
            } => {
                let mut response = ApiError::new(ErrorCode::RateLimited, message).into_response();
                response
                    .headers_mut()
                    .insert(RETRY_AFTER, HeaderValue::from(retry_after));
                response
            }
        }
    }
}

impl Output for Failure {
    fn output(spec: &mut Spec<'_>) {
        spec.error();
    }
}

async fn login(
    State(state): State<AppState>,
    attempt: Attempt<SignInRequest>,
) -> Result<SignedIn, Failure> {
    let Attempt {
        body: request,
        address,
    } = attempt;
    let now = state.clock.now();
    let granted = reserve(&state, &request.login, address, now).await?;
    let found = bosun_db::users::credentials_by_login(&state.db, &request.login)
        .await
        .map_err(database)?;
    let stored = found
        .as_ref()
        .and_then(|credentials| credentials.password_hash.clone());
    let matches = state
        .sign_in
        .hasher
        .verify(request.password, stored)
        .await
        .map_err(failed)?;
    let Some(credentials) = found.filter(|_| matches) else {
        info!(login = %request.login, %address, "a sign-in failed");
        return Err(Failure::Refused(Refusal::Unauthenticated(
            "the login or the password is not right",
        )));
    };
    bosun_db::sign_in::release(&state.db, granted)
        .await
        .map_err(database)?;
    let permissions = access::load(&state.db, &credentials.user, None)
        .await
        .map_err(database)?;
    let started = sessions::start(&state.db, credentials.user.id, now).await?;
    info!(
        login = %credentials.user.login,
        %address,
        session_id = %started.id,
        password_change_required = credentials.password_change_required,
        "signed in"
    );
    Ok(SignedIn {
        cookie: sessions::set_cookie(&started.token)?,
        me: Me {
            user: credentials.user,
            csrf_token: Some(sessions::csrf(&started.token)),
            password_change_required: credentials.password_change_required,
            permissions,
        },
    })
}

async fn methods() -> Json<SignInMethods> {
    Json(SignInMethods {
        password: true,
        providers: Vec::new(),
    })
}

async fn logout(
    State(state): State<AppState>,
    caller: Caller<Authenticated>,
) -> Result<SignedOut, ApiError> {
    let principal = caller.principal;
    let session = principal.session().ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            "the request is authenticated by a personal token, and only a session signs out",
        )
    })?;
    bosun_db::sessions::end(&state.db, session)
        .await
        .map_err(database)?;
    info!(login = %principal.user.login, session_id = %session, "signed out");
    Ok(SignedOut)
}

async fn change_password(
    State(state): State<AppState>,
    caller: Caller<Authenticated>,
    attempt: Attempt<PasswordChangeRequest>,
) -> Result<Json<Me>, Failure> {
    let principal = caller.principal;
    let Attempt {
        body: request,
        address,
    } = attempt;
    let login = principal.user.login.clone();
    let mut faults = Faults::default();
    if !password::acceptable(&request.new_password) {
        faults.add("new_password", password::RULE);
    } else if request.new_password == request.current_password {
        faults.add(
            "new_password",
            "is the current password, and a new password differs from it",
        );
    }
    faults.finish("the new password is not valid")?;
    let credentials = bosun_db::users::credentials(&state.db, principal.user.id)
        .await
        .map_err(database)?
        .ok_or_else(|| failed(format!("the user `{login}` vanished during the request")))?;
    let current = credentials.password_hash.ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            format!("`{login}` has no password, so there is none to change"),
        )
    })?;
    let granted = reserve(&state, &login, address, state.clock.now()).await?;
    let hasher = &state.sign_in.hasher;
    if !hasher
        .verify(request.current_password, Some(current))
        .await
        .map_err(failed)?
    {
        info!(%login, %address, "a change of password gave a wrong current password");
        let mut faults = Faults::default();
        faults.add(
            "current_password",
            format!("is not the password of `{login}`"),
        );
        return Err(faults.refuse("the current password is not right").into());
    }
    let hash = hasher.hash(request.new_password).await.map_err(failed)?;
    let transaction = state.db.begin().await.map_err(database)?;
    bosun_db::users::set_password(&transaction, principal.user.id, hash)
        .await
        .map_err(database)?;
    let ended =
        bosun_db::sessions::end_others(&transaction, principal.user.id, principal.session())
            .await
            .map_err(database)?;
    bosun_db::sign_in::release(&transaction, granted)
        .await
        .map_err(database)?;
    transaction.commit().await.map_err(database)?;
    info!(%login, ended_sessions = ended, "changed the password");
    if credentials.password_change_required && login == owner::FIRST_OWNER {
        owner::forget_first_start(&state.sign_in.data_dir);
    }
    Ok(Json(Me {
        csrf_token: principal.csrf_token(),
        user: principal.user,
        password_change_required: false,
        permissions: principal.permissions,
    }))
}

async fn reserve(
    state: &AppState,
    login: &str,
    address: IpAddr,
    now: DateTime<Utc>,
) -> Result<Granted, Failure> {
    let mut subjects = Vec::with_capacity(2);
    if owner::is_login(login) {
        subjects.push(Subject {
            kind: Kind::Account,
            key: login.to_owned(),
        });
    }
    subjects.push(Subject {
        kind: Kind::Address,
        key: address.to_string(),
    });
    match bosun_db::sign_in::reserve(&state.db, subjects, now, &state.sign_in.lockout)
        .await
        .map_err(database)?
    {
        Reservation::Granted(granted) => Ok(granted),
        Reservation::Locked { subject, until } => {
            let milliseconds = (until - now).num_milliseconds().max(1);
            let retry_after = (milliseconds + 999) / 1000;
            let whose = match subject.kind {
                Kind::Account => format!("for `{}`", subject.key),
                Kind::Address => format!("from {}", subject.key),
            };
            info!(%login, %address, %until, "a sign-in was refused during a lockout");
            Err(Failure::Locked {
                message: format!(
                    "too many failed sign-ins {whose}; sign-in is locked for {retry_after} more \
                     seconds"
                ),
                retry_after,
            })
        }
    }
}
