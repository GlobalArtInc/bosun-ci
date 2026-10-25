mod cache;
mod key;
mod wire;

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bosun_types::api::{Annotation, AnnotationKind};
use bosun_types::status::Conclusion;
use chrono::{DateTime, TimeDelta, Utc};
use reqwest::header::{
    ACCEPT, AsHeaderName, ETAG, HeaderMap, HeaderValue, IF_NONE_MATCH, LINK, RETRY_AFTER,
};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio_stream::StreamExt;
use tokio_util::io::StreamReader;
use url::Url;

use self::cache::{Cache, Holder, Key, Stored};
use self::key::AppKey;
use crate::{
    Access, Account, App, CheckRun, CheckStatus, CommitStatus, Comparison, Download, Entry, Error,
    File, Forge, InstallationToken, InstalledRepository, Permissions, Repository, StatusState,
};

const JSON: &str = "application/vnd.github+json";

const SHA: &str = "application/vnd.github.sha";

const API_VERSION: &str = "2022-11-28";

const PAGE_SIZE: &str = "100";

const COMPARE_FILES: usize = 300;

const PULL_REQUEST_FILES: usize = 3000;

const TOKEN_MARGIN: TimeDelta = TimeDelta::minutes(5);

const SECONDARY_WAIT: TimeDelta = TimeDelta::minutes(1);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const READ_TIMEOUT: Duration = Duration::from_secs(60);

const MESSAGE_LIMIT: usize = 200;

const NO_FILE: &str = ".github";

const ANNOTATION_TITLE: usize = 255;

const ANNOTATION_MESSAGE: usize = 65_535;

type Slot = Arc<tokio::sync::Mutex<Option<InstallationToken>>>;

pub struct GitHubApp {
    id: u64,
    key: AppKey,
    api: Url,
    http: reqwest::Client,
    installations: Mutex<HashMap<u64, Slot>>,
    cache: Cache,
}

#[derive(Clone, Copy)]
enum Credential<'a> {
    App,
    Installation(u64),
    Anonymous,
    Token(&'a str),
}

impl Credential<'_> {
    fn holder(self) -> Option<Holder> {
        match self {
            Credential::App => Some(Holder::App),
            Credential::Installation(installation) => Some(Holder::Installation(installation)),
            Credential::Anonymous => Some(Holder::Anonymous),
            Credential::Token(_) => None,
        }
    }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
    next: Option<String>,
}

impl Reply {
    fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        if !self.status.is_success() {
            return Err(self.failure());
        }
        serde_json::from_slice(&self.body).map_err(|error| Error::Body(error.to_string()))
    }

    fn done(&self) -> Result<(), Error> {
        if self.status.is_success() {
            Ok(())
        } else {
            Err(self.failure())
        }
    }

    fn failure(&self) -> Error {
        failure(self.status, &self.body)
    }
}

impl GitHubApp {
    pub fn new(id: u64, private_key: &[u8], api_url: &Url) -> Result<GitHubApp, Error> {
        if !matches!(api_url.scheme(), "http" | "https") || api_url.cannot_be_a_base() {
            return Err(Error::Config(format!(
                "the forge's API address `{api_url}` is not an http or https address"
            )));
        }
        let key = AppKey::parse(private_key)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(API_VERSION),
        );
        let http = reqwest::Client::builder()
            .user_agent(concat!("bosun/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| Error::Config(format!("the HTTP client cannot start: {error}")))?;
        Ok(GitHubApp {
            id,
            key,
            api: api_url.clone(),
            http,
            installations: Mutex::default(),
            cache: Cache::default(),
        })
    }

    fn url(&self, segments: &[&str], path: &str) -> Url {
        let mut url = self.api.clone();
        if let Ok(mut pushed) = url.path_segments_mut() {
            pushed.pop_if_empty();
            pushed.extend(segments);
            pushed.extend(path.split('/').filter(|part| !part.is_empty()));
        }
        url
    }

    fn repository_url(&self, repository: &Repository, segments: &[&str], path: &str) -> Url {
        self.repository_path(&repository.owner, &repository.name, segments, path)
    }

    fn repository_path(&self, owner: &str, name: &str, segments: &[&str], path: &str) -> Url {
        let mut all = vec!["repos", owner, name];
        all.extend_from_slice(segments);
        self.url(&all, path)
    }

    fn page(&self, next: &str) -> Result<Url, Error> {
        let url = Url::parse(next).map_err(|error| {
            Error::Body(format!("the next page `{next}` is not an address: {error}"))
        })?;
        let base = self.api.path().trim_end_matches('/');
        let inside = url.origin() == self.api.origin()
            && url
                .path()
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('/'));
        if inside {
            Ok(url)
        } else {
            Err(Error::Body(format!(
                "the next page `{next}` is outside the forge's API"
            )))
        }
    }

