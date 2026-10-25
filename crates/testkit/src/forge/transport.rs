use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use axum::body::Bytes;
use axum::extract::{Path as Segments, RawQuery, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;

use super::git::Tree;
use super::{Repository, Shared, State as Model, api};

const UPLOAD: &str = "git-upload-pack";

const RECEIVE: &str = "git-receive-pack";

const HEAD_END: &[u8] = b"\r\n\r\n";

const SETTINGS: [(&str, &str); 2] = [
    ("receive.shallowUpdate", "true"),
    ("uploadpack.allowReachableSHA1InWant", "true"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Service {
    Upload,
    Receive,
}

impl Service {
    fn named(name: &str) -> Option<Service> {
        match name {
            UPLOAD => Some(Service::Upload),
            RECEIVE => Some(Service::Receive),
            _ => None,
        }
    }
}

struct Request {
    method: &'static str,
    path: String,
    query: String,
    headers: HeaderMap,
    user: Option<String>,
    body: Bytes,
}

enum Refusal {
    Unauthenticated,
    Missing,
    Denied(String),
}

impl Refusal {
    fn response(self) -> Response {
        match self {
            Refusal::Unauthenticated => challenge(),
            Refusal::Missing => refuse(StatusCode::NOT_FOUND, "Repository not found."),
            Refusal::Denied(full_name) => refuse(
                StatusCode::FORBIDDEN,
                &format!("Permission to {full_name}.git denied to installation."),
            ),
        }
    }
}

struct Pushed {
    changed: BTreeMap<String, Option<String>>,
    commits: Vec<(String, Vec<String>, Tree)>,
    blobs: HashMap<String, Vec<u8>>,
}

pub(super) fn create(root: &Path, id: u64, branch: &str) {
    let directory = root.join(directory(id));
    let created = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(format!("--initial-branch={branch}"))
        .arg(&directory)
        .env_clear()
        .envs(environment(root))
        .output()
        .unwrap_or_else(|error| panic!("the fake forge cannot run git: {error}"));
    assert!(
        created.status.success(),
        "git init of {} failed: {}",
        directory.display(),
        String::from_utf8_lossy(&created.stderr)
    );
    for (key, value) in SETTINGS {
        git(root, &directory, &["config", key, value], Vec::new())
            .unwrap_or_else(|problem| panic!("{problem}"));
    }
}

pub(super) async fn advertise(
    State(shared): State<Arc<Shared>>,
    Segments((owner, repository)): Segments<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let query = query.unwrap_or_default();
    let service = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("service="))
        .and_then(Service::named);
    let Some(service) = service else {
        return refuse(
            StatusCode::FORBIDDEN,
            "The fake forge serves git's smart HTTP protocol only.",
        );
    };
    let request = Request {
        method: "GET",
        path: "info/refs".to_owned(),
        query,
        headers,
        user: None,
        body: Bytes::new(),
    };
    serve(shared, &owner, &repository, service, request).await
}

pub(super) async fn upload_pack(
    State(shared): State<Arc<Shared>>,
    Segments((owner, repository)): Segments<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = Request {
        method: "POST",
        path: UPLOAD.to_owned(),
        query: String::new(),
        headers,
        user: None,
        body,
    };
    serve(shared, &owner, &repository, Service::Upload, request).await
}

pub(super) async fn receive_pack(
    State(shared): State<Arc<Shared>>,
    Segments((owner, repository)): Segments<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = Request {
        method: "POST",
        path: RECEIVE.to_owned(),
        query: String::new(),
        headers,
        user: None,
        body,
    };
    serve(shared, &owner, &repository, Service::Receive, request).await
}

async fn serve(
    shared: Arc<Shared>,
    owner: &str,
    repository: &str,
    service: Service,
    mut request: Request,
) -> Response {
    let Some(root) = shared.git.clone() else {
        return api::missing().await;
    };
    let name = repository.strip_suffix(".git").unwrap_or(repository);
    let full_name = format!("{owner}/{name}");
    let granted = access(
        &shared.lock(),
        &full_name,
        credential(&request.headers),
        service,
    );
    let (id, user) = match granted {
        Ok(granted) => granted,
        Err(refused) => return refused.response(),
    };
    request.path = format!("/{}/{}", directory(id), request.path);
    request.user = user;
    let answered = tokio::task::spawn_blocking(move || {
        let repository = root.join(directory(id));
        if service == Service::Receive && request.method == "POST" {
            let before = references(&root, &repository)?;
            let answer = backend(&root, &request)?;
            let after = references(&root, &repository)?;
            let (commits, blobs) = known(&shared, &full_name);
            let pushed = pushed(&root, &repository, &before, &after, &commits, &blobs)?;
            if let Some(found) = shared.lock().repositories.get_mut(&full_name) {
                found.absorb(pushed);
            }
            Ok(answer)
        } else {
            backend(&root, &request)
        }
    })
    .await;
    match answered {
        Ok(Ok(answer)) => answer,
        Ok(Err(problem)) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &problem),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

fn access(
    model: &Model,
    full_name: &str,
    secret: Option<String>,
    service: Service,
) -> Result<(u64, Option<String>), Refusal> {
    let Some(repository) = model.repositories.get(full_name) else {
        return Err(Refusal::Missing);
    };
    let reading = service == Service::Upload;
    if reading && repository.public && secret.is_none() {
        return Ok((repository.id, None));
    }
    let Some(secret) = secret else {
        return Err(Refusal::Unauthenticated);
    };
    let now = Utc::now();
    let Some(minted) = model
        .tokens
        .iter()
        .find(|minted| minted.token == secret)
        .filter(|minted| !minted.revoked && minted.expires_at > now)
    else {
        return Err(Refusal::Unauthenticated);
    };
    let user = Some(format!("installation-{}", minted.installation));
    if reading && repository.public {
        return Ok((repository.id, user));
    }
    if !minted.covers(repository) {
        return Err(Refusal::Missing);
    }
    let needed = if reading { "read" } else { "write" };
    let held = minted
        .permissions
        .get("contents")
        .map_or(0, |held| api::level(held));
    if held < api::level(needed) {
        return Err(Refusal::Denied(full_name.to_owned()));
    }
    Ok((repository.id, user))
}

fn credential(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = value.split_once(' ')?;
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = STANDARD.decode(rest).ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (_, password) = decoded.split_once(':')?;
        Some(password.to_owned())
    } else if scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("token") {
        Some(rest.to_owned())
    } else {
        None
    }
}

fn challenge() -> Response {
    let mut response = refuse(
        StatusCode::UNAUTHORIZED,
        "Invalid username or token. Password authentication is not supported for Git operations.",
    );
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"GitHub\""),
    );
    response
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{message}\n"),
    )
        .into_response()
}

