use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use bosun_blob::{Key, Store};
use serde::Deserialize;
use serde::de::IgnoredAny;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::{ReaderStream, StreamReader};

use crate::Services;
use crate::index::{Artifact, ArtifactId, Index, IndexError};
use crate::signed::Purpose;

pub const MAX_BLOCKS: usize = 50_000;
pub const MAX_BLOCK_ID_BYTES: usize = 88;
const MAX_BLOCK_LIST_BYTES: usize = MAX_BLOCKS * (MAX_BLOCK_ID_BYTES + 32);
const CONTAINER: &str = "artifacts";
const CHUNK_BYTES: usize = 64 * 1024;
const BLOB_TYPE: HeaderName = HeaderName::from_static("x-ms-blob-type");
const MS_RANGE: HeaderName = HeaderName::from_static("x-ms-range");
const MS_ERROR_CODE: HeaderName = HeaderName::from_static("x-ms-error-code");

pub(crate) fn object(id: ArtifactId) -> String {
    format!("{CONTAINER}/{id}")
}

fn block_prefix(id: ArtifactId) -> String {
    format!("blocks/{id}/")
}

fn block_key(id: ArtifactId, block: &str) -> Result<Key, BlobError> {
    let hex: String = block.bytes().map(|byte| format!("{byte:02x}")).collect();
    Key::new(format!("blocks/{id}/{hex}"))
        .map_err(|error| BlobError::internal("a block key", &error))
}

fn download_name(name: &str) -> &str {
    match name.rsplit(['/', '\\']).next() {
        Some("" | "." | "..") | None => "artifact",
        Some(last) => last,
    }
}

fn content_disposition(name: &str) -> String {
    let name = download_name(name);
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

fn valid_block_id(block: &str) -> bool {
    !block.is_empty()
        && block.len() <= MAX_BLOCK_ID_BYTES
        && block
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

struct BlobError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl BlobError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "AuthenticationFailed",
            "the signed address is invalid, altered, expired or for another use",
        )
    }

    fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "BlobNotFound",
            "the blob does not exist",
        )
    }

    fn internal(context: &str, error: &dyn std::fmt::Display) -> Self {
        tracing::error!(%error, "{context}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            "the blob service failed",
        )
    }

    fn store(context: &str, error: &bosun_blob::Error) -> Self {
        match error {
            bosun_blob::Error::NotFound(_) => Self::not_found(),
            other => Self::internal(context, other),
        }
    }

    fn index(error: &IndexError) -> Self {
        Self::internal("the artifact index failed", error)
    }
}

impl IntoResponse for BlobError {
    fn into_response(self) -> Response {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><Error><Code>{}</Code><Message>{}</Message></Error>",
            self.code, self.message
        );
        let mut response = (self.status, body).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/xml"),
        );
        headers.insert(MS_ERROR_CODE, HeaderValue::from_static(self.code));
        response
    }
}

pub(crate) fn router<S, I>(services: Arc<Services<S, I>>) -> Router
where
    S: Store + 'static,
    I: Index + 'static,
{
    Router::new()
        .route("/_runtime/blob/{container}/{blob}", any(handle::<S, I>))
        .with_state(services)
}

