mod sign;
mod wire;

use std::error::Error as _;
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, RANGE};
use reqwest::{Method, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::runtime::Handle;
use tokio_stream::StreamExt as _;
use tokio_util::io::StreamReader;

use self::sign::{Canonical, EMPTY_PAYLOAD, Signer};
use crate::key::starts_some_key;
use crate::{Error, Key, Object, Store, clip};

const PART: usize = 8 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const READ_TIMEOUT: Duration = Duration::from_secs(60);

const NO_SUCH_KEY: &str = "NoSuchKey";

const BUCKET_RULE: &str = "a bucket name is 3 to 63 lowercase letters, digits, dots and hyphens \
                           that begin and end with a letter or a digit";

#[derive(Clone)]
pub struct S3Settings {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub path_style: bool,
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Clone)]
pub struct S3 {
    client: Arc<Client>,
}

struct Client {
    http: reqwest::Client,
    base: Url,
    host: String,
    root: String,
    bucket: String,
    signer: Signer,
}

struct Call<'a> {
    method: Method,
    key: Option<&'a Key>,
    query: Vec<(&'static str, String)>,
    body: Option<Vec<u8>>,
    range: Option<String>,
}

struct Upload {
    store: S3,
    key: Key,
    id: String,
    open: bool,
}

struct Refusal {
    code: Option<String>,
    error: Error,
}

