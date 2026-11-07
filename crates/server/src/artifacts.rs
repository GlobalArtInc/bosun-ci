use axum::Json;
use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::extract::State;
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, RANGE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bosun_blob::{Key, Store as _};
use bosun_db::artifacts::Stored;
use bosun_db::projects::Holder;
use bosun_services::{ArtifactId, MAX_ARTIFACT_ID};
use bosun_types::api::{Artifact, Page};
use bosun_types::error::ErrorCode;
use bosun_types::id::RunId;
use sea_orm::prelude::Uuid;
use serde_json::json;
use tokio_util::io::ReaderStream;
use tracing::info;

use crate::AppState;
use crate::access::{Caller, OnProject, ProjectView, RunOverride};
use crate::envelope::ApiError;
use crate::openapi::{Api, Input, Output, Spec};
use crate::reply::Removed;
use crate::request::{Listing, Segment, database, failed, invalid};

const CHUNK: usize = 64 * 1024;

const OCTETS: &str = "application/octet-stream";

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/runs/{run}/artifacts", list)
        .get("/api/v1/artifacts/{artifact}", download)
        .delete("/api/v1/artifacts/{artifact}", delete)
}

fn missing(reference: &str) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("there is no artifact `{reference}`"),
    )
}

fn present(stored: Stored) -> Result<Artifact, ApiError> {
    let done = stored
        .finalized
        .ok_or_else(|| failed("an artifact listed as finalised has no finalisation"))?;
    Ok(Artifact {
        id: stored.id,
        run_id: stored.run_id,
        attempt_id: stored.attempt_id,
        execution_id: stored.execution_id,
        name: stored.name,
        content_type: stored.content_type,
        size: done.size,
        digest: done
            .sha256
            .map(|digest| bosun_services::Sha256::new(digest).to_string()),
        created_at: stored.created_at,
        finalized_at: done.at,
        expires_at: stored.expires_at,
    })
}

fn after(cursor: Option<Uuid>) -> Result<i64, ApiError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    i64::try_from(cursor.as_u128())
        .ok()
        .filter(|id| (0..=MAX_ARTIFACT_ID).contains(id))
        .ok_or_else(|| {
            invalid(format!(
                "`cursor` is `{cursor}`, which is not the `next` of a page of this list"
            ))
        })
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<Artifact>>, ApiError> {
    let absent = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no run `{reference}`"),
        )
    };
    let run: RunId = reference.parse().map_err(|_| absent())?;
    caller.holding(&state.db, Holder::Run(run), absent).await?;
    let listing = listing?;
    let found =
        bosun_db::artifacts::of_run(&state.db, run, after(listing.cursor())?, listing.fetch())
            .await
            .map_err(database)?;
    let items = found
        .into_iter()
        .map(present)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(listing.page(items, |artifact| {
        Uuid::from_u128(u128::from(artifact.id.unsigned_abs()))
    })))
}

pub(crate) struct Ranged(Option<HeaderValue>);

impl<S: Send + Sync> FromRequestParts<S> for Ranged {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Ranged, ApiError> {
        let mut values = parts.headers.get_all(RANGE).iter();
        match (values.next(), values.next()) {
            (first, None) => Ok(Ranged(first.cloned())),
            (Some(_), Some(_)) => Err(invalid("the request carries more than one `Range` header")),
            (None, Some(_)) => Err(failed("a header list has a second value and no first")),
        }
    }
}

impl Input for Ranged {
    fn input(spec: &mut Spec<'_>) {
        spec.parameter(json!({
            "name": "Range",
            "in": "header",
            "description": "one byte range of the artifact, `bytes=<first>-<last>`, \
                `bytes=<first>-` or `bytes=-<count>`; the answer is `206` with `Content-Range`",
            "schema": { "type": "string" }
        }));
    }
}

#[derive(Clone, Copy)]
enum Wanted {
    From(u64),
    Between(u64, u64),
    Last(u64),
}

impl Ranged {
    fn wanted(&self) -> Result<Option<Wanted>, ApiError> {
        let Some(value) = &self.0 else {
            return Ok(None);
        };
        let invalid = || invalid("`Range` is not a single byte range of the form `bytes=a-b`");
        let text = value.to_str().map_err(|_| invalid())?;
        let (unit, spec) = text.split_once('=').ok_or_else(invalid)?;
        if !unit.trim().eq_ignore_ascii_case("bytes") {
            return Err(invalid());
        }
        let number = |digits: &str| -> Result<u64, ApiError> {
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
                digits.parse().map_err(|_| invalid())
            } else {
                Err(invalid())
            }
        };
        let (first, last) = spec.trim().split_once('-').ok_or_else(invalid)?;
        match (first, last) {
            ("", last) => number(last).map(Wanted::Last).map(Some),
            (first, "") => number(first).map(Wanted::From).map(Some),
            (first, last) => {
                let (first, last) = (number(first)?, number(last)?);
                if first > last {
                    Err(invalid())
                } else {
                    Ok(Some(Wanted::Between(first, last)))
                }
            }
        }
    }
}

fn resolve(wanted: Wanted, size: u64) -> Option<(u64, u64)> {
    let last = size.checked_sub(1)?;
    match wanted {
        Wanted::From(first) if first <= last => Some((first, last)),
        Wanted::Between(first, end) if first <= last => Some((first, end.min(last))),
        Wanted::Last(count) if count > 0 => Some((size.saturating_sub(count), last)),
        _ => None,
    }
}

pub(crate) struct Download(Response);

impl IntoResponse for Download {
    fn into_response(self) -> Response {
        self.0
    }
}

