use std::sync::Arc;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use bosun_blob::Store;
use bosun_types::id::{AttemptId, ExecutionId};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::index::{
    Artifact, ArtifactFilter, ArtifactId, Finalized, Index, IndexError, NewArtifact, Sha256,
};
use crate::signed::Purpose;
use crate::token::{JobIdentity, JobToken};
use crate::twirp::{Code, TwirpError, json};
use crate::{Services, blob};

pub(crate) const SERVICE: &str = "github.actions.results.api.v1.ArtifactService";
const BODY_LIMIT: usize = 1 << 20;
const MAX_NAME_BYTES: usize = 255;
const MAX_MIME_BYTES: usize = 127;
const ARCHIVE_TYPE: &str = "application/zip";

pub(crate) fn router<S, I>(services: Arc<Services<S, I>>) -> Router
where
    S: Store + 'static,
    I: Index + 'static,
{
    Router::new()
        .route(&format!("/twirp/{SERVICE}/{{method}}"), any(handle::<S, I>))
        .with_state(services)
}

async fn handle<S, I>(
    State(services): State<Arc<Services<S, I>>>,
    Path(method): Path<String>,
    request: Request,
) -> Response
where
    S: Store + 'static,
    I: Index + 'static,
{
    match dispatch(&services, &method, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn dispatch<S, I>(
    services: &Services<S, I>,
    method: &str,
    request: Request,
) -> Result<Response, TwirpError>
where
    S: Store + 'static,
    I: Index + 'static,
{
    if request.method() != Method::POST {
        return Err(TwirpError::new(Code::BadRoute, "only POST is served"));
    }
    if !is_json(&request) {
        return Err(TwirpError::new(
            Code::BadRoute,
            "only the content type application/json is served",
        ));
    }
    let job = request
        .extensions()
        .get::<JobToken>()
        .ok_or_else(|| {
            TwirpError::new(
                Code::Unauthenticated,
                "the request carries no valid job token",
            )
        })?
        .identity();
    let body = to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map_err(|_| {
            TwirpError::new(
                Code::Malformed,
                "the request body cannot be read or is too large",
            )
        })?;
    match method {
        "CreateArtifact" => reply(services.create_artifact(job, parse(&body)?).await?),
        "FinalizeArtifact" => reply(services.finalize_artifact(job, parse(&body)?).await?),
        "ListArtifacts" => reply(services.list_artifacts(job, parse(&body)?).await?),
        "GetSignedArtifactURL" => reply(services.signed_artifact_url(job, parse(&body)?).await?),
        "DeleteArtifact" => reply(services.delete_artifact(job, parse(&body)?).await?),
        _ => Err(TwirpError::new(Code::BadRoute, "no such method")),
    }
}

fn is_json(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, TwirpError> {
    let malformed = |error: serde_json::Error| {
        TwirpError::new(
            Code::Malformed,
            format!("the request is not valid: {error}"),
        )
    };
    let value: Value = serde_json::from_slice(body).map_err(malformed)?;
    if !value.is_object() {
        return Err(TwirpError::new(
            Code::Malformed,
            "the request is not a JSON object",
        ));
    }
    serde_json::from_value(value).map_err(malformed)
}

fn reply<T: Serialize>(message: T) -> Result<Response, TwirpError> {
    serde_json::to_vec(&message)
        .map(|body| json(StatusCode::OK, body))
        .map_err(|error| TwirpError::internal("a response cannot be written", &error))
}

fn int64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(i64),
        Text(String),
    }
    match Option::<Raw>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Raw::Number(number)) => Ok(Some(number)),
        Some(Raw::Text(text)) => text.parse().map(Some).map_err(de::Error::custom),
    }
}

#[derive(Deserialize)]
struct CreateRequest {
    #[serde(default, alias = "workflowRunBackendId")]
    workflow_run_backend_id: String,
    #[serde(default, alias = "workflowJobRunBackendId")]
    workflow_job_run_backend_id: String,
    #[serde(default)]
    name: String,
    #[serde(default, alias = "expiresAt")]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default, alias = "mimeType")]
    mime_type: Option<String>,
}