impl S3 {
    pub fn new(settings: S3Settings) -> Result<S3, Error> {
        let endpoint = Url::parse(&settings.endpoint).map_err(|error| {
            Error::Settings(format!(
                "the endpoint `{}` is not an address: {error}",
                settings.endpoint
            ))
        })?;
        let plain = matches!(endpoint.scheme(), "http" | "https")
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.path() == "/"
            && endpoint.query().is_none()
            && endpoint.fragment().is_none();
        let host = match endpoint.host_str() {
            Some(host) if plain => host.to_owned(),
            _ => {
                return Err(Error::Settings(format!(
                    "the endpoint `{}` is not an http or https address of a host, without a \
                     user, a path or a query",
                    settings.endpoint
                )));
            }
        };
        if !bucket_name(&settings.bucket) {
            return Err(Error::Settings(format!(
                "`{}` is not a bucket name: {BUCKET_RULE}",
                settings.bucket
            )));
        }
        if settings.region.is_empty() {
            return Err(Error::Settings("the region is empty".to_owned()));
        }
        if settings.access_key_id.is_empty() || settings.secret_access_key.is_empty() {
            return Err(Error::Settings(
                "the access key id and the secret access key are both needed".to_owned(),
            ));
        }
        let (base, root) = if settings.path_style {
            (endpoint, format!("/{}", settings.bucket))
        } else {
            if host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok()
            {
                return Err(Error::Settings(format!(
                    "the endpoint `{}` is an IP address, which can name the bucket only in the \
                     path: set path_style",
                    settings.endpoint
                )));
            }
            let mut base = endpoint;
            base.set_host(Some(&format!("{}.{host}", settings.bucket)))
                .map_err(|error| {
                    Error::Settings(format!(
                        "the bucket `{}` cannot lead the host `{host}`: {error}",
                        settings.bucket
                    ))
                })?;
            (base, String::new())
        };
        let host = match (base.host_str(), base.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            (None, _) => host,
        };
        let http = reqwest::Client::builder()
            .user_agent(concat!("bosun/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| Error::Settings(format!("the HTTP client cannot start: {error}")))?;
        Ok(S3 {
            client: Arc::new(Client {
                http,
                base,
                host,
                root,
                bucket: settings.bucket,
                signer: Signer::new(
                    settings.access_key_id,
                    settings.secret_access_key,
                    settings.region,
                ),
            }),
        })
    }

    pub async fn create_bucket(&self) -> Result<(), Error> {
        let bucket = &self.client.bucket;
        let response = self
            .client
            .send(bucket, Call::bucket(Method::PUT).body(Vec::new()))
            .await?;
        settled(bucket, response).await
    }

    pub async fn delete_bucket(&self) -> Result<(), Error> {
        let bucket = &self.client.bucket;
        let response = self
            .client
            .send(bucket, Call::bucket(Method::DELETE))
            .await?;
        settled(bucket, response).await
    }

    pub async fn uploads(&self) -> Result<Vec<Key>, Error> {
        let bucket = &self.client.bucket;
        let page: wire::Uploads = self
            .client
            .xml(
                bucket,
                Call::bucket(Method::GET).query(vec![("uploads", String::new())]),
            )
            .await?;
        if page.is_truncated {
            return Err(request_error(
                bucket,
                "the uploads in progress are more than one page of the listing holds".to_owned(),
            ));
        }
        Ok(page
            .upload
            .into_iter()
            .filter_map(|entry| Key::new(entry.key).ok())
            .collect())
    }

    async fn initiate(&self, key: &Key) -> Result<Upload, Error> {
        let initiated: wire::Initiated = self
            .client
            .xml(
                key.as_str(),
                Call::object(Method::POST, key)
                    .query(vec![("uploads", String::new())])
                    .body(Vec::new()),
            )
            .await?;
        Ok(Upload {
            store: self.clone(),
            key: key.clone(),
            id: initiated.upload_id,
            open: true,
        })
    }

    async fn upload_parts<R>(
        &self,
        upload: &Upload,
        first: Vec<u8>,
        body: &mut R,
    ) -> Result<(), Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        let key = &upload.key;
        let target = key.as_str();
        let mut etags = Vec::new();
        let mut next = first;
        while !next.is_empty() {
            let number = etags.len() + 1;
            let call = Call::object(Method::PUT, key)
                .query(vec![
                    ("partNumber", number.to_string()),
                    ("uploadId", upload.id.clone()),
                ])
                .body(next);
            let response = self.client.send(target, call).await?;
            if !response.status().is_success() {
                return Err(refusal(target, response).await.error);
            }
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    request_error(
                        target,
                        format!("the store answered part {number} without an ETag"),
                    )
                })?
                .to_owned();
            etags.push(etag);
            next = part(body, key).await?;
        }
        let call = Call::object(Method::POST, key)
            .query(vec![("uploadId", upload.id.clone())])
            .body(wire::completion(&etags).into_bytes());
        let response = self.client.send(target, call).await?;
        if !response.status().is_success() {
            return Err(refusal(target, response).await.error);
        }
        let text = response
            .text()
            .await
            .map_err(|error| request_error(target, transport(error)))?;
        match quick_xml::de::from_str::<wire::Failure>(&text) {
            Ok(failure) => Err(request_error(
                target,
                format!(
                    "the store did not complete the upload: {}: {}",
                    failure.code, failure.message
                ),
            )),
            Err(_) => Ok(()),
        }
    }

    async fn abort(&self, key: &Key, id: &str) -> Result<(), Error> {
        let call = Call::object(Method::DELETE, key).query(vec![("uploadId", id.to_owned())]);
        let response = self.client.send(key.as_str(), call).await?;
        settled(key.as_str(), response).await
    }

    async fn sized(&self, key: &Key, bounds: (Bound<u64>, Bound<u64>)) -> Result<Object, Error> {
        let target = key.as_str();
        let response = self
            .client
            .send(target, Call::object(Method::HEAD, key))
            .await?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(Error::NotFound(key.clone()));
        }
        if !status.is_success() {
            return Err(request_error(
                target,
                format!("the store answered {status}"),
            ));
        }
        let size = content_length(&response).ok_or_else(|| sizeless(target))?;
        if clip(key, bounds, size)?.is_empty() {
            Ok(Object::new(size, tokio::io::empty()))
        } else {
            Err(request_error(
                target,
                "the object changed while it was read".to_owned(),
            ))
        }
    }
}