    async fn request(
        &self,
        credential: Credential<'_>,
        method: Method,
        url: Url,
        accept: &'static str,
        body: Option<&Value>,
    ) -> Result<Reply, Error> {
        let secret = match credential {
            Credential::App => Some(self.key.sign(self.id, Utc::now())?),
            Credential::Installation(installation) => {
                Some(self.installation_token(installation).await?)
            }
            Credential::Anonymous => None,
            Credential::Token(token) => Some(token.to_owned()),
        };
        self.send(credential.holder(), secret, method, url, accept, body)
            .await
    }

    async fn send(
        &self,
        holder: Option<Holder>,
        secret: Option<String>,
        method: Method,
        url: Url,
        accept: &'static str,
        body: Option<&Value>,
    ) -> Result<Reply, Error> {
        let key = holder
            .filter(|_| method == Method::GET)
            .map(|holder| Key::new(holder, accept, &url));
        let cached = key.as_ref().and_then(|key| self.cache.get(key));
        let mut request = self.http.request(method, url).header(ACCEPT, accept);
        if let Some(secret) = secret {
            request = request.bearer_auth(secret);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(stored) = &cached {
            request = request.header(IF_NONE_MATCH, stored.etag.clone());
        }
        let response = request.send().await.map_err(Error::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED
            && let Some(stored) = cached
        {
            return Ok(Reply {
                status: StatusCode::OK,
                body: stored.body.to_vec(),
                next: stored.next,
            });
        }
        if let Some(reset) = limited(status, response.headers(), Utc::now()) {
            return Err(Error::RateLimited { reset });
        }
        let etag = response.headers().get(ETAG).cloned();
        let next = next_page(response.headers());
        let body = response.bytes().await.map_err(Error::Transport)?.to_vec();
        if status == StatusCode::OK
            && let (Some(key), Some(etag)) = (key, etag)
        {
            self.cache.put(
                key,
                Stored {
                    etag,
                    body: body.as_slice().into(),
                    next: next.clone(),
                },
            );
        }
        Ok(Reply { status, body, next })
    }

    async fn installation_token(&self, installation: u64) -> Result<String, Error> {
        let slot = Arc::clone(
            self.installations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(installation)
                .or_default(),
        );
        let mut slot = slot.lock().await;
        if let Some(token) = slot.as_ref()
            && token.expires_at - Utc::now() > TOKEN_MARGIN
        {
            return Ok(token.token.clone());
        }
        let token = self.access_token(installation, &json!({})).await?;
        let secret = token.token.clone();
        *slot = Some(token);
        Ok(secret)
    }

    async fn access_token(
        &self,
        installation: u64,
        scope: &Value,
    ) -> Result<InstallationToken, Error> {
        let jwt = self.key.sign(self.id, Utc::now())?;
        let installation = installation.to_string();
        let url = self.url(
            &["app", "installations", &installation, "access_tokens"],
            "",
        );
        let reply = self
            .send(None, Some(jwt), Method::POST, url, JSON, Some(scope))
            .await?;
        let token: wire::AccessToken = reply.json()?;
        Ok(InstallationToken {
            token: token.token,
            expires_at: token.expires_at,
        })
    }

    async fn blob(&self, repository: &Repository, sha: &str) -> Result<Vec<u8>, Error> {
        let url = self.repository_url(repository, &["git", "blobs", sha], "");
        let reply = self
            .request(
                Credential::Installation(repository.installation),
                Method::GET,
                url,
                JSON,
                None,
            )
            .await?;
        let blob: wire::Blob = reply.json()?;
        if blob.encoding != "base64" {
            return Err(Error::Body(format!(
                "the blob {sha} comes in the encoding `{}`",
                blob.encoding
            )));
        }
        decode(&blob.content)
    }
}

impl Forge for GitHubApp {
    async fn app(&self) -> Result<App, Error> {
        let reply = self
            .request(
                Credential::App,
                Method::GET,
                self.url(&["app"], ""),
                JSON,
                None,
            )
            .await?;
        let app: wire::App = reply.json()?;
        let login = format!("{}[bot]", app.slug);
        let reply = self
            .request(
                Credential::Anonymous,
                Method::GET,
                self.url(&["users", &login], ""),
                JSON,
                None,
            )
            .await?;
        let bot: wire::User = reply.json()?;
        Ok(App {
            slug: app.slug,
            bot: Account {
                id: bot.id,
                login: bot.login,
            },
        })
    }

    async fn resolve(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Option<String>, Error> {
        self.resolve_as(
            Credential::Installation(repository.installation),
            &repository.owner,
            &repository.name,
            reference,
        )
        .await
    }

    async fn file(
        &self,
        repository: &Repository,
        commit: &str,
        path: &str,
    ) -> Result<Option<File>, Error> {
        let Some(contents) = self.contents(repository, commit, path).await? else {
            return Ok(None);
        };
        let wire::Contents::Item(item) = contents else {
            return Ok(None);
        };
        if item.kind != "file" {
            return Ok(None);
        }
        let content = match item.encoding.as_deref() {
            Some("base64") => decode(&item.content)?,
            _ => self.blob(repository, &item.sha).await?,
        };
        Ok(Some(File {
            sha: item.sha,
            content,
        }))
    }

    async fn directory(
        &self,
        repository: &Repository,
        commit: &str,
        path: &str,
    ) -> Result<Option<Vec<Entry>>, Error> {
        match self.contents(repository, commit, path).await? {
            Some(wire::Contents::Listing(listed)) => {
                Ok(Some(listed.into_iter().map(Entry::from).collect()))
            }
            Some(wire::Contents::Item(_)) | None => Ok(None),
        }
    }

    async fn compare(
        &self,
        repository: &Repository,
        base: &str,
        head: &str,
    ) -> Result<Comparison, Error> {
        let url = self.repository_url(repository, &["compare"], &format!("{base}...{head}"));
        let reply = self
            .request(
                Credential::Installation(repository.installation),
                Method::GET,
                url,
                JSON,
                None,
            )
            .await?;
        let comparison: wire::Comparison = reply.json()?;
        let truncated = comparison.files.len() >= COMPARE_FILES;
        Ok(Comparison {
            files: comparison
                .files
                .into_iter()
                .map(|file| file.filename)
                .collect(),
            truncated,
        })
    }

    async fn parents(
        &self,
        repository: &Repository,
        commit: &str,
    ) -> Result<Option<Vec<String>>, Error> {
        let url = self.repository_url(repository, &["git", "commits", commit], "");
        let reply = self
            .request(
                Credential::Installation(repository.installation),
                Method::GET,
                url,
                JSON,
                None,
            )
            .await?;
        if matches!(
            reply.status,
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return Ok(None);
        }
        let commit: wire::Commit = reply.json()?;
        Ok(Some(
            commit
                .parents
                .into_iter()
                .map(|parent| parent.sha)
                .collect(),
        ))
    }

    async fn pull_request_files(
        &self,
        repository: &Repository,
        number: u64,
    ) -> Result<Comparison, Error> {
        let number = number.to_string();
        let mut url = self.repository_url(repository, &["pulls", &number, "files"], "");
        url.query_pairs_mut().append_pair("per_page", PAGE_SIZE);
        let mut files = Vec::new();
        let mut next = Some(url);
        while let Some(url) = next.take() {
            let reply = self
                .request(
                    Credential::Installation(repository.installation),
                    Method::GET,
                    url,
                    JSON,
                    None,
                )
                .await?;
            let page: Vec<wire::Changed> = reply.json()?;
            files.extend(page.into_iter().map(|file| file.filename));
            next = reply
                .next
                .as_deref()
                .map(|link| self.page(link))
                .transpose()?;
        }
        let truncated = files.len() >= PULL_REQUEST_FILES;
        Ok(Comparison { files, truncated })
    }

    async fn installation_repositories(
        &self,
        installation: u64,
    ) -> Result<Vec<InstalledRepository>, Error> {
        let mut url = self.url(&["installation", "repositories"], "");
        url.query_pairs_mut().append_pair("per_page", PAGE_SIZE);
        let mut repositories = Vec::new();
        let mut next = Some(url);
        while let Some(url) = next.take() {
            let reply = self
                .request(
                    Credential::Installation(installation),
                    Method::GET,
                    url,
                    JSON,
                    None,
                )
                .await?;
            let page: wire::InstallationRepositories = reply.json()?;
            repositories.extend(page.repositories.into_iter().map(InstalledRepository::from));
            next = reply
                .next
                .as_deref()
                .map(|link| self.page(link))
                .transpose()?;
        }
        Ok(repositories)
    }

    async fn installation_permissions(&self, installation: u64) -> Result<Permissions, Error> {
        let installation = installation.to_string();
        let reply = self
            .request(
                Credential::App,
                Method::GET,
                self.url(&["app", "installations", &installation], ""),
                JSON,
                None,
            )
            .await?;
        let found: wire::Installation = reply.json()?;
        Ok(found
            .permissions
            .into_iter()
            .filter_map(|(name, level)| held(&level).map(|access| (name, access)))
            .collect())
    }

    async fn mint_token(
        &self,
        installation: u64,
        repositories: &[String],
        permissions: &Permissions,
    ) -> Result<InstallationToken, Error> {
        if repositories.is_empty() || permissions.is_empty() {
            return Err(Error::Unscoped);
        }
        let permissions: Map<String, Value> = permissions
            .iter()
            .map(|(name, access)| (name.clone(), Value::from(access_name(*access))))
            .collect();
        self.access_token(
            installation,
            &json!({ "repositories": repositories, "permissions": permissions }),
        )
        .await
    }

    async fn revoke_token(&self, token: &str) -> Result<(), Error> {
        self.request(
            Credential::Token(token),
            Method::DELETE,
            self.url(&["installation", "token"], ""),
            JSON,
            None,
        )
        .await?
        .done()
    }

    async fn create_check_run(
        &self,
        repository: &Repository,
        head_sha: &str,
        check_run: &CheckRun,
    ) -> Result<u64, Error> {
        let mut body = check_run_body(check_run);
        body.insert("head_sha".to_owned(), Value::from(head_sha));
        let reply = self
            .request(
                Credential::Installation(repository.installation),
                Method::POST,
                self.repository_url(repository, &["check-runs"], ""),
                JSON,
                Some(&Value::Object(body)),
            )
            .await?;
        let created: wire::Created = reply.json()?;
        Ok(created.id)
    }

    async fn update_check_run(
        &self,
        repository: &Repository,
        id: u64,
        check_run: &CheckRun,
    ) -> Result<(), Error> {
        let id = id.to_string();
        self.request(
            Credential::Installation(repository.installation),
            Method::PATCH,
            self.repository_url(repository, &["check-runs", &id], ""),
            JSON,
            Some(&Value::Object(check_run_body(check_run))),
        )
        .await?
        .done()
    }

    async fn create_status(
        &self,
        repository: &Repository,
        commit: &str,
        status: &CommitStatus,
    ) -> Result<(), Error> {
        let mut body = Map::new();
        body.insert("state".to_owned(), Value::from(state_name(status.state)));
        body.insert("context".to_owned(), Value::from(status.context.as_str()));
        if let Some(description) = &status.description {
            body.insert("description".to_owned(), Value::from(description.as_str()));
        }
        if let Some(target_url) = &status.target_url {
            body.insert("target_url".to_owned(), Value::from(target_url.as_str()));
        }
        self.request(
            Credential::Installation(repository.installation),
            Method::POST,
            self.repository_url(repository, &["statuses", commit], ""),
            JSON,
            Some(&Value::Object(body)),
        )
        .await?
        .done()
    }

    async fn tarball(&self, repository: &Repository, commit: &str) -> Result<Download, Error> {
        let secret = self.installation_token(repository.installation).await?;
        self.download(Some(secret), &repository.owner, &repository.name, commit)
            .await
    }

    async fn resolve_public(
        &self,
        owner: &str,
        name: &str,
        reference: &str,
    ) -> Result<Option<String>, Error> {
        self.resolve_as(Credential::Anonymous, owner, name, reference)
            .await
    }

    async fn public_tarball(
        &self,
        owner: &str,
        name: &str,
        commit: &str,
    ) -> Result<Download, Error> {
        self.download(None, owner, name, commit).await
    }
}

impl GitHubApp {
    async fn resolve_as(
        &self,
        credential: Credential<'_>,
        owner: &str,
        name: &str,
        reference: &str,
    ) -> Result<Option<String>, Error> {
        if reference.split('/').all(str::is_empty) {
            return Ok(None);
        }
        let url = self.repository_path(owner, name, &["commits"], reference);
        let reply = self
            .request(credential, Method::GET, url, SHA, None)
            .await?;
        if matches!(
            reply.status,
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return Ok(None);
        }
        reply.done()?;
        let sha = String::from_utf8_lossy(&reply.body).trim().to_owned();
        if sha.is_empty() || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Body(format!(
                "`{reference}` resolved to `{sha}`, which is not a commit"
            )));
        }
        Ok(Some(sha))
    }

    async fn download(
        &self,
        secret: Option<String>,
        owner: &str,
        name: &str,
        commit: &str,
    ) -> Result<Download, Error> {
        let url = self.repository_path(owner, name, &["tarball"], commit);
        let mut request = self.http.get(url).header(ACCEPT, JSON);
        if let Some(secret) = secret {
            request = request.bearer_auth(secret);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.without_url()))?;
        let status = response.status();
        if let Some(reset) = limited(status, response.headers(), Utc::now()) {
            return Err(Error::RateLimited { reset });
        }
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .map_err(|error| Error::Transport(error.without_url()))?;
            return Err(failure(status, &body));
        }
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| io::Error::other(error.without_url())));
        Ok(Download::new(StreamReader::new(body)))
    }

    async fn contents(
        &self,
        repository: &Repository,
        commit: &str,
        path: &str,
    ) -> Result<Option<wire::Contents>, Error> {
        let mut url = self.repository_url(repository, &["contents"], path);
        url.query_pairs_mut().append_pair("ref", commit);
        let reply = self
            .request(
                Credential::Installation(repository.installation),
                Method::GET,
                url,
                JSON,
                None,
            )
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        reply.json().map(Some)
    }
}

