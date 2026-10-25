use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use super::git::{self, Node};
use super::{
    APP_ID, APP_SLUG, BOT, BOT_ID, Bucket, COMPARE_FILES, MintedToken, PULL_REQUEST_FILES,
    RecordedAnnotation, RecordedCheckRun, RecordedDownload, RecordedOutput, RecordedRequest,
    RecordedStatus, Repository, Shared, State as Model, events, jwt,
};

type Fake = State<Arc<Shared>>;

const DOCS: &str = "https://docs.github.com/rest";

const JSON: &str = "application/json; charset=utf-8";

const INLINE: usize = 1 << 20;

const PAGE: usize = 30;

const LARGEST_PAGE: usize = 100;

const SUMMARY_CHARACTERS: usize = 65_535;

const ANNOTATIONS_PER_REQUEST: usize = 50;

const ANNOTATION_LEVELS: &[&str] = &["notice", "warning", "failure"];

const ANNOTATION_MESSAGE_BYTES: usize = 64 * 1024;

const ANNOTATION_TITLE_CHARACTERS: usize = 255;

const CHECK_STATUSES: &[&str] = &[
    "queued",
    "in_progress",
    "completed",
    "waiting",
    "requested",
    "pending",
];

const CHECK_CONCLUSIONS: &[&str] = &[
    "action_required",
    "cancelled",
    "failure",
    "neutral",
    "success",
    "skipped",
    "stale",
    "timed_out",
];

const STATUS_STATES: &[&str] = &["error", "failure", "pending", "success"];

struct Answer {
    status: StatusCode,
    content_type: &'static str,
    body: Vec<u8>,
    headers: Vec<(HeaderName, String)>,
}

impl Answer {
    fn json(status: StatusCode, value: &Value) -> Answer {
        Answer::raw(status, JSON, value.to_string().into_bytes())
    }