impl Output for Download {
    fn output(spec: &mut Spec<'_>) {
        let content = json!({ OCTETS: { "schema": { "type": "string", "format": "binary" } } });
        spec.content(StatusCode::OK, content.clone());
        spec.content(StatusCode::PARTIAL_CONTENT, content);
        spec.empty(StatusCode::RANGE_NOT_SATISFIABLE);
    }
}

fn disposition(name: &str) -> String {
    let name = match name.rsplit(['/', '\\']).next() {
        Some("" | "." | "..") | None => "artifact",
        Some(last) => last,
    };
    let mut plain = String::with_capacity(name.len());
    for character in name.chars() {
        match character {
            '"' | '\\' => {
                plain.push('\\');
                plain.push(character);
            }
            _ if character.is_ascii() && !character.is_ascii_control() => plain.push(character),
            _ => plain.push('_'),
        }
    }
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}")
}

fn header(value: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(value).map_err(failed)
}

async fn finalised(state: &AppState, reference: &str) -> Result<Stored, ApiError> {
    let absent = || missing(reference);
    let id: ArtifactId = reference.parse().map_err(|_| absent())?;
    bosun_db::artifacts::find(&state.db, id.get())
        .await
        .map_err(database)?
        .filter(|stored| stored.finalized.is_some())
        .ok_or_else(absent)
}

async fn download(
    State(state): State<AppState>,
    caller: Caller<OnProject<ProjectView>>,
    Segment(reference): Segment,
    ranged: Ranged,
) -> Result<Download, ApiError> {
    let absent = || missing(&reference);
    let id: ArtifactId = reference.parse().map_err(|_| absent())?;
    caller
        .holding(&state.db, Holder::Artifact(id.get()), absent)
        .await?;
    let stored = finalised(&state, &reference).await?;
    let wanted = ranged.wanted()?;
    let key = Key::new(stored.key.as_str()).map_err(failed)?;
    let gone = || {
        ApiError::new(
            ErrorCode::Expired,
            format!(
                "the store holds no content for artifact `{reference}`; retention or a restore \
                 of an older bucket removed it"
            ),
        )
    };
    let size = match state.store.get(&key, 0..0).await {
        Ok(object) => object.size(),
        Err(bosun_blob::Error::NotFound(_)) => return Err(gone()),
        Err(error) => return Err(failed(error)),
    };
    let window = match wanted {
        None => size.checked_sub(1).map(|last| (0, last)),
        Some(wanted) => match resolve(wanted, size) {
            Some(window) => Some(window),
            None => {
                let mut response = ApiError::new(
                    ErrorCode::InvalidInput,
                    format!("the range lies outside the artifact, which is {size} bytes"),
                )
                .into_response();
                *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                response
                    .headers_mut()
                    .insert(CONTENT_RANGE, header(&format!("bytes */{size}"))?);
                return Ok(Download(response));
            }
        },
    };
    let length = window.map_or(0, |(first, last)| last - first + 1);
    let body = match window {
        Some((first, last)) => {
            let object = match state.store.get(&key, first..=last).await {
                Ok(object) => object,
                Err(bosun_blob::Error::NotFound(_)) => return Err(gone()),
                Err(error) => return Err(failed(error)),
            };
            Body::from_stream(ReaderStream::with_capacity(object, CHUNK))
        }
        None => Body::empty(),
    };
    let mut response = Response::new(body);
    let out = response.headers_mut();
    out.insert(CONTENT_TYPE, header(&stored.content_type)?);
    out.insert(CONTENT_LENGTH, header(&length.to_string())?);
    out.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    out.insert(CONTENT_DISPOSITION, header(&disposition(&stored.name))?);
    out.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    out.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    out.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let (Some(_), Some((first, last))) = (wanted, window) {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            CONTENT_RANGE,
            header(&format!("bytes {first}-{last}/{size}"))?,
        );
    }
    Ok(Download(response))
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnProject<RunOverride>>,
    Segment(reference): Segment,
) -> Result<Removed, ApiError> {
    let absent = || missing(&reference);
    let id: ArtifactId = reference.parse().map_err(|_| absent())?;
    caller
        .holding(&state.db, Holder::Artifact(id.get()), absent)
        .await?;
    let stored = finalised(&state, &reference).await?;
    let key = Key::new(stored.key.as_str()).map_err(failed)?;
    state.store.delete(&key).await.map_err(failed)?;
    if !bosun_db::artifacts::delete(&state.db, id.get())
        .await
        .map_err(database)?
    {
        return Err(absent());
    }
    info!(
        artifact = id.get(),
        run = %stored.run_id,
        user = caller.principal.user.login,
        "deleted an artifact"
    );
    Ok(Removed)
}

#[cfg(test)]
mod tests {
    use super::{disposition, header};

    #[test]
    fn a_file_name_reaches_the_header_as_its_last_segment_quoted_and_encoded() {
        let named = disposition("dist/say \"hi\" é.txt");
        assert_eq!(
            named,
            "attachment; filename=\"say \\\"hi\\\" _.txt\"; \
             filename*=UTF-8''say%20%22hi%22%20%C3%A9.txt"
        );
        assert!(header(&named).is_ok());
        assert_eq!(
            disposition("C:\\temp\\report.txt"),
            "attachment; filename=\"report.txt\"; filename*=UTF-8''report.txt"
        );
        for bare in ["..", "a/..", "a\\b\\..", ".", "", "a/"] {
            assert_eq!(
                disposition(bare),
                "attachment; filename=\"artifact\"; filename*=UTF-8''artifact",
                "{bare:?}"
            );
        }
    }
}