fn backend(root: &Path, request: &Request) -> Result<Response, String> {
    let mut command = Command::new("git");
    command
        .arg("http-backend")
        .env_clear()
        .envs(environment(root))
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", request.method)
        .env("PATH_INFO", &request.path)
        .env("QUERY_STRING", &request.query)
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env("REMOTE_ADDR", "127.0.0.1");
    for (name, variable) in [
        (header::CONTENT_TYPE.as_str(), "CONTENT_TYPE"),
        (header::CONTENT_ENCODING.as_str(), "HTTP_CONTENT_ENCODING"),
        ("git-protocol", "HTTP_GIT_PROTOCOL"),
    ] {
        if let Some(value) = request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
        {
            command.env(variable, value);
        }
    }
    if let Some(user) = &request.user {
        command.env("REMOTE_USER", user);
    }
    let output = run(command, request.body.to_vec())?;
    if !output.status.success() {
        return Err(format!(
            "git http-backend failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(respond(&output.stdout))
}

fn respond(output: &[u8]) -> Response {
    let (head, body) = output
        .windows(HEAD_END.len())
        .position(|window| window == HEAD_END)
        .map_or((output, &[][..]), |at| {
            (&output[..at], &output[at + HEAD_END.len()..])
        });
    let mut status = StatusCode::OK;
    let mut headers = HeaderMap::new();
    for line in String::from_utf8_lossy(head).lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("status") {
            if let Some(code) = value
                .split_whitespace()
                .next()
                .and_then(|code| code.parse::<u16>().ok())
                .and_then(|code| StatusCode::from_u16(code).ok())
            {
                status = code;
            }
        } else if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.trim().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.append(name, value);
        }
    }
    (status, headers, body.to_vec()).into_response()
}

fn known(shared: &Shared, full_name: &str) -> (HashSet<String>, HashSet<String>) {
    let model = shared.lock();
    model
        .repositories
        .get(full_name)
        .map(|repository| {
            (
                repository.commits.keys().cloned().collect(),
                repository.blobs.keys().cloned().collect(),
            )
        })
        .unwrap_or_default()
}

fn references(root: &Path, repository: &Path) -> Result<BTreeMap<String, String>, String> {
    let listed = git(
        root,
        repository,
        &[
            "for-each-ref",
            "--format=%(objectname) %(*objectname) %(refname)",
            "refs/heads",
            "refs/tags",
        ],
        Vec::new(),
    )?;
    Ok(String::from_utf8_lossy(&listed)
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ' ');
            let object = fields.next()?;
            let peeled = fields.next()?;
            let name = fields.next()?;
            let commit = if peeled.is_empty() { object } else { peeled };
            Some((name.to_owned(), commit.to_owned()))
        })
        .collect())
}