    fn raw(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Answer {
        Answer {
            status,
            content_type,
            body,
            headers: Vec::new(),
        }
    }

    fn message(status: StatusCode, message: &str) -> Answer {
        Answer::json(
            status,
            &json!({ "message": message, "documentation_url": DOCS }),
        )
    }

    fn not_found() -> Answer {
        Answer::message(StatusCode::NOT_FOUND, "Not Found")
    }

    fn invalid(message: &str) -> Answer {
        Answer::message(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    fn with(mut self, name: HeaderName, value: impl Into<String>) -> Answer {
        self.headers.push((name, value.into()));
        self
    }
}

impl IntoResponse for Answer {
    fn into_response(self) -> Response {
        let mut response = (self.status, self.body).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(self.content_type),
        );
        for (name, value) in self.headers {
            if let Ok(value) = HeaderValue::from_str(&value) {
                headers.append(name, value);
            }
        }
        response
    }
}

pub(super) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/api/v3/app", get(app))
        .route(
            "/api/v3/app/installations/{installation}/access_tokens",
            post(access_token),
        )
        .route("/api/v3/installation/token", delete(revoke))
        .route("/api/v3/users/{login}", get(user))
        .route(
            "/api/v3/installation/repositories",
            get(installation_repositories),
        )
        .route("/api/v3/repos/{owner}/{repo}/contents", get(contents_root))
        .route(
            "/api/v3/repos/{owner}/{repo}/contents/{*path}",
            get(contents_path),
        )
        .route("/api/v3/repos/{owner}/{repo}/git/blobs/{sha}", get(blob))
        .route(
            "/api/v3/repos/{owner}/{repo}/git/commits/{sha}",
            get(git_commit),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/commits/{*reference}",
            get(commit),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/compare/{*basehead}",
            get(compare),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/pulls/{number}/files",
            get(pull_request_files),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/check-runs",
            post(create_check_run),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/check-runs/{id}",
            patch(update_check_run),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/statuses/{sha}",
            post(create_status),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/tarball/{*reference}",
            get(tarball),
        )
        .route(
            "/codeload/{owner}/{repo}/legacy.tar.gz/{sha}",
            get(codeload),
        )
        .fallback(missing)
        .layer(middleware::from_fn_with_state(Arc::clone(&shared), observe))
        .with_state(shared)
}

async fn missing() -> Response {
    Answer::not_found().into_response()
}

async fn app(State(shared): Fake, headers: HeaderMap) -> Response {
    if let Err(refused) = as_app(&shared, &headers) {
        return refused.into_response();
    }
    Answer::json(
        StatusCode::OK,
        &json!({ "id": APP_ID, "slug": APP_SLUG, "name": "Bosun (fake)" }),
    )
    .into_response()
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    repositories: Option<Vec<String>>,
    permissions: Option<BTreeMap<String, String>>,
}

async fn access_token(
    State(shared): Fake,
    Path(installation): Path<u64>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(refused) = as_app(&shared, &headers) {
        return refused.into_response();
    }
    let scope: Scope = if body.is_empty() {
        Scope::default()
    } else {
        match parse(&body) {
            Ok(scope) => scope,
            Err(refused) => return refused.into_response(),
        }
    };
    let mut model = shared.lock();
    let Some(granted) = model
        .installations
        .get(&installation)
        .map(|installation| installation.permissions.clone())
    else {
        return Answer::not_found().into_response();
    };
    let visible = model.installation_repositories(installation);
    let (repositories, selection) = match scope.repositories {
        Some(names) => {
            if names.iter().any(|name| !visible.contains(name)) {
                return Answer::invalid(
                    "There is at least one repository that does not exist or is not accessible \
                     to the parent installation.",
                )
                .into_response();
            }
            let names: BTreeSet<String> = names.into_iter().collect();
            (names.into_iter().collect::<Vec<_>>(), "selected")
        }
        None => (visible.into_iter().collect(), "all"),
    };
    let permissions = match scope.permissions {
        Some(requested) => {
            let exceeds = requested.iter().any(|(name, access)| {
                level(access) == 0
                    || granted.get(name).map_or(0, |held| level(held)) < level(access)
            });
            if exceeds {
                return Answer::invalid(
                    "The permissions requested are not granted to this installation.",
                )
                .into_response();
            }
            requested
        }
        None => granted,
    };
    let token = format!("ghs_{}", Uuid::now_v7().simple());
    let expires_at = Utc::now() + model.token_lifetime;
    let listed: Vec<Value> = repositories
        .iter()
        .map(|name| json!({ "name": name }))
        .collect();
    let answer = Answer::json(
        StatusCode::CREATED,
        &json!({
            "token": token,
            "expires_at": expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "permissions": permissions,
            "repository_selection": selection,
            "repositories": listed,
        }),
    );
    model.tokens.push(MintedToken {
        token,
        installation,
        repository_selection: selection.to_owned(),
        repositories,
        permissions,
        expires_at,
        revoked: false,
    });
    answer.into_response()
}

async fn revoke(State(shared): Fake, headers: HeaderMap) -> Response {
    let mut model = shared.lock();
    let now = Utc::now();
    let secret = bearer(&headers);
    let Some(minted) = model
        .tokens
        .iter_mut()
        .find(|minted| Some(minted.token.as_str()) == secret)
        .filter(|minted| !minted.revoked && minted.expires_at > now)
    else {
        return Answer::message(StatusCode::UNAUTHORIZED, "Bad credentials").into_response();
    };
    minted.revoked = true;
    StatusCode::NO_CONTENT.into_response()
}

async fn user(Path(login): Path<String>) -> Response {
    if login == BOT.login {
        Answer::json(
            StatusCode::OK,
            &json!({ "login": login, "id": BOT_ID, "type": "Bot" }),
        )
        .into_response()
    } else {
        Answer::not_found().into_response()
    }
}

#[derive(Deserialize)]
struct At {
    r#ref: Option<String>,
}

async fn contents_root(
    State(shared): Fake,
    Path((owner, repo)): Path<(String, String)>,
    Query(at): Query<At>,
    headers: HeaderMap,
) -> Response {
    contents(&shared, &headers, &owner, &repo, "", at)
}

async fn contents_path(
    State(shared): Fake,
    Path((owner, repo, path)): Path<(String, String, String)>,
    Query(at): Query<At>,
    headers: HeaderMap,
) -> Response {
    contents(&shared, &headers, &owner, &repo, &path, at)
}

fn contents(
    shared: &Shared,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    path: &str,
    at: At,
) -> Response {
    read(shared, headers, owner, repo, "contents", |_, repository| {
        let reference = at
            .r#ref
            .unwrap_or_else(|| repository.default_branch.clone());
        let Some((_, tree)) = repository.tree(&reference) else {
            return Answer::message(
                StatusCode::NOT_FOUND,
                &format!("No commit found for the ref {reference}"),
            );
        };
        let path = path.trim_matches('/');
        if let Some(sha) = tree.get(path) {
            let content = &repository.blobs[sha];
            let (encoding, inline) = if content.len() > INLINE {
                ("none", String::new())
            } else {
                ("base64", git::base64_lines(content))
            };
            return Answer::json(
                StatusCode::OK,
                &json!({
                    "type": "file",
                    "encoding": encoding,
                    "size": content.len(),
                    "name": path.rsplit('/').next().unwrap_or(path),
                    "path": path,
                    "content": inline,
                    "sha": sha,
                }),
            );
        }
        let Some(entries) = git::listing(tree, path) else {
            return Answer::not_found();
        };
        let entries: Vec<Value> = entries
            .into_iter()
            .map(|(name, node)| {
                let full = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                let (kind, sha, size) = match node {
                    Node::File { sha } => {
                        let size = repository.blobs.get(&sha).map_or(0, Vec::len);
                        ("file", sha, size)
                    }
                    Node::Directory { sha } => ("dir", sha, 0),
                };
                json!({ "type": kind, "name": name, "path": full, "sha": sha, "size": size })
            })
            .collect();
        Answer::json(StatusCode::OK, &Value::Array(entries))
    })
}

async fn blob(
    State(shared): Fake,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    read(
        &shared,
        &headers,
        &owner,
        &repo,
        "contents",
        |_, repository| match repository.blobs.get(&sha) {
            Some(content) => Answer::json(
                StatusCode::OK,
                &json!({
                    "sha": sha,
                    "size": content.len(),
                    "encoding": "base64",
                    "content": git::base64_lines(content),
                }),
            ),
            None => Answer::not_found(),
        },
    )
}

async fn git_commit(
    State(shared): Fake,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    read(
        &shared,
        &headers,
        &owner,
        &repo,
        "contents",
        |_, repository| match repository.parents.get(&sha) {
            Some(parents) => {
                let parents: Vec<Value> = parents
                    .iter()
                    .map(|parent| json!({ "sha": parent }))
                    .collect();
                Answer::json(StatusCode::OK, &json!({ "sha": sha, "parents": parents }))
            }
            None => Answer::not_found(),
        },
    )
}

async fn commit(
    State(shared): Fake,
    Path((owner, repo, reference)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let plain = headers
        .get(header::ACCEPT)
        .and_then(|accept| accept.to_str().ok())
        .is_some_and(|accept| accept.contains("application/vnd.github.sha"));
    read(
        &shared,
        &headers,
        &owner,
        &repo,
        "contents",
        |_, repository| match repository.resolve(&reference) {
            Some(sha) if plain => Answer::raw(
                StatusCode::OK,
                "application/vnd.github.sha; charset=utf-8",
                sha.as_bytes().to_vec(),
            ),
            Some(sha) => Answer::json(StatusCode::OK, &json!({ "sha": sha })),
            None => Answer::invalid(&format!("No commit found for SHA: {reference}")),
        },
    )
}

async fn compare(
    State(shared): Fake,
    Path((owner, repo, basehead)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    read(
        &shared,
        &headers,
        &owner,
        &repo,
        "contents",
        |_, repository| {
            let Some((base, head)) = basehead.split_once("...") else {
                return Answer::not_found();
            };
            let (Some((_, base)), Some((_, head))) = (repository.tree(base), repository.tree(head))
            else {
                return Answer::not_found();
            };
            let changes = git::diff(base, head);
            let status = if changes.is_empty() {
                "identical"
            } else {
                "ahead"
            };
            let files: Vec<Value> = changes
                .into_iter()
                .take(COMPARE_FILES)
                .map(|(path, change)| json!({ "filename": path, "status": change.name() }))
                .collect();
            Answer::json(StatusCode::OK, &json!({ "status": status, "files": files }))
        },
    )
}

#[derive(Deserialize)]
struct Paging {
    per_page: Option<usize>,
    page: Option<usize>,
}

async fn pull_request_files(
    State(shared): Fake,
    Path((owner, repo, number)): Path<(String, String, u64)>,
    Query(paging): Query<Paging>,
    headers: HeaderMap,
) -> Response {
    read(
        &shared,
        &headers,
        &owner,
        &repo,
        "pull_requests",
        |shared, repository| {
            let Some(pull) = repository.pulls.get(&number) else {
                return Answer::not_found();
            };
            let (Some((_, base)), Some((_, head))) =
                (repository.tree(&pull.base), repository.tree(&pull.head))
            else {
                return Answer::not_found();
            };
            let files: Vec<Value> = git::diff(base, head)
                .iter()
                .take(PULL_REQUEST_FILES)
                .map(|(path, change)| json!({ "filename": path, "status": change.name() }))
                .collect();
            let number = number.to_string();
            paginate(
                shared,
                &["repos", &owner, &repo, "pulls", &number, "files"],
                &paging,
                files,
                Value::Array,
            )
        },
    )
}

fn paginate(
    shared: &Shared,
    path: &[&str],
    paging: &Paging,
    items: Vec<Value>,
    body: impl FnOnce(Vec<Value>) -> Value,
) -> Answer {
    let size = paging.per_page.unwrap_or(PAGE).clamp(1, LARGEST_PAGE);
    let page = paging.page.unwrap_or(1).max(1);
    let last = items.len().div_ceil(size).max(1);
    let shown: Vec<Value> = items
        .into_iter()
        .skip((page - 1) * size)
        .take(size)
        .collect();
    let link = |page: usize| {
        let mut url = shared.api.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.extend(path);
        }
        url.query_pairs_mut()
            .append_pair("per_page", &size.to_string())
            .append_pair("page", &page.to_string());
        url
    };
    let mut links = Vec::new();
    if page < last {
        links.push(format!(r#"<{}>; rel="next""#, link(page + 1)));
        links.push(format!(r#"<{}>; rel="last""#, link(last)));
    }
    if page > 1 {
        links.push(format!(r#"<{}>; rel="first""#, link(1)));
        links.push(format!(r#"<{}>; rel="prev""#, link(page - 1)));
    }
    let answer = Answer::json(StatusCode::OK, &body(shown));
    if links.is_empty() {
        answer
    } else {
        answer.with(header::LINK, links.join(", "))
    }
}

async fn installation_repositories(
    State(shared): Fake,
    Query(paging): Query<Paging>,
    headers: HeaderMap,
) -> Response {
    let mut model = shared.lock();
    let (installation, selection, visible) = match model.live(&headers) {
        Ok(minted) => (
            minted.installation,
            minted.repository_selection.clone(),
            model
                .repositories
                .values()
                .filter(|repository| minted.covers(repository))
                .map(|repository| events::repository(&shared, &model, repository))
                .collect::<Vec<Value>>(),
        ),
        Err(refused) => return refused.into_response(),
    };
    let total = visible.len();
    let answer = paginate(
        &shared,
        &["installation", "repositories"],
        &paging,
        visible,
        |repositories| {
            json!({
                "total_count": total,
                "repository_selection": selection,
                "repositories": repositories,
            })
        },
    );
    conditional(&mut model, &headers, Some(installation), answer)
}

pub(super) async fn observe(State(shared): Fake, request: Request, next: Next) -> Response {
    let delay = shared.lock().delay;
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, usize::MAX).await else {
        return Answer::message(StatusCode::BAD_REQUEST, "Problems reading the body")
            .into_response();
    };
    let recorded = serde_json::from_slice(&body).ok();
    let failing = shared.lock().failing;
    let response = if failing {
        Answer::message(StatusCode::BAD_GATEWAY, "Server Error").into_response()
    } else {
        next.run(Request::from_parts(parts, Body::from(body))).await
    };
    shared.lock().requests.push(RecordedRequest {
        method,
        path,
        body: recorded,
        status: response.status().as_u16(),
    });
    response
}

#[derive(Deserialize)]
struct CheckRunBody {
    name: Option<String>,
    head_sha: Option<String>,
    details_url: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    output: Option<OutputBody>,
}

#[derive(Deserialize)]
struct OutputBody {
    title: Option<String>,
    summary: Option<String>,
    #[serde(default)]
    annotations: Vec<AnnotationBody>,
}

#[derive(Deserialize)]
struct AnnotationBody {
    path: Option<String>,
    start_line: Option<u64>,
    end_line: Option<u64>,
    start_column: Option<u64>,
    end_column: Option<u64>,
    annotation_level: Option<String>,
    message: Option<String>,
    title: Option<String>,
}

fn output(body: OutputBody, earlier: Option<RecordedOutput>) -> Result<RecordedOutput, Answer> {
    let (Some(title), Some(summary)) = (body.title, body.summary) else {
        return Err(Answer::invalid(
            "Invalid request. title and summary are required in output.",
        ));
    };
    if title.is_empty() || summary.is_empty() {
        return Err(Answer::invalid(
            "Invalid request. title and summary of output cannot be blank.",
        ));
    }
    if summary.chars().count() > SUMMARY_CHARACTERS {
        return Err(Answer::invalid(&format!(
            "Invalid request. output.summary is longer than {SUMMARY_CHARACTERS} characters."
        )));
    }
    if body.annotations.len() > ANNOTATIONS_PER_REQUEST {
        return Err(Answer::invalid(&format!(
            "Invalid request. Only {ANNOTATIONS_PER_REQUEST} annotations are allowed per request."
        )));
    }
    let mut annotations = earlier
        .map(|earlier| earlier.annotations)
        .unwrap_or_default();
    for annotation in body.annotations {
        annotations.push(recorded_annotation(annotation)?);
    }
    Ok(RecordedOutput {
        title,
        summary,
        annotations,
    })
}

fn recorded_annotation(body: AnnotationBody) -> Result<RecordedAnnotation, Answer> {
    let (Some(path), Some(start_line), Some(end_line), Some(level), Some(message)) = (
        body.path,
        body.start_line,
        body.end_line,
        body.annotation_level,
        body.message,
    ) else {
        return Err(Answer::invalid(
            "Invalid request. path, start_line, end_line, annotation_level and message are \
             required in an annotation.",
        ));
    };
    if start_line == 0 || end_line < start_line {
        return Err(Answer::invalid(&format!(
            "Invalid request. The lines {start_line} to {end_line} of an annotation are not a \
             range of lines."
        )));
    }
    if !ANNOTATION_LEVELS.contains(&level.as_str()) {
        return Err(Answer::invalid(&format!(
            "Invalid request. `{level}` is not an annotation level."
        )));
    }
    if (body.start_column.is_some() || body.end_column.is_some()) && start_line != end_line {
        return Err(Answer::invalid(
            "Invalid request. start_column and end_column are only allowed when start_line and \
             end_line are the same.",
        ));
    }
    if message.is_empty() || message.len() > ANNOTATION_MESSAGE_BYTES {
        return Err(Answer::invalid(
            "Invalid request. The message of an annotation is empty or longer than 64 KB.",
        ));
    }
    if body
        .title
        .as_ref()
        .is_some_and(|title| title.chars().count() > ANNOTATION_TITLE_CHARACTERS)
    {
        return Err(Answer::invalid(&format!(
            "Invalid request. The title of an annotation is longer than \
             {ANNOTATION_TITLE_CHARACTERS} characters."
        )));
    }
    Ok(RecordedAnnotation {
        path,
        start_line,
        end_line,
        start_column: body.start_column,
        end_column: body.end_column,
        annotation_level: level,
        message,
        title: body.title,
    })
}

async fn create_check_run(
    State(shared): Fake,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: CheckRunBody = match parse(&body) {
        Ok(body) => body,
        Err(refused) => return refused.into_response(),
    };
    write(
        &shared,
        &headers,
        &owner,
        &repo,
        "checks",
        |model, full_name| {
            let (Some(name), Some(head_sha)) = (body.name, body.head_sha) else {
                return Answer::invalid("Invalid request. name and head_sha are required.");
            };
            if !model.repositories[full_name]
                .commits
                .contains_key(&head_sha)
            {
                return Answer::invalid(&format!("No commit found for SHA: {head_sha}"));
            }
            let status = match settle(body.status.as_deref(), body.conclusion.as_deref()) {
                Ok(status) => status,
                Err(refused) => return refused,
            };
            let output = match body.output.map(|written| output(written, None)).transpose() {
                Ok(output) => output,
                Err(refused) => return refused,
            };
            let record = RecordedCheckRun {
                id: model.allocate(),
                repository: full_name.to_owned(),
                head_sha,
                name,
                details_url: body.details_url,
                status,
                conclusion: body.conclusion,
                output,
            };
            let answer = Answer::json(StatusCode::CREATED, &check_run(&record));
            model.check_runs.push(record);
            answer
        },
    )
}

async fn update_check_run(
    State(shared): Fake,
    Path((owner, repo, id)): Path<(String, String, u64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: CheckRunBody = match parse(&body) {
        Ok(body) => body,
        Err(refused) => return refused.into_response(),
    };
    write(
        &shared,
        &headers,
        &owner,
        &repo,
        "checks",
        |model, full_name| {
            let Some(record) = model
                .check_runs
                .iter_mut()
                .find(|record| record.id == id && record.repository == full_name)
            else {
                return Answer::not_found();
            };
            let conclusion = body.conclusion.or_else(|| record.conclusion.clone());
            let status = body.status.unwrap_or_else(|| record.status.clone());
            let status = match settle(Some(&status), conclusion.as_deref()) {
                Ok(status) => status,
                Err(refused) => return refused,
            };
            if let Some(written) = body.output {
                match output(written, record.output.clone()) {
                    Ok(output) => record.output = Some(output),
                    Err(refused) => return refused,
                }
            }
            if let Some(name) = body.name {
                record.name = name;
            }
            if let Some(details_url) = body.details_url {
                record.details_url = Some(details_url);
            }
            record.status = status;
            record.conclusion = conclusion;
            Answer::json(StatusCode::OK, &check_run(record))
        },
    )
}

#[derive(Deserialize)]
struct StatusBody {
    state: String,
    context: Option<String>,
    description: Option<String>,
    target_url: Option<String>,
}

async fn create_status(
    State(shared): Fake,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: StatusBody = match parse(&body) {
        Ok(body) => body,
        Err(refused) => return refused.into_response(),
    };
    write(
        &shared,
        &headers,
        &owner,
        &repo,
        "statuses",
        |model, full_name| {
            if !STATUS_STATES.contains(&body.state.as_str()) {
                return Answer::invalid(&format!("`{}` is not a state", body.state));
            }
            if !model.repositories[full_name].commits.contains_key(&sha) {
                return Answer::invalid(&format!("No commit found for SHA: {sha}"));
            }
            let record = RecordedStatus {
                repository: full_name.to_owned(),
                sha,
                context: body.context.unwrap_or_else(|| "default".to_owned()),
                state: body.state,
                description: body.description,
                target_url: body.target_url,
            };
            let answer = Answer::json(
                StatusCode::CREATED,
                &json!({
                    "state": record.state,
                    "context": record.context,
                    "description": record.description,
                    "target_url": record.target_url,
                }),
            );
            model.statuses.push(record);
            answer
        },
    )
}

async fn tarball(
    State(shared): Fake,
    Path((owner, repo, reference)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    read(
        &shared,
        &headers,
        &owner,
        &repo,
        "contents",
        |shared, repository| match repository.resolve(&reference) {
            Some(sha) => {
                let mut location: Url = shared.api.clone();
                if let Ok(mut segments) = location.path_segments_mut() {
                    segments
                        .clear()
                        .extend(["codeload", &owner, &repo, "legacy.tar.gz", sha]);
                }
                Answer::raw(StatusCode::FOUND, "text/html; charset=utf-8", Vec::new())
                    .with(header::LOCATION, location.to_string())
            }
            None => Answer::not_found(),
        },
    )
}

async fn codeload(
    State(shared): Fake,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> Response {
    let mut model = shared.lock();
    let full_name = format!("{owner}/{repo}");
    let Some(repository) = model.repositories.get(&full_name) else {
        return Answer::not_found().into_response();
    };
    let Some(tree) = repository.commits.get(&sha) else {
        return Answer::not_found().into_response();
    };
    let prefix = format!("{}-{}-{}", repository.owner, repository.name, &sha[..7]);
    let tarball = git::tarball(&prefix, tree, &repository.blobs);
    model.downloads.push(RecordedDownload {
        repository: full_name,
        commit: sha,
    });
    Answer::raw(StatusCode::OK, "application/x-gzip", tarball).into_response()
}

fn read(
    shared: &Shared,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    permission: &str,
    produce: impl FnOnce(&Shared, &Repository) -> Answer,
) -> Response {
    let full_name = format!("{owner}/{repo}");
    let mut model = shared.lock();
    let installation = match model.reader(headers, &full_name, permission) {
        Ok(installation) => installation,
        Err(refused) => return refused.into_response(),
    };
    let answer = match model.repositories.get(&full_name) {
        Some(repository) => produce(shared, repository),
        None => Answer::not_found(),
    };
    conditional(&mut model, headers, installation, answer)
}

fn conditional(
    model: &mut Model,
    headers: &HeaderMap,
    installation: Option<u64>,
    mut answer: Answer,
) -> Response {
    if answer.status == StatusCode::OK {
        let etag = format!(r#"W/"{}""#, git::hex(&Sha256::digest(&answer.body)));
        let matched = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag));
        if matched {
            return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
        }
        answer = answer.with(header::ETAG, etag);
    }
    match model.spend(installation) {
        Ok(rate) => {
            answer.headers.extend(rate);
            answer.into_response()
        }
        Err(limited) => limited.into_response(),
    }
}

fn write(
    shared: &Shared,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    permission: &str,
    change: impl FnOnce(&mut Model, &str) -> Answer,
) -> Response {
    let full_name = format!("{owner}/{repo}");
    let mut model = shared.lock();
    let installation = match model.authorize(headers, &full_name, permission, "write") {
        Ok(installation) => installation,
        Err(refused) => return refused.into_response(),
    };
    let rate = match model.spend(Some(installation)) {
        Ok(rate) => rate,
        Err(limited) => return limited.into_response(),
    };
    let mut answer = change(&mut model, &full_name);
    answer.headers.extend(rate);
    answer.into_response()
}

impl Model {
    fn live(&self, headers: &HeaderMap) -> Result<&MintedToken, Answer> {
        let now = Utc::now();
        let secret = bearer(headers);
        self.tokens
            .iter()
            .find(|minted| Some(minted.token.as_str()) == secret)
            .filter(|minted| !minted.revoked && minted.expires_at > now)
            .ok_or_else(|| Answer::message(StatusCode::UNAUTHORIZED, "Bad credentials"))
    }

    fn authorize(
        &self,
        headers: &HeaderMap,
        full_name: &str,
        permission: &str,
        needed: &str,
    ) -> Result<u64, Answer> {
        let minted = self.live(headers)?;
        let repository = self
            .repositories
            .get(full_name)
            .filter(|repository| minted.covers(repository))
            .ok_or_else(Answer::not_found)?;
        let granted = minted
            .permissions
            .get(permission)
            .map_or(0, |held| level(held));
        if granted < level(needed) {
            return Err(Answer::message(
                StatusCode::FORBIDDEN,
                "Resource not accessible by integration",
            ));
        }
        Ok(repository.installation)
    }

    fn reader(
        &self,
        headers: &HeaderMap,
        full_name: &str,
        permission: &str,
    ) -> Result<Option<u64>, Answer> {
        let public = self
            .repositories
            .get(full_name)
            .is_some_and(|repository| repository.public);
        match (public, bearer(headers)) {
            (true, None) => Ok(None),
            (false, None) => Err(Answer::not_found()),
            (true, Some(_)) => self.live(headers).map(|minted| Some(minted.installation)),
            (false, Some(_)) => self
                .authorize(headers, full_name, permission, "read")
                .map(Some),
        }
    }

    fn spend(&mut self, installation: Option<u64>) -> Result<Vec<(HeaderName, String)>, Answer> {
        let Some(installation) = installation else {
            return Ok(Vec::new());
        };
        let Some(bucket) = self.buckets.get_mut(&installation) else {
            return Ok(Vec::new());
        };
        if bucket.remaining == 0 {
            let mut refused = Answer::message(
                StatusCode::FORBIDDEN,
                &format!("API rate limit exceeded for installation ID {installation}."),
            );
            refused.headers = rate(bucket);
            return Err(refused);
        }
        bucket.remaining -= 1;
        Ok(rate(bucket))
    }
}

fn rate(bucket: &Bucket) -> Vec<(HeaderName, String)> {
    vec![
        (
            HeaderName::from_static("x-ratelimit-limit"),
            bucket.limit.to_string(),
        ),
        (
            HeaderName::from_static("x-ratelimit-remaining"),
            bucket.remaining.to_string(),
        ),
        (
            HeaderName::from_static("x-ratelimit-used"),
            (bucket.limit - bucket.remaining).to_string(),
        ),
        (
            HeaderName::from_static("x-ratelimit-reset"),
            bucket.reset.timestamp().to_string(),
        ),
        (
            HeaderName::from_static("x-ratelimit-resource"),
            "core".to_owned(),
        ),
    ]
}

fn as_app(shared: &Shared, headers: &HeaderMap) -> Result<(), Answer> {
    let token = bearer(headers).ok_or_else(|| {
        Answer::message(
            StatusCode::UNAUTHORIZED,
            "A JSON web token could not be decoded",
        )
    })?;
    jwt::verify(&shared.public_key, token, Utc::now())
        .map_err(|message| Answer::message(StatusCode::UNAUTHORIZED, &message))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("token "))
}

fn level(access: &str) -> u8 {
    match access {
        "read" => 1,
        "write" | "admin" => 2,
        _ => 0,
    }
}

fn settle(status: Option<&str>, conclusion: Option<&str>) -> Result<String, Answer> {
    if let Some(conclusion) = conclusion {
        if !CHECK_CONCLUSIONS.contains(&conclusion) {
            return Err(Answer::invalid(&format!(
                "`{conclusion}` is not a conclusion"
            )));
        }
        return Ok("completed".to_owned());
    }
    let status = status.unwrap_or("queued");
    if !CHECK_STATUSES.contains(&status) {
        return Err(Answer::invalid(&format!("`{status}` is not a status")));
    }
    if status == "completed" {
        return Err(Answer::invalid(
            "Invalid request. conclusion is required when status is completed.",
        ));
    }
    Ok(status.to_owned())
}

fn check_run(record: &RecordedCheckRun) -> Value {
    json!({
        "id": record.id,
        "name": record.name,
        "head_sha": record.head_sha,
        "details_url": record.details_url,
        "status": record.status,
        "conclusion": record.conclusion,
        "output": {
            "title": record.output.as_ref().map(|output| output.title.as_str()),
            "summary": record.output.as_ref().map(|output| output.summary.as_str()),
            "annotations_count": record.output.as_ref().map_or(0, |output| output.annotations.len()),
        },
    })
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Answer> {
    serde_json::from_slice(body).map_err(|error| {
        Answer::message(
            StatusCode::BAD_REQUEST,
            &format!("Problems parsing JSON: {error}"),
        )
    })
}