#[derive(Deserialize)]
struct FinalizeRequest {
    #[serde(default, alias = "workflowRunBackendId")]
    workflow_run_backend_id: String,
    #[serde(default, alias = "workflowJobRunBackendId")]
    workflow_job_run_backend_id: String,
    #[serde(default)]
    name: String,
    #[serde(default, deserialize_with = "int64")]
    size: Option<i64>,
    #[serde(default)]
    hash: Option<String>,
}

#[derive(Deserialize)]
struct ListRequest {
    #[serde(default, alias = "workflowRunBackendId")]
    workflow_run_backend_id: String,
    #[serde(default, alias = "workflowJobRunBackendId")]
    workflow_job_run_backend_id: String,
    #[serde(default, alias = "nameFilter")]
    name_filter: Option<String>,
    #[serde(default, alias = "idFilter", deserialize_with = "int64")]
    id_filter: Option<i64>,
}

#[derive(Deserialize)]
struct TargetRequest {
    #[serde(default, alias = "workflowRunBackendId")]
    workflow_run_backend_id: String,
    #[serde(default, alias = "workflowJobRunBackendId")]
    workflow_job_run_backend_id: String,
    #[serde(default)]
    name: String,
}

#[derive(Serialize)]
struct CreateResponse {
    ok: bool,
    signed_upload_url: String,
}

#[derive(Serialize)]
struct FinalizeResponse {
    ok: bool,
    artifact_id: String,
}

#[derive(Serialize)]
struct ListResponse {
    artifacts: Vec<Listed>,
}

#[derive(Serialize)]
struct Listed {
    workflow_run_backend_id: String,
    workflow_job_run_backend_id: String,
    database_id: String,
    name: String,
    size: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

#[derive(Serialize)]
struct SignedResponse {
    signed_url: String,
}

#[derive(Serialize)]
struct DeleteResponse {
    ok: bool,
    artifact_id: String,
}

fn ids(run: &str, job: &str) -> Result<(AttemptId, ExecutionId), TwirpError> {
    let attempt = run.parse::<AttemptId>().map_err(|_| {
        TwirpError::new(
            Code::InvalidArgument,
            "workflow_run_backend_id is not a run",
        )
    })?;
    let execution = job.parse::<ExecutionId>().map_err(|_| {
        TwirpError::new(
            Code::InvalidArgument,
            "workflow_job_run_backend_id is not a job",
        )
    })?;
    Ok((attempt, execution))
}

fn own(run: &str, job: &str, caller: JobIdentity) -> Result<(), TwirpError> {
    let (attempt, execution) = ids(run, job)?;
    if attempt == caller.attempt && execution == caller.execution {
        Ok(())
    } else {
        Err(TwirpError::new(
            Code::PermissionDenied,
            "the job token belongs to another run or job",
        ))
    }
}

fn name(name: &str) -> Result<(), TwirpError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(TwirpError::new(
            Code::InvalidArgument,
            format!("an artifact name is 1 to {MAX_NAME_BYTES} bytes without control characters"),
        ));
    }
    Ok(())
}

fn content_type(requested: Option<String>) -> Result<String, TwirpError> {
    let Some(requested) = requested.filter(|value| !value.is_empty()) else {
        return Ok(ARCHIVE_TYPE.to_owned());
    };
    let token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
    };
    match requested.split_once('/') {
        Some((main, sub)) if requested.len() <= MAX_MIME_BYTES && token(main) && token(sub) => {
            Ok(requested)
        }
        _ => Err(TwirpError::new(
            Code::InvalidArgument,
            "mime_type is not a media type without parameters",
        )),
    }
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn index_error(context: &str, error: IndexError) -> TwirpError {
    match error {
        IndexError::AlreadyExists => TwirpError::new(
            Code::AlreadyExists,
            "an artifact with this name already exists on the workflow run",
        ),
        IndexError::NotFound => TwirpError::new(Code::NotFound, "the artifact does not exist"),
        IndexError::Finalized => TwirpError::new(
            Code::FailedPrecondition,
            "the artifact was finalised with another size or digest",
        ),
        IndexError::Backend(_) => TwirpError::internal(context, &error),
    }
}