impl Store for S3 {
    async fn put<R>(&self, key: &Key, mut body: R) -> Result<(), Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        let first = part(&mut body, key).await?;
        if first.len() < PART {
            let response = self
                .client
                .send(key.as_str(), Call::object(Method::PUT, key).body(first))
                .await?;
            return settled(key.as_str(), response).await;
        }
        let upload = self.initiate(key).await?;
        let finished = self.upload_parts(&upload, first, &mut body).await;
        upload.settle(finished).await
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, Error>
    where
        B: RangeBounds<u64> + Send,
    {
        let bounds = (range.start_bound().cloned(), range.end_bound().cloned());
        let start = match bounds.0 {
            Bound::Included(start) => Some(start),
            Bound::Excluded(start) => start.checked_add(1),
            Bound::Unbounded => Some(0),
        };
        let end = match bounds.1 {
            Bound::Included(end) => end.checked_add(1),
            Bound::Excluded(end) => Some(end),
            Bound::Unbounded => None,
        };
        let header = match (start, end) {
            (Some(0), None) => None,
            (Some(start), None) => Some(format!("bytes={start}-")),
            (Some(start), Some(end)) if start < end => Some(format!("bytes={start}-{}", end - 1)),
            _ => return self.sized(key, bounds).await,
        };
        let target = key.as_str();
        let ranged = header.is_some();
        let response = self
            .client
            .send(target, Call::object(Method::GET, key).range(header))
            .await?;
        match response.status() {
            StatusCode::OK if !ranged => {
                let size = content_length(&response).ok_or_else(|| sizeless(target))?;
                Ok(Object::new(size, reader(response)))
            }
            StatusCode::PARTIAL_CONTENT if ranged => match content_range(&response) {
                Some((first, size)) if Some(first) == start => {
                    Ok(Object::new(size, reader(response)))
                }
                _ => Err(request_error(
                    target,
                    "the store answered another range than the one asked for".to_owned(),
                )),
            },
            StatusCode::RANGE_NOT_SATISFIABLE => self.sized(key, bounds).await,
            _ => {
                let refusal = refusal(target, response).await;
                if refusal.code.as_deref() == Some(NO_SUCH_KEY) {
                    Err(Error::NotFound(key.clone()))
                } else {
                    Err(refusal.error)
                }
            }
        }
    }