fn check_run_body(check_run: &CheckRun) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("name".to_owned(), Value::from(check_run.name.as_str()));
    if let Some(details_url) = &check_run.details_url {
        body.insert("details_url".to_owned(), Value::from(details_url.as_str()));
    }
    let status = match check_run.status {
        CheckStatus::Queued => "queued",
        CheckStatus::InProgress => "in_progress",
        CheckStatus::Completed(conclusion) => {
            body.insert(
                "conclusion".to_owned(),
                Value::from(conclusion_name(conclusion)),
            );
            "completed"
        }
    };
    body.insert("status".to_owned(), Value::from(status));
    if let Some(output) = &check_run.output {
        let mut object = Map::new();
        object.insert("title".to_owned(), Value::from(output.title.as_str()));
        object.insert("summary".to_owned(), Value::from(output.summary.as_str()));
        if !output.annotations.is_empty() {
            object.insert(
                "annotations".to_owned(),
                output.annotations.iter().map(annotation_body).collect(),
            );
        }
        body.insert("output".to_owned(), Value::Object(object));
    }
    body
}

fn annotation_body(annotation: &Annotation) -> Value {
    let start = annotation.line.unwrap_or(1).max(1);
    let end = annotation.end_line.unwrap_or(start).max(start);
    let mut body = Map::new();
    body.insert(
        "path".to_owned(),
        Value::from(annotation.file.as_deref().unwrap_or(NO_FILE)),
    );
    body.insert("start_line".to_owned(), Value::from(start));
    body.insert("end_line".to_owned(), Value::from(end));
    if let Some(column) = annotation.column.filter(|_| start == end) {
        let column = column.max(1);
        let end_column = annotation.end_column.unwrap_or(column).max(column);
        body.insert("start_column".to_owned(), Value::from(column));
        body.insert("end_column".to_owned(), Value::from(end_column));
    }
    let level = match annotation.kind {
        AnnotationKind::Notice => "notice",
        AnnotationKind::Warning => "warning",
        AnnotationKind::Error => "failure",
    };
    body.insert("annotation_level".to_owned(), Value::from(level));
    body.insert(
        "message".to_owned(),
        Value::from(within_bytes(&annotation.message, ANNOTATION_MESSAGE)),
    );
    if let Some(title) = &annotation.title {
        let end = title
            .char_indices()
            .nth(ANNOTATION_TITLE)
            .map_or(title.len(), |(end, _)| end);
        body.insert("title".to_owned(), Value::from(&title[..end]));
    }
    Value::Object(body)
}