impl<S, I> Services<S, I>
where
    S: Store + 'static,
    I: Index + 'static,
{
    fn address(&self, purpose: Purpose, id: ArtifactId) -> String {
        let object = blob::object(id);
        let query = self
            .signer
            .query(purpose, &object, self.now() + crate::signed::LIFETIME);
        format!("{}/_runtime/blob/{object}?{query}", self.origin)
    }

    async fn named(
        &self,
        job: JobIdentity,
        request: &TargetRequest,
    ) -> Result<Artifact, TwirpError> {
        name(&request.name)?;
        let (attempt, execution) = ids(
            &request.workflow_run_backend_id,
            &request.workflow_job_run_backend_id,
        )?;
        let filter = ArtifactFilter {
            name: Some(request.name.clone()),
            id: None,
        };
        self.index
            .artifacts(job.attempt, &filter)
            .await
            .map_err(|error| index_error("artifacts cannot be listed", error))?
            .into_iter()
            .filter(|artifact| {
                artifact.finalized.is_some()
                    && artifact.attempt == attempt
                    && artifact.execution == execution
            })
            .max_by_key(|artifact| artifact.id)
            .ok_or_else(|| TwirpError::new(Code::NotFound, "no such artifact"))
    }

    async fn discard_pending(&self, job: JobIdentity, name: &str) -> Result<(), TwirpError> {
        let filter = ArtifactFilter {
            name: Some(name.to_owned()),
            id: None,
        };
        let pending = self
            .index
            .artifacts(job.attempt, &filter)
            .await
            .map_err(|error| index_error("artifacts cannot be listed", error))?
            .into_iter()
            .filter(|artifact| {
                artifact.finalized.is_none()
                    && artifact.attempt == job.attempt
                    && artifact.execution == job.execution
            });
        for artifact in pending {
            self.store
                .delete(&artifact.key)
                .await
                .map_err(|error| TwirpError::internal("an artifact cannot be removed", &error))?;
            self.clear_blocks(artifact.id)
                .await
                .map_err(|error| TwirpError::internal("staged blocks cannot be removed", &error))?;
            self.index
                .delete_artifact(job.attempt, artifact.id)
                .await
                .map_err(|error| index_error("an artifact cannot be deleted", error))?;
        }
        Ok(())
    }

    async fn create_artifact(
        &self,
        job: JobIdentity,
        request: CreateRequest,
    ) -> Result<CreateResponse, TwirpError> {
        own(
            &request.workflow_run_backend_id,
            &request.workflow_job_run_backend_id,
            job,
        )?;
        name(&request.name)?;
        let content_type = content_type(request.mime_type)?;
        self.discard_pending(job, &request.name).await?;
        let artifact = self
            .index
            .create_artifact(NewArtifact {
                attempt: job.attempt,
                execution: job.execution,
                name: request.name,
                content_type,
                created_at: self.now(),
                expires_at: request.expires_at,
            })
            .await
            .map_err(|error| index_error("an artifact cannot be created", error))?;
        Ok(CreateResponse {
            ok: true,
            signed_upload_url: self.address(Purpose::Upload, artifact.id),
        })
    }

    async fn finalize_artifact(
        &self,
        job: JobIdentity,
        request: FinalizeRequest,
    ) -> Result<FinalizeResponse, TwirpError> {
        own(
            &request.workflow_run_backend_id,
            &request.workflow_job_run_backend_id,
            job,
        )?;
        name(&request.name)?;
        let size = u64::try_from(request.size.unwrap_or(0))
            .map_err(|_| TwirpError::new(Code::InvalidArgument, "size is negative"))?;
        let sha256 = match request.hash.as_deref() {
            None | Some("") => None,
            Some(hash) => Some(Sha256::parse(hash).ok_or_else(|| {
                TwirpError::new(
                    Code::InvalidArgument,
                    "hash is not sha256: and 64 hex digits",
                )
            })?),
        };
        let filter = ArtifactFilter {
            name: Some(request.name),
            id: None,
        };
        let artifact = self
            .index
            .artifacts(job.attempt, &filter)
            .await
            .map_err(|error| index_error("artifacts cannot be listed", error))?
            .into_iter()
            .find(|artifact| artifact.execution == job.execution)
            .ok_or_else(|| {
                TwirpError::new(Code::NotFound, "this job created no artifact of that name")
            })?;
        let stored = match self.store.get(&artifact.key, 0..0).await {
            Ok(object) => object.size(),
            Err(bosun_blob::Error::NotFound(_)) => {
                return Err(TwirpError::new(
                    Code::FailedPrecondition,
                    "the artifact has no uploaded content",
                ));
            }
            Err(error) => return Err(TwirpError::internal("an artifact cannot be read", &error)),
        };
        if stored != size {
            return Err(TwirpError::new(
                Code::FailedPrecondition,
                "the declared size differs from the size of the stored object",
            ));
        }
        self.clear_blocks(artifact.id)
            .await
            .map_err(|error| TwirpError::internal("staged blocks cannot be removed", &error))?;
        self.index
            .finalize_artifact(
                artifact.id,
                Finalized {
                    size,
                    sha256,
                    at: self.now(),
                },
            )
            .await
            .map_err(|error| index_error("an artifact cannot be finalised", error))?;
        Ok(FinalizeResponse {
            ok: true,
            artifact_id: artifact.id.to_string(),
        })
    }

    async fn list_artifacts(
        &self,
        job: JobIdentity,
        request: ListRequest,
    ) -> Result<ListResponse, TwirpError> {
        own(
            &request.workflow_run_backend_id,
            &request.workflow_job_run_backend_id,
            job,
        )?;
        let id = match request.id_filter {
            None => None,
            Some(id) => match ArtifactId::new(id) {
                Some(id) => Some(id),
                None => {
                    return Ok(ListResponse {
                        artifacts: Vec::new(),
                    });
                }
            },
        };
        let filter = ArtifactFilter {
            name: request.name_filter,
            id,
        };
        let artifacts = self
            .index
            .artifacts(job.attempt, &filter)
            .await
            .map_err(|error| index_error("artifacts cannot be listed", error))?
            .into_iter()
            .filter_map(|artifact| {
                let finalized = artifact.finalized?;
                Some(Listed {
                    workflow_run_backend_id: artifact.attempt.to_string(),
                    workflow_job_run_backend_id: artifact.execution.to_string(),
                    database_id: artifact.id.to_string(),
                    name: artifact.name,
                    size: finalized.size.to_string(),
                    created_at: timestamp(artifact.created_at),
                    digest: finalized.sha256.map(|digest| digest.to_string()),
                })
            })
            .collect();
        Ok(ListResponse { artifacts })
    }

    async fn signed_artifact_url(
        &self,
        job: JobIdentity,
        request: TargetRequest,
    ) -> Result<SignedResponse, TwirpError> {
        let artifact = self.named(job, &request).await?;
        Ok(SignedResponse {
            signed_url: self.address(Purpose::Download, artifact.id),
        })
    }

    async fn delete_artifact(
        &self,
        job: JobIdentity,
        request: TargetRequest,
    ) -> Result<DeleteResponse, TwirpError> {
        let artifact = self.named(job, &request).await?;
        self.store
            .delete(&artifact.key)
            .await
            .map_err(|error| TwirpError::internal("an artifact cannot be removed", &error))?;
        self.clear_blocks(artifact.id)
            .await
            .map_err(|error| TwirpError::internal("staged blocks cannot be removed", &error))?;
        let removed = self
            .index
            .delete_artifact(job.attempt, artifact.id)
            .await
            .map_err(|error| index_error("an artifact cannot be deleted", error))?;
        if !removed {
            return Err(TwirpError::new(Code::NotFound, "no such artifact"));
        }
        Ok(DeleteResponse {
            ok: true,
            artifact_id: artifact.id.to_string(),
        })
    }
}
