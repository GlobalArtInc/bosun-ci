mod address;
mod blob;
mod token;
mod twirp;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bosun_blob::Directory;
use bosun_blob::Store;
use bosun_services::{
    AddressSigner, JobIdentity, KeySet, MAX_BLOCKS, MemoryIndex, Services, SigningKey,
};
use bosun_types::id::{AttemptId, ExecutionId};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

pub const ORIGIN: &str = "http://127.0.0.1:8080";
const SECRET: [u8; 32] = [0x42; 32];

pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn run(number: u8) -> AttemptId {
    format!("0192f3a4-5b6c-7d8e-9f01-0000000000a{number}")
        .parse()
        .expect("a run id")
}

pub fn job(number: u8) -> ExecutionId {
    format!("0192f3a4-5b6c-7d8e-9f01-0000000000b{number}")
        .parse()
        .expect("a job id")
}

pub fn identity(number: u8) -> JobIdentity {
    JobIdentity {
        attempt: run(number),
        execution: job(number),
    }
}

pub fn start() -> DateTime<Utc> {
    "2026-11-07T10:00:00Z".parse().expect("a time")
}

pub fn block(number: u8) -> String {
    STANDARD.encode(format!("block-{number:04}"))
}

pub fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

#[derive(Clone)]
struct Auth {
    keys: Arc<KeySet>,
    now: Arc<Mutex<DateTime<Utc>>>,
}

async fn authenticate(State(auth): State<Auth>, mut request: Request, next: Next) -> Response {
    if let Some(value) = request.headers().get(header::AUTHORIZATION) {
        let now = *auth.now.lock().expect("the clock");
        let verified = value
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(|token| auth.keys.verify(token, now));
        match verified {
            Some(Ok(token)) => {
                request.extensions_mut().insert(token);
            }
            _ => return StatusCode::UNAUTHORIZED.into_response(),
        }
    }
    next.run(request).await
}

pub struct Harness {
    app: Router,
    directory: TempDir,
    now: Arc<Mutex<DateTime<Utc>>>,
    pub signing: SigningKey,
}

impl Harness {
    pub async fn new() -> Self {
        Self::configured(|store| store, MAX_BLOCKS).await
    }

    pub async fn limited(blocks: usize) -> Self {
        Self::configured(|store| store, blocks).await
    }

    pub async fn configured<S>(wrap: impl FnOnce(Directory) -> S, blocks: usize) -> Self
    where
        S: Store + 'static,
    {
        let directory = TempDir::new().expect("a directory");
        let store = wrap(Directory::open(directory.path()).await.expect("a store"));
        let now = Arc::new(Mutex::new(start()));
        let clock = Arc::clone(&now);
        let services = Services::new(
            store,
            MemoryIndex::new(),
            AddressSigner::new(&SECRET).expect("a secret"),
            ORIGIN,
        )
        .expect("services")
        .with_block_limit(blocks)
        .with_clock(Arc::new(move || *clock.lock().expect("the clock")));
        let services = Arc::new(services);
        let (signing, _) = SigningKey::generate().expect("a key");
        let mut keys = KeySet::new();
        keys.insert(signing.public_key());
        let auth = Auth {
            keys: Arc::new(keys),
            now: Arc::clone(&now),
        };
        let twirp = services
            .twirp_router()
            .layer(middleware::from_fn_with_state(auth, authenticate));
        Self {
            app: twirp.merge(services.blob_router()),
            directory,
            now,
            signing,
        }
    }

    pub fn root(&self) -> &Path {
        self.directory.path()
    }

    pub fn advance(&self, by: Duration) {
        *self.now.lock().expect("the clock") += by;
    }

    pub fn token(&self, who: JobIdentity) -> String {
        self.signing
            .mint(who, start(), start() + Duration::hours(24))
            .expect("a token")
    }

    pub async fn store(&self) -> Directory {
        Directory::open(self.directory.path())
            .await
            .expect("a store")
    }

    pub async fn send(&self, request: Request<Body>) -> Reply {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("an infallible service");
        let (parts, body) = response.into_parts();
        let body = to_bytes(body, usize::MAX).await.expect("a body");
        Reply {
            status: parts.status,
            headers: parts.headers,
            body: body.to_vec(),
        }
    }

    pub async fn twirp(&self, token: Option<&str>, method: &str, body: &Value) -> Reply {
        let mut request = Request::post(format!(
            "/twirp/github.actions.results.api.v1.ArtifactService/{method}"
        ))
        .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.send(
            request
                .body(Body::from(body.to_string()))
                .expect("a request"),
        )
        .await
    }

    pub async fn blob(
        &self,
        method: Method,
        address: &str,
        extra: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Reply {
        let path = address.strip_prefix(ORIGIN).unwrap_or(address);
        let uri = if extra.is_empty() {
            path.to_owned()
        } else if path.contains('?') {
            format!("{path}&{extra}")
        } else {
            format!("{path}?{extra}")
        };
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        self.send(request.body(Body::from(body.to_vec())).expect("a request"))
            .await
    }

    pub async fn stage(&self, address: &str, block: &str, data: &[u8]) -> Reply {
        let extra = format!("comp=block&blockid={}", encode(block));
        self.blob(Method::PUT, address, &extra, &[], data).await
    }

    pub async fn commit(&self, address: &str, blocks: &[String]) -> Reply {
        let list: String = blocks
            .iter()
            .map(|block| format!("<Latest>{block}</Latest>"))
            .collect();
        let xml =
            format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>{list}</BlockList>");
        self.blob(Method::PUT, address, "comp=blocklist", &[], xml.as_bytes())
            .await
    }

    pub async fn create(&self, token: &str, who: JobIdentity, name: &str) -> String {
        let response = self
            .twirp(
                Some(token),
                "CreateArtifact",
                &serde_json::json!({
                    "workflow_run_backend_id": who.attempt.to_string(),
                    "workflow_job_run_backend_id": who.execution.to_string(),
                    "name": name,
                    "version": 4,
                }),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
        response.json()["signed_upload_url"]
            .as_str()
            .expect("an upload address")
            .to_owned()
    }

    pub async fn finalize(&self, token: &str, who: JobIdentity, name: &str, size: usize) {
        let response = self
            .twirp(
                Some(token),
                "FinalizeArtifact",
                &serde_json::json!({
                    "workflow_run_backend_id": who.attempt.to_string(),
                    "workflow_job_run_backend_id": who.execution.to_string(),
                    "name": name,
                    "size": size.to_string(),
                }),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
    }

    pub async fn download_address(&self, token: &str, who: JobIdentity, name: &str) -> String {
        let response = self
            .twirp(
                Some(token),
                "GetSignedArtifactURL",
                &serde_json::json!({
                    "workflow_run_backend_id": who.attempt.to_string(),
                    "workflow_job_run_backend_id": who.execution.to_string(),
                    "name": name,
                }),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.json());
        response.json()["signed_url"]
            .as_str()
            .expect("a download address")
            .to_owned()
    }

    pub async fn publish(&self, name: &str, content: &[u8]) -> (String, String) {
        let who = identity(1);
        let token = self.token(who);
        let upload = self.create(&token, who, name).await;
        let created = self
            .blob(
                Method::PUT,
                &upload,
                "",
                &[("x-ms-blob-type", "BlockBlob")],
                content,
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
        self.finalize(&token, who, name, content.len()).await;
        let download = self.download_address(&token, who, name).await;
        (upload, download)
    }
}