fn pushed(
    root: &Path,
    repository: &Path,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    commits: &HashSet<String>,
    blobs: &HashSet<String>,
) -> Result<Pushed, String> {
    let mut changed: BTreeMap<String, Option<String>> = after
        .iter()
        .filter(|(name, commit)| before.get(*name) != Some(*commit))
        .map(|(name, commit)| (name.clone(), Some(commit.clone())))
        .collect();
    changed.extend(
        before
            .keys()
            .filter(|name| !after.contains_key(*name))
            .map(|name| (name.clone(), None)),
    );
    let tips: Vec<&str> = changed.values().flatten().map(String::as_str).collect();
    let mut pushed = Pushed {
        changed: BTreeMap::new(),
        commits: Vec::new(),
        blobs: HashMap::new(),
    };
    if !tips.is_empty() {
        let mut arguments = vec!["rev-list", "--parents"];
        arguments.extend(&tips);
        let listed = git(root, repository, &arguments, Vec::new())?;
        let mut wanted: Vec<String> = Vec::new();
        for line in String::from_utf8_lossy(&listed).lines() {
            let mut fields = line.split_whitespace().map(str::to_owned);
            let Some(commit) = fields.next() else {
                continue;
            };
            if commits.contains(&commit) {
                continue;
            }
            let parents: Vec<String> = fields.collect();
            let tree = tree(root, repository, &commit)?;
            for sha in tree.values() {
                if !blobs.contains(sha) && !wanted.contains(sha) {
                    wanted.push(sha.clone());
                }
            }
            pushed.commits.push((commit, parents, tree));
        }
        pushed.blobs = contents(root, repository, &wanted)?;
    }
    pushed.changed = changed;
    Ok(pushed)
}

fn tree(root: &Path, repository: &Path, commit: &str) -> Result<Tree, String> {
    let listed = git(
        root,
        repository,
        &["ls-tree", "-r", "-z", commit],
        Vec::new(),
    )?;
    let mut tree = Tree::new();
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split(' ');
        let (Some(_), Some(kind), Some(sha)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if kind == "blob" {
            tree.insert(path.to_owned(), sha.to_owned());
        }
    }
    Ok(tree)
}

fn contents(
    root: &Path,
    repository: &Path,
    wanted: &[String],
) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    let mut input = wanted.join("\n").into_bytes();
    input.push(b'\n');
    let output = git(root, repository, &["cat-file", "--batch"], input)?;
    let mut rest = output.as_slice();
    while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 1..];
        let mut fields = line.split(' ');
        let (Some(sha), Some(_), Some(size)) = (fields.next(), fields.next(), fields.next()) else {
            return Err(format!("git cat-file answered `{line}`"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("git cat-file answered `{line}`"))?;
        if rest.len() < size + 1 {
            return Err(format!("git cat-file cut {sha} short"));
        }
        found.insert(sha.to_owned(), rest[..size].to_vec());
        rest = &rest[size + 1..];
    }
    Ok(found)
}

fn git(
    root: &Path,
    repository: &Path,
    arguments: &[&str],
    input: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(repository)
        .args(arguments)
        .env_clear()
        .envs(environment(root));
    let output = run(command, input)?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn run(mut command: Command, input: Vec<u8>) -> Result<std::process::Output, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the fake forge cannot run git: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "git has no standard input".to_owned())?;
    let writer = thread::spawn(move || stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .map_err(|error| format!("git did not finish: {error}"))?;
    let _ = writer.join();
    Ok(output)
}

fn environment(root: &Path) -> [(&'static str, OsString); 3] {
    [
        ("PATH", std::env::var_os("PATH").unwrap_or_default()),
        ("HOME", root.as_os_str().to_owned()),
        ("GIT_CONFIG_NOSYSTEM", OsString::from("1")),
    ]
}

fn directory(id: u64) -> String {
    format!("{id}.git")
}

impl Repository {
    fn absorb(&mut self, pushed: Pushed) {
        self.blobs.extend(pushed.blobs);
        for (commit, parents, tree) in pushed.commits {
            self.commits.insert(commit.clone(), tree);
            self.parents.insert(commit, parents);
        }
        let mut branches = Vec::new();
        for (reference, commit) in pushed.changed {
            if let Some(branch) = reference.strip_prefix("refs/heads/") {
                branches.push(branch.to_owned());
            }
            match commit {
                Some(commit) => self.refs.insert(reference, commit),
                None => self.refs.remove(&reference),
            };
        }
        let touched: Vec<u64> = self
            .pulls
            .iter()
            .filter(|(_, pull)| branches.contains(&pull.base) || branches.contains(&pull.head))
            .map(|(number, _)| *number)
            .collect();
        for number in touched {
            self.merge(number);
        }
    }
}
