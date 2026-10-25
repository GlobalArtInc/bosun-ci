use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bosun_blob::{Key, Store};
use bosun_types::api::Agent;
use bosun_types::error::ErrorCode;
use tokio_util::io::ReaderStream;
use tracing::warn;

use crate::AppState;
use crate::auth;
use crate::envelope::ApiError;
use crate::request::{database, failed};
use crate::token::{AGENT, Token};
use crate::tools::PLATFORMS;

const TARBALL: &str = ".tar.gz";

const ARCHIVE: &str = ".tar.xz";

const CHUNK: usize = 64 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/_runtime/actions/{owner}/{repository}/{archive}",
            get(action),
        )
        .route("/_runtime/tools/node/{version}/{archive}", get(node))
}

async fn action(
    State(state): State<AppState>,
    Path((owner, repository, archive)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let agent = authenticate(&state, &headers).await?;
    let missing = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("the server keeps no action {owner}/{repository} at {archive}"),
        )
    };
    let commit = archive.strip_suffix(TARBALL).ok_or_else(missing)?;
    let held = bosun_db::actions::held(&state.db, agent.id, &owner, &repository, commit)
        .await
        .map_err(database)?;
    if !held {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            format!(
                "agent `{}` holds no execution that uses the action {owner}/{repository} at \
                 {commit}",
                agent.name
            ),
        ));
    }
    let stored = bosun_db::actions::find(&state.db, &owner, &repository, commit)
        .await
        .map_err(database)?
        .ok_or_else(missing)?;
    serve(&state, &stored.key, stored.size, "application/gzip").await
}

async fn node(
    State(state): State<AppState>,
    Path((version, archive)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers).await?;
    let platform = archive
        .strip_suffix(ARCHIVE)
        .filter(|platform| PLATFORMS.contains(platform))
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!(
                    "there is no Node for {archive}; the server has Node for {}, each as \
                     <platform>{ARCHIVE}",
                    PLATFORMS.join(", ")
                ),
            )
        })?;
    if !state.tools.runtimes().serves(&version) {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!(
                "the server serves no Node {version}, since its runtime map names no action \
                 runtime that runs on it"
            ),
        ));
    }
    match state.tools.node(&version, platform).await {
        Ok(Ok(stored)) => serve(&state, &stored.key, stored.size, "application/x-xz").await,
        Ok(Err(refused)) => {
            warn!(
                version,
                platform,
                reason = refused,
                "a Node runtime could not be fetched"
            );
            Err(ApiError::new(
                ErrorCode::Internal,
                format!("Node {version} for {platform} cannot be fetched: {refused}"),
            ))
        }
        Err(failure) => Err(failed(failure)),
    }
}

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Agent, ApiError> {
    let refused = |message: &str| ApiError::new(ErrorCode::Unauthenticated, message);
    let header = headers
        .get(AUTHORIZATION)
        .ok_or_else(|| refused("the request carries no agent credential"))?;
    let credential = header
        .to_str()
        .ok()
        .and_then(auth::bearer)
        .ok_or_else(|| refused("the authorization header does not hold a bearer token"))?;
    let token = Token::parse(AGENT, credential)
        .ok_or_else(|| refused("the bearer token is not a well-formed agent credential"))?;
    bosun_db::agents::authenticate(&state.db, &token.sha256())
        .await
        .map_err(database)?
        .ok_or_else(|| refused("the agent credential is not known"))
}

async fn serve(
    state: &AppState,
    key: &str,
    size: u64,
    content_type: &'static str,
) -> Result<Response, ApiError> {
    let key = Key::new(key).map_err(failed)?;
    let object = state.store.get(&key, ..).await.map_err(failed)?;
    Ok((
        [
            (CONTENT_TYPE, content_type.to_owned()),
            (CONTENT_LENGTH, size.to_string()),
        ],
        Body::from_stream(ReaderStream::with_capacity(object, CHUNK)),
    )
        .into_response())
}