fn within_bytes(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn conclusion_name(conclusion: Conclusion) -> &'static str {
    match conclusion {
        Conclusion::Success => "success",
        Conclusion::Failure => "failure",
        Conclusion::Cancelled => "cancelled",
        Conclusion::Skipped => "skipped",
        Conclusion::TimedOut => "timed_out",
    }
}

fn state_name(state: StatusState) -> &'static str {
    match state {
        StatusState::Pending => "pending",
        StatusState::Success => "success",
        StatusState::Failure => "failure",
        StatusState::Error => "error",
    }
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Read => "read",
        Access::Write => "write",
    }
}

fn held(level: &str) -> Option<Access> {
    match level {
        "read" => Some(Access::Read),
        "write" | "admin" => Some(Access::Write),
        _ => None,
    }
}

fn decode(text: &str) -> Result<Vec<u8>, Error> {
    let packed: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    STANDARD
        .decode(packed)
        .map_err(|error| Error::Body(format!("the content is not base64: {error}")))
}

fn failure(status: StatusCode, body: &[u8]) -> Error {
    let message = match serde_json::from_slice::<wire::Failure>(body) {
        Ok(failure) => failure.message,
        Err(_) => String::from_utf8_lossy(body)
            .chars()
            .take(MESSAGE_LIMIT)
            .collect(),
    };
    Error::Status {
        status: status.as_u16(),
        message,
    }
}