async fn handle<S, I>(
    State(services): State<Arc<Services<S, I>>>,
    Path((container, blob)): Path<(String, String)>,
    request: Request,
) -> Response
where
    S: Store + 'static,
    I: Index + 'static,
{
    match serve(&services, &container, &blob, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

struct Params {
    pairs: Vec<(String, String)>,
}

impl Params {
    fn new(query: Option<&str>) -> Self {
        let pairs = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        Self { pairs }
    }

    fn get(&self, key: &str) -> Result<Option<&str>, BlobError> {
        let mut found = self.pairs.iter().filter(|(name, _)| name == key);
        match (found.next(), found.next()) {
            (None, _) => Ok(None),
            (Some((_, value)), None) => Ok(Some(value)),
            (Some(_), Some(_)) => Err(BlobError::bad(
                "InvalidQueryParameterValue",
                "a query parameter is given twice",
            )),
        }
    }
}

async fn serve<S, I>(
    services: &Services<S, I>,
    container: &str,
    blob: &str,
    request: Request,
) -> Result<Response, BlobError>
where
    S: Store + 'static,
    I: Index + 'static,
{
    let (parts, body) = request.into_parts();
    let purpose = match parts.method {
        Method::GET | Method::HEAD => Purpose::Download,
        Method::PUT => Purpose::Upload,
        _ => {
            return Err(BlobError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "UnsupportedHttpVerb",
                "only GET, HEAD and PUT are served",
            ));
        }
    };
    let params = Params::new(parts.uri.query());
    services
        .signer
        .verify(
            purpose,
            &format!("{container}/{blob}"),
            params.get("expires").ok().flatten(),
            params.get("sig").ok().flatten(),
            services.now(),
        )
        .map_err(|_| BlobError::forbidden())?;
    let id = (container == CONTAINER)
        .then(|| blob.parse::<ArtifactId>().ok())
        .flatten()
        .ok_or_else(BlobError::not_found)?;
    let artifact = services
        .index
        .artifact(id)
        .await
        .map_err(|error| BlobError::index(&error))?
        .ok_or_else(BlobError::not_found)?;
    match purpose {
        Purpose::Download => {
            services
                .download(&artifact, &parts.method, &parts.headers, &params)
                .await
        }
        Purpose::Upload => {
            services
                .upload(&artifact, &parts.headers, &params, body)
                .await
        }
    }
}

fn reader(body: Body) -> impl AsyncRead + Send + Unpin {
    StreamReader::new(
        body.into_data_stream()
            .map(|chunk| chunk.map_err(io::Error::other)),
    )
}

fn created() -> Response {
    StatusCode::CREATED.into_response()
}

#[derive(Deserialize)]
#[serde(rename = "BlockList")]
struct BlockList {
    #[serde(rename = "$value", default)]
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
enum Entry {
    Latest(String),
    Uncommitted(String),
    Committed(IgnoredAny),
}

fn block_list(bytes: &[u8]) -> Result<Vec<String>, BlobError> {
    let invalid = |message: &str| BlobError::bad("InvalidXmlDocument", message.to_owned());
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("the block list is not UTF-8"))?;
    let list: BlockList = quick_xml::de::from_str(text)
        .map_err(|_| invalid("the block list is not a BlockList document"))?;
    if list.entries.len() > MAX_BLOCKS {
        return Err(BlobError::bad(
            "InvalidBlockList",
            format!("a block list names at most {MAX_BLOCKS} blocks"),
        ));
    }
    let mut seen = HashSet::new();
    let mut blocks = Vec::with_capacity(list.entries.len());
    for entry in list.entries {
        let block = match entry {
            Entry::Latest(block) | Entry::Uncommitted(block) => block,
            Entry::Committed(_) => {
                return Err(BlobError::bad(
                    "InvalidBlockList",
                    "only staged blocks can be committed",
                ));
            }
        };
        if !valid_block_id(&block) {
            return Err(BlobError::bad(
                "InvalidBlockList",
                "a block id is not valid",
            ));
        }
        if !seen.insert(block.clone()) {
            return Err(BlobError::bad("InvalidBlockList", "a block is named twice"));
        }
        blocks.push(block);
    }
    Ok(blocks)
}

#[derive(Clone, Copy)]
enum Wanted {
    From(u64),
    Between(u64, u64),
    Last(u64),
}

fn wanted(headers: &HeaderMap) -> Result<Option<Wanted>, BlobError> {
    let invalid = || BlobError::bad("InvalidRange", "the range is not a single byte range");
    let Some(value) = headers
        .get(&MS_RANGE)
        .or_else(|| headers.get(header::RANGE))
    else {
        return Ok(None);
    };
    let text = value.to_str().map_err(|_| invalid())?;
    let (unit, spec) = text.split_once('=').ok_or_else(invalid)?;
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Err(invalid());
    }
    let number = |digits: &str| -> Result<u64, BlobError> {
        if digits.bytes().all(|byte| byte.is_ascii_digit()) {
            digits.parse().map_err(|_| invalid())
        } else {
            Err(invalid())
        }
    };
    let (first, last) = spec.trim().split_once('-').ok_or_else(invalid)?;
    match (first, last) {
        ("", "") => Err(invalid()),
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

fn resolve(wanted: Wanted, size: u64) -> Option<(u64, u64)> {
    let last = size.checked_sub(1)?;
    match wanted {
        Wanted::From(first) if first <= last => Some((first, last)),
        Wanted::Between(first, end) if first <= last => Some((first, end.min(last))),
        Wanted::Last(count) if count > 0 => Some((size.saturating_sub(count), last)),
        _ => None,
    }
}

fn unsatisfiable(size: u64) -> Result<Response, BlobError> {
    let mut response = BlobError::new(
        StatusCode::RANGE_NOT_SATISFIABLE,
        "InvalidRange",
        "the range lies outside the blob",
    )
    .into_response();
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{size}"))
            .map_err(|error| BlobError::internal("a header value", &error))?,
    );
    Ok(response)
}

fn number_header(value: u64) -> Result<HeaderValue, BlobError> {
    HeaderValue::from_str(&value.to_string())
        .map_err(|error| BlobError::internal("a header value", &error))
}

impl<S, I> Services<S, I>
where
    S: Store + 'static,
    I: Index + 'static,
{
    pub(crate) async fn clear_blocks(&self, id: ArtifactId) -> Result<(), bosun_blob::Error> {
        for key in self.store.list(&block_prefix(id)).await? {
            self.store.delete(&key).await?;
        }
        Ok(())
    }

    async fn download(
        &self,
        artifact: &Artifact,
        method: &Method,
        headers: &HeaderMap,
        params: &Params,
    ) -> Result<Response, BlobError> {
        if artifact.finalized.is_none() {
            return Err(BlobError::not_found());
        }
        if params.get("comp")?.is_some() {
            return Err(BlobError::bad(
                "UnsupportedQueryParameter",
                "only the whole blob is served",
            ));
        }
        let size = self
            .store
            .get(&artifact.key, 0..0)
            .await
            .map_err(|error| BlobError::store("an artifact cannot be read", &error))?
            .size();
        let requested = wanted(headers)?;
        let window = match requested {
            None => size.checked_sub(1).map(|last| (0, last)),
            Some(wanted) => match resolve(wanted, size) {
                Some(window) => Some(window),
                None => return unsatisfiable(size),
            },
        };
        let length = window.map_or(0, |(first, last)| last - first + 1);
        let body = match window {
            Some((first, last)) if *method != Method::HEAD => {
                let object = self
                    .store
                    .get(&artifact.key, first..=last)
                    .await
                    .map_err(|error| BlobError::store("an artifact cannot be read", &error))?;
                Body::from_stream(ReaderStream::with_capacity(object, CHUNK_BYTES))
            }
            _ => Body::empty(),
        };
        let mut response = Response::new(body);
        if requested.is_some() {
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        }
        let out = response.headers_mut();
        out.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&artifact.content_type)
                .map_err(|error| BlobError::internal("a content type", &error))?,
        );
        out.insert(header::CONTENT_LENGTH, number_header(length)?);
        out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        out.insert(BLOB_TYPE, HeaderValue::from_static("BlockBlob"));
        out.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&content_disposition(&artifact.name))
                .map_err(|error| BlobError::internal("a content disposition", &error))?,
        );
        out.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        out.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        );
        out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let (Some(_), Some((first, last))) = (requested, window) {
            out.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {first}-{last}/{size}"))
                    .map_err(|error| BlobError::internal("a header value", &error))?,
            );
        }
        Ok(response)
    }

    async fn upload(
        &self,
        artifact: &Artifact,
        headers: &HeaderMap,
        params: &Params,
        body: Body,
    ) -> Result<Response, BlobError> {
        if artifact.finalized.is_some() {
            return Err(BlobError::new(
                StatusCode::CONFLICT,
                "ArtifactFinalized",
                "the artifact is finalised and takes no more content",
            ));
        }
        let comp = params.get("comp")?;
        let block = params.get("blockid")?;
        let block_blob = headers
            .get(&BLOB_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("BlockBlob"));
        match (comp, block) {
            (Some("block"), Some(block)) => self.stage(artifact, block, body).await,
            (Some("blocklist"), None) => self.commit(artifact, body).await,
            (None, None) if block_blob => {
                self.store
                    .put(&artifact.key, reader(body))
                    .await
                    .map_err(|error| BlobError::store("an upload cannot be stored", &error))?;
                Ok(created())
            }
            _ => Err(BlobError::bad(
                "UnsupportedRequest",
                "only Put Block, Put Block List and Put Blob of a block blob are served",
            )),
        }
    }

    async fn stage(
        &self,
        artifact: &Artifact,
        block: &str,
        body: Body,
    ) -> Result<Response, BlobError> {
        if !valid_block_id(block) {
            return Err(BlobError::bad(
                "InvalidQueryParameterValue",
                "blockid is not a base64 block id of at most 88 characters",
            ));
        }
        let key = block_key(artifact.id, block)?;
        match self
            .index
            .register_block(artifact.id, block, self.max_blocks)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return Err(BlobError::new(
                    StatusCode::CONFLICT,
                    "BlockCountExceedsLimit",
                    format!(
                        "an artifact stages at most {} distinct blocks",
                        self.max_blocks
                    ),
                ));
            }
            Err(IndexError::NotFound) => return Err(BlobError::not_found()),
            Err(error) => return Err(BlobError::index(&error)),
        }
        self.store
            .put(&key, reader(body))
            .await
            .map_err(|error| BlobError::store("a block cannot be stored", &error))?;
        Ok(created())
    }

    async fn commit(&self, artifact: &Artifact, body: Body) -> Result<Response, BlobError> {
        let bytes = to_bytes(body, MAX_BLOCK_LIST_BYTES).await.map_err(|_| {
            BlobError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "RequestBodyTooLarge",
                "the block list is too large",
            )
        })?;
        let blocks = block_list(&bytes)?;
        let staged: HashSet<Key> = self
            .store
            .list(&block_prefix(artifact.id))
            .await
            .map_err(|error| BlobError::store("staged blocks cannot be listed", &error))?
            .into_iter()
            .collect();
        let keys = blocks
            .iter()
            .map(|block| block_key(artifact.id, block))
            .collect::<Result<Vec<_>, _>>()?;
        if keys.iter().any(|key| !staged.contains(key)) {
            return Err(BlobError::bad(
                "InvalidBlockList",
                "the block list names a block that was never staged",
            ));
        }
        self.assemble(&artifact.key, &keys).await?;
        Ok(created())
    }

    async fn assemble(&self, target: &Key, blocks: &[Key]) -> Result<(), BlobError> {
        let (sender, receiver) = mpsc::channel::<io::Result<Bytes>>(2);
        let producer = async {
            for key in blocks {
                let mut object = match self.store.get(key, ..).await {
                    Ok(object) => object,
                    Err(error) => {
                        let _ = sender.send(Err(io::Error::other(error.to_string()))).await;
                        return Err(error);
                    }
                };
                let mut buffer = vec![0u8; CHUNK_BYTES];
                loop {
                    match object.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(read) => {
                            if sender
                                .send(Ok(Bytes::copy_from_slice(&buffer[..read])))
                                .await
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                        Err(error) => {
                            let failure = io::Error::new(error.kind(), error.to_string());
                            let _ = sender.send(Err(failure)).await;
                            return Err(bosun_blob::Error::Io {
                                target: key.to_string(),
                                source: error,
                            });
                        }
                    }
                }
            }
            drop(sender);
            Ok(())
        };
        let consumer = self
            .store
            .put(target, StreamReader::new(ReceiverStream::new(receiver)));
        let (produced, stored) = tokio::join!(producer, consumer);
        stored.map_err(|error| BlobError::internal("an object cannot be assembled", &error))?;
        produced.map_err(|error| BlobError::internal("a block cannot be read", &error))
    }
}