    async fn delete(&self, key: &Key) -> Result<(), Error> {
        let target = key.as_str();
        let response = self
            .client
            .send(target, Call::object(Method::DELETE, key))
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        let refusal = refusal(target, response).await;
        if refusal.code.as_deref() == Some(NO_SUCH_KEY) {
            Ok(())
        } else {
            Err(refusal.error)
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, Error> {
        if !starts_some_key(prefix) {
            return Ok(Vec::new());
        }
        let mut keys = Vec::new();
        let mut token = None;
        loop {
            let mut query = vec![("list-type", "2".to_owned())];
            if !prefix.is_empty() {
                query.push(("prefix", prefix.to_owned()));
            }
            if let Some(token) = token.take() {
                query.push(("continuation-token", token));
            }
            let page: wire::Listing = self
                .client
                .xml(prefix, Call::bucket(Method::GET).query(query))
                .await?;
            keys.extend(
                page.contents
                    .into_iter()
                    .filter_map(|entry| Key::new(entry.key).ok()),
            );
            match (page.is_truncated, page.next_continuation_token) {
                (false, _) => break,
                (true, Some(next)) => token = Some(next),
                (true, None) => {
                    return Err(request_error(
                        prefix,
                        "the store cut a listing short without a continuation token".to_owned(),
                    ));
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}

impl fmt::Debug for S3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3")
            .field("base", &self.client.base.as_str())
            .field("bucket", &self.client.bucket)
            .finish_non_exhaustive()
    }
}

impl Client {
    fn locate(&self, key: Option<&Key>, query: &[(&str, String)]) -> (Url, String, String) {
        let path = match key {
            Some(key) => format!("{}/{}", self.root, sign::encode(key.as_str(), true)),
            None if self.root.is_empty() => "/".to_owned(),
            None => self.root.clone(),
        };
        let query = sign::query(query);
        let mut url = self.base.clone();
        url.set_path(&path);
        url.set_query((!query.is_empty()).then_some(query.as_str()));
        (url, path, query)
    }

    async fn send(&self, target: &str, call: Call<'_>) -> Result<Response, Error> {
        let (url, path, query) = self.locate(call.key, &call.query);
        let payload = call
            .body
            .as_deref()
            .map_or_else(|| EMPTY_PAYLOAD.to_owned(), sign::digest);
        let signature = self.signer.sign(
            &Canonical {
                method: call.method.as_str(),
                host: &self.host,
                path: &path,
                query: &query,
                payload: &payload,
            },
            Utc::now(),
        );
        let mut request = self
            .http
            .request(call.method, url)
            .header("x-amz-date", signature.date)
            .header("x-amz-content-sha256", payload)
            .header(AUTHORIZATION, signature.authorization);
        if let Some(range) = call.range {
            request = request.header(RANGE, range);
        }
        if let Some(body) = call.body {
            request = request.body(body);
        }
        request
            .send()
            .await
            .map_err(|error| request_error(target, transport(error)))
    }

    async fn xml<T: DeserializeOwned>(&self, target: &str, call: Call<'_>) -> Result<T, Error> {
        let response = self.send(target, call).await?;
        if !response.status().is_success() {
            return Err(refusal(target, response).await.error);
        }
        let text = response
            .text()
            .await
            .map_err(|error| request_error(target, transport(error)))?;
        quick_xml::de::from_str(&text).map_err(|error| {
            request_error(
                target,
                format!("the store answered with XML that is not the answer asked for: {error}"),
            )
        })
    }
}

impl<'a> Call<'a> {
    fn object(method: Method, key: &'a Key) -> Call<'a> {
        Call {
            method,
            key: Some(key),
            query: Vec::new(),
            body: None,
            range: None,
        }
    }

    fn bucket(method: Method) -> Call<'a> {
        Call {
            method,
            key: None,
            query: Vec::new(),
            body: None,
            range: None,
        }
    }

    fn query(mut self, query: Vec<(&'static str, String)>) -> Call<'a> {
        self.query = query;
        self
    }

    fn body(mut self, body: Vec<u8>) -> Call<'a> {
        self.body = Some(body);
        self
    }

    fn range(mut self, range: Option<String>) -> Call<'a> {
        self.range = range;
        self
    }
}

impl Upload {
    async fn settle(mut self, finished: Result<(), Error>) -> Result<(), Error> {
        if finished.is_err() {
            let _ = self.store.abort(&self.key, &self.id).await;
        }
        self.open = false;
        finished
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        if let Ok(runtime) = Handle::try_current() {
            let store = self.store.clone();
            let key = self.key.clone();
            let id = std::mem::take(&mut self.id);
            runtime.spawn(async move {
                let _ = store.abort(&key, &id).await;
            });
        }
    }
}

async fn part<R>(body: &mut R, key: &Key) -> Result<Vec<u8>, Error>
where
    R: AsyncRead + Unpin,
{
    let mut part = Vec::new();
    body.take(PART as u64)
        .read_to_end(&mut part)
        .await
        .map_err(|source| Error::Io {
            target: key.to_string(),
            source,
        })?;
    Ok(part)
}

async fn settled(target: &str, response: Response) -> Result<(), Error> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(refusal(target, response).await.error)
    }
}

async fn refusal(target: &str, response: Response) -> Refusal {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let failure = quick_xml::de::from_str::<wire::Failure>(&text).ok();
    let problem = match &failure {
        Some(failure) if failure.message.is_empty() => {
            format!("the store answered {status} with {}", failure.code)
        }
        Some(failure) => format!(
            "the store answered {status} with {}: {}",
            failure.code, failure.message
        ),
        None => format!("the store answered {status}"),
    };
    Refusal {
        code: failure.map(|failure| failure.code),
        error: request_error(target, problem),
    }
}

fn reader(response: Response) -> impl AsyncRead + Send + 'static {
    StreamReader::new(
        response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| io::Error::other(transport(error)))),
    )
}

fn content_length(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn content_range(response: &Response) -> Option<(u64, u64)> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let (range, size) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (first, _) = range.split_once('-')?;
    Some((first.parse().ok()?, size.parse().ok()?))
}

fn sizeless(target: &str) -> Error {
    request_error(
        target,
        "the store answered without the size of the object".to_owned(),
    )
}