fn limited(status: StatusCode, headers: &HeaderMap, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    if let Some(seconds) = number(headers, RETRY_AFTER) {
        return Some(now + TimeDelta::seconds(seconds));
    }
    if number(headers, "x-ratelimit-remaining") == Some(0) {
        let reset = number(headers, "x-ratelimit-reset")
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
            .unwrap_or(now + SECONDARY_WAIT);
        return Some(reset);
    }
    (status == StatusCode::TOO_MANY_REQUESTS).then(|| now + SECONDARY_WAIT)
}

fn number(headers: &HeaderMap, name: impl AsHeaderName) -> Option<i64> {
    let value: i64 = headers.get(name)?.to_str().ok()?.trim().parse().ok()?;
    (value >= 0).then_some(value)
}

fn next_page(headers: &HeaderMap) -> Option<String> {
    let link = headers.get(LINK)?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let mut fields = part.split(';');
        let target = fields.next()?.trim();
        let next = fields.any(|field| field.trim() == r#"rel="next""#);
        let target = target.strip_prefix('<')?.strip_suffix('>')?;
        next.then(|| target.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue};
    use url::Url;

    use super::{GitHubApp, limited, next_page};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                *name,
                HeaderValue::from_str(value).unwrap_or_else(|error| panic!("{error}")),
            );
        }
        headers
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap_or_else(|| panic!("a timestamp"))
    }

    #[test]
    fn the_next_page_comes_from_the_link_header() {
        let link = r#"<https://api.github.com/repositories/1/pulls/2/files?per_page=100&page=2>; rel="next", <https://api.github.com/repositories/1/pulls/2/files?per_page=100&page=5>; rel="last""#;
        assert_eq!(
            next_page(&headers(&[("link", link)])).as_deref(),
            Some("https://api.github.com/repositories/1/pulls/2/files?per_page=100&page=2")
        );
        let last = r#"<https://api.github.com/repositories/1/pulls/2/files?page=1>; rel="first", <https://api.github.com/repositories/1/pulls/2/files?page=4>; rel="prev""#;
        assert_eq!(next_page(&headers(&[("link", last)])), None);
        assert_eq!(next_page(&HeaderMap::new()), None);
    }

    #[test]
    fn a_next_page_outside_the_api_is_refused() {
        let api = Url::parse("https://forge.example.org/api/v3")
            .unwrap_or_else(|error| panic!("{error}"));
        let app = GitHubApp::new(1, bosun_testkit::forge::APP_KEY.as_bytes(), &api)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            app.page("https://forge.example.org/api/v3/repositories/1/pulls/2/files?page=2")
                .is_ok()
        );
        for outside in [
            "https://elsewhere.example.org/api/v3/repositories/1/pulls/2/files?page=2",
            "http://forge.example.org/api/v3/repositories/1/pulls/2/files?page=2",
            "https://forge.example.org:8443/api/v3/repositories/1/pulls/2/files?page=2",
            "https://forge.example.org/api/v30/repositories/1/pulls/2/files?page=2",
            "https://forge.example.org/elsewhere?page=2",
            "not an address",
        ] {
            assert!(app.page(outside).is_err(), "{outside}");
        }
    }

    #[test]
    fn rate_limits_are_read_from_the_response() {
        let now = now();
        let reset = now + TimeDelta::minutes(20);
        let exhausted = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &reset.timestamp().to_string()),
        ]);
        assert_eq!(limited(StatusCode::FORBIDDEN, &exhausted, now), Some(reset));
        assert_eq!(
            limited(StatusCode::TOO_MANY_REQUESTS, &exhausted, now),
            Some(reset)
        );
        let retry = headers(&[("retry-after", "30"), ("x-ratelimit-remaining", "12")]);
        assert_eq!(
            limited(StatusCode::FORBIDDEN, &retry, now),
            Some(now + TimeDelta::seconds(30))
        );
        assert_eq!(
            limited(StatusCode::TOO_MANY_REQUESTS, &HeaderMap::new(), now),
            Some(now + TimeDelta::minutes(1))
        );
        let forbidden = headers(&[("x-ratelimit-remaining", "12")]);
        assert_eq!(limited(StatusCode::FORBIDDEN, &forbidden, now), None);
        assert_eq!(limited(StatusCode::NOT_FOUND, &exhausted, now), None);
    }
}