fn transport(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn request_error(target: &str, problem: String) -> Error {
    Error::Request {
        target: target.to_owned(),
        problem,
    }
}

fn bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
        && name.starts_with(|character: char| character.is_ascii_alphanumeric())
        && name.ends_with(|character: char| character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{S3, S3Settings};
    use crate::{Error, Key};

    fn settings(endpoint: &str, path_style: bool) -> S3Settings {
        S3Settings {
            endpoint: endpoint.to_owned(),
            bucket: "bosun".to_owned(),
            region: "eu-central-1".to_owned(),
            path_style,
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
        }
    }

    fn located(
        store: &S3,
        key: Option<&str>,
        query: &[(&str, String)],
    ) -> (String, String, String) {
        let key = key.map(|key| Key::new(key).unwrap());
        let (url, path, query) = store.client.locate(key.as_ref(), query);
        (url.to_string(), path, query)
    }

    #[test]
    fn virtual_hosted_addressing_puts_the_bucket_in_the_host() {
        let store = S3::new(settings("https://s3.example.org", false)).unwrap();
        assert_eq!(store.client.host, "bosun.s3.example.org");
        assert_eq!(
            located(&store, Some("logs/a b/c+d"), &[]),
            (
                "https://bosun.s3.example.org/logs/a%20b/c%2Bd".to_owned(),
                "/logs/a%20b/c%2Bd".to_owned(),
                String::new(),
            )
        );
        assert_eq!(
            located(
                &store,
                None,
                &[
                    ("list-type", "2".to_owned()),
                    ("prefix", "logs/".to_owned())
                ]
            ),
            (
                "https://bosun.s3.example.org/?list-type=2&prefix=logs%2F".to_owned(),
                "/".to_owned(),
                "list-type=2&prefix=logs%2F".to_owned(),
            )
        );
    }

    #[test]
    fn path_style_addressing_puts_the_bucket_in_the_path() {
        let store = S3::new(settings("http://127.0.0.1:9000/", true)).unwrap();
        assert_eq!(store.client.host, "127.0.0.1:9000");
        assert_eq!(
            located(&store, Some("a/%41"), &[("uploads", String::new())]),
            (
                "http://127.0.0.1:9000/bosun/a/%2541?uploads=".to_owned(),
                "/bosun/a/%2541".to_owned(),
                "uploads=".to_owned(),
            )
        );
        assert_eq!(located(&store, None, &[]).1, "/bosun");
        let store = S3::new(settings("https://s3.example.org:443", true)).unwrap();
        assert_eq!(store.client.host, "s3.example.org");
    }

    #[test]
    fn settings_the_backend_cannot_use_are_refused_with_the_reason() {
        let cases = [
            (settings("s3.example.org", true), "is not an address"),
            (
                settings("ftp://s3.example.org", true),
                "is not an http or https address",
            ),
            (
                settings("https://s3.example.org/bosun", true),
                "without a user, a path or a query",
            ),
            (
                settings("https://key@s3.example.org", true),
                "without a user, a path or a query",
            ),
            (
                settings("https://s3.example.org/?a=b", true),
                "without a user, a path or a query",
            ),
            (settings("http://127.0.0.1:9000", false), "is an IP address"),
            (settings("http://[::1]:9000", false), "is an IP address"),
            (
                S3Settings {
                    bucket: "Bosun".to_owned(),
                    ..settings("https://s3.example.org", true)
                },
                "`Bosun` is not a bucket name",
            ),
            (
                S3Settings {
                    bucket: "-bosun".to_owned(),
                    ..settings("https://s3.example.org", true)
                },
                "`-bosun` is not a bucket name",
            ),
            (
                S3Settings {
                    bucket: "b".repeat(64),
                    ..settings("https://s3.example.org", true)
                },
                "is not a bucket name",
            ),
            (
                S3Settings {
                    region: String::new(),
                    ..settings("https://s3.example.org", true)
                },
                "the region is empty",
            ),
            (
                S3Settings {
                    secret_access_key: String::new(),
                    ..settings("https://s3.example.org", true)
                },
                "the access key id and the secret access key are both needed",
            ),
        ];
        for (settings, expected) in cases {
            let endpoint = settings.endpoint.clone();
            match S3::new(settings) {
                Err(Error::Settings(problem)) => {
                    assert!(problem.contains(expected), "{endpoint}: {problem}");
                }
                other => panic!("{endpoint} was not refused with `{expected}`: {other:?}"),
            }
        }
    }
}
