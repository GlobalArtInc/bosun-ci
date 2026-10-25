use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use bosun_testkit::forge::{APP_ID, FakeForge, RecordedCheckRun, RecordedStatus, Sender};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

const MANIFEST: &str = "manifest.json";

const TARBALL: &str = "tarball.tar.gz";

const READ: &[(&str, &str)] = &[("contents", "read"), ("metadata", "read")];

struct Control {
    forge: FakeForge,
    hook: Mutex<Option<String>>,
}

type Shared = State<Arc<Control>>;

#[derive(Deserialize)]
struct Manifest {
    commit: String,
}

#[derive(Deserialize)]
struct NewInstallation {
    account: String,
    #[serde(default)]
    permissions: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct NewRepository {
    installation: u64,
    full_name: String,
    #[serde(default)]
    public: bool,
}

#[derive(Deserialize)]
struct NewToken {
    installation: u64,
    permissions: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct NewPull {
    repository: String,
    base: String,
    head: String,
    fork: Option<String>,
}

#[derive(Deserialize)]
struct Hook {
    url: String,
}

#[derive(Deserialize)]
struct Who {
    login: String,
    id: u64,
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Delivery {
    Installation {
        installation: u64,
        action: String,
    },
    Push {
        repository: String,
        r#ref: String,
        before: String,
        after: String,
        sender: Who,
    },
    PullRequest {
        repository: String,
        number: u64,
        action: String,
        sender: Who,
    },
}

fn main() {
    let (data, actions) = arguments();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("the fake forge has no runtime: {error}"))
        .block_on(serve(data, actions));
}

fn arguments() -> (PathBuf, PathBuf) {
    let mut data = None;
    let mut actions = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--data") => data = arguments.next().map(PathBuf::from),
            Some("--actions") => actions = arguments.next().map(PathBuf::from),
            _ => usage(),
        }
    }
    match (data, actions) {
        (Some(data), Some(actions)) => (data, actions),
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!("usage: bosun-fake-forge --data <directory> --actions <directory>");
    std::process::exit(2);
}

async fn serve(data: PathBuf, actions: PathBuf) {
    let forge = FakeForge::serving_git(data).await;
    vendor(&forge, &actions);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("the control of the fake forge cannot listen: {error}"));
    let control = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("the control of the fake forge has no address: {error}"));
    let api = forge.api_url();
    let address = format!(
        "{}:{}",
        api.host_str().unwrap_or_default(),
        api.port().unwrap_or_default()
    );
    println!(
        "{}",
        json!({
            "message": "listening",
            "forge": address,
            "control": control.to_string(),
            "app_id": APP_ID,
            "webhook_secret": forge.webhook_secret(),
        })
    );
    let router = Router::new()
        .route("/installations", post(install))
        .route("/repositories", post(create_repository))
        .route("/tokens", post(mint))
        .route("/pulls", post(open_pull_request))
        .route("/hook", put(hook))
        .route("/deliveries", post(deliver))
        .route("/check-runs", get(check_runs))
        .route("/statuses", get(statuses))
        .with_state(Arc::new(Control {
            forge,
            hook: Mutex::new(None),
        }));
    axum::serve(listener, router)
        .await
        .unwrap_or_else(|error| panic!("the control of the fake forge stopped: {error}"));
}

fn vendor(forge: &FakeForge, root: &Path) {
    for (owner, owned) in directories(root) {
        let installation = forge.install(&owner, READ);
        for (name, versions) in directories(&owned) {
            let full_name = format!("{owner}/{name}");
            forge.create_repository(installation, &full_name);
            forge.publish(&full_name);
            for (tag, vendored) in directories(&versions) {
                let manifest = vendored.join(MANIFEST);
                let text = fs::read(&manifest).unwrap_or_else(|error| {
                    panic!("{} cannot be read: {error}", manifest.display())
                });
                let manifest: Manifest = serde_json::from_slice(&text).unwrap_or_else(|error| {
                    panic!("{} is not a manifest: {error}", manifest.display())
                });
                let files = unpack(&vendored.join(TARBALL));
                forge.vendor(&full_name, &tag, &manifest.commit, files);
            }
        }
    }
}

fn directories(parent: &Path) -> Vec<(String, PathBuf)> {
    let entries = fs::read_dir(parent)
        .unwrap_or_else(|error| panic!("{} cannot be listed: {error}", parent.display()));
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((name, path))
        })
        .collect();
    found.sort();
    found
}

fn unpack(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = File::open(path)
        .unwrap_or_else(|error| panic!("{} cannot be read: {error}", path.display()));
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let entries = archive
        .entries()
        .unwrap_or_else(|error| panic!("{} is not a tarball: {error}", path.display()));
    let mut files = Vec::new();
    for entry in entries {
        let mut entry =
            entry.unwrap_or_else(|error| panic!("{} is broken: {error}", path.display()));
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry
            .path()
            .unwrap_or_else(|error| panic!("{} names no path: {error}", path.display()))
            .to_string_lossy()
            .into_owned();
        let Some((_, inner)) = name.split_once('/') else {
            continue;
        };
        let inner = inner.to_owned();
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .unwrap_or_else(|error| panic!("{inner} of {} is broken: {error}", path.display()));
        files.push((inner, content));
    }
    files
}

fn permissions(listed: &BTreeMap<String, String>) -> Vec<(&str, &str)> {
    listed
        .iter()
        .map(|(name, access)| (name.as_str(), access.as_str()))
        .collect()
}

async fn install(
    State(control): Shared,
    Json(body): Json<NewInstallation>,
) -> (StatusCode, Json<Value>) {
    let id = control
        .forge
        .install(&body.account, &permissions(&body.permissions));
    (StatusCode::CREATED, Json(json!({ "id": id })))
}

async fn create_repository(
    State(control): Shared,
    Json(body): Json<NewRepository>,
) -> (StatusCode, Json<Value>) {
    let id = control
        .forge
        .create_repository(body.installation, &body.full_name);
    if body.public {
        control.forge.publish(&body.full_name);
    }
    (StatusCode::CREATED, Json(json!({ "id": id })))
}

async fn mint(State(control): Shared, Json(body): Json<NewToken>) -> (StatusCode, Json<Value>) {
    let token = control
        .forge
        .mint(body.installation, &permissions(&body.permissions));
    (StatusCode::CREATED, Json(json!({ "token": token })))
}

async fn open_pull_request(
    State(control): Shared,
    Json(body): Json<NewPull>,
) -> (StatusCode, Json<Value>) {
    let number = match &body.fork {
        Some(owner) => {
            control
                .forge
                .open_fork_pull_request(&body.repository, owner, &body.base, &body.head)
        }
        None => control
            .forge
            .open_pull_request(&body.repository, &body.base, &body.head),
    };
    (StatusCode::CREATED, Json(json!({ "number": number })))
}

async fn hook(State(control): Shared, Json(body): Json<Hook>) -> StatusCode {
    *control.hook.lock().unwrap_or_else(PoisonError::into_inner) = Some(body.url);
    StatusCode::NO_CONTENT
}

async fn deliver(State(control): Shared, Json(body): Json<Delivery>) -> (StatusCode, Json<Value>) {
    let Some(url) = control
        .hook
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "message": "the App has no webhook address; PUT /hook sets it" })),
        );
    };
    let forge = &control.forge;
    let (event, payload) = match &body {
        Delivery::Installation {
            installation,
            action,
        } => (
            "installation",
            forge.installation_event(*installation, action),
        ),
        Delivery::Push {
            repository,
            r#ref,
            before,
            after,
            sender,
        } => (
            "push",
            forge.push_event(repository, r#ref, before, after, sender.as_sender()),
        ),
        Delivery::PullRequest {
            repository,
            number,
            action,
            sender,
        } => (
            "pull_request",
            forge.pull_request_event(repository, *number, action, sender.as_sender()),
        ),
    };
    let webhook = forge.webhook(event, &payload);
    let status = webhook.send(&url).await;
    (
        StatusCode::OK,
        Json(json!({ "delivery": webhook.delivery(), "status": status })),
    )
}

async fn check_runs(State(control): Shared) -> Json<Vec<RecordedCheckRun>> {
    Json(control.forge.check_runs())
}

async fn statuses(State(control): Shared) -> Json<Vec<RecordedStatus>> {
    Json(control.forge.statuses())
}

impl Who {
    fn as_sender(&self) -> Sender<'_> {
        Sender {
            login: &self.login,
            id: self.id,
        }
    }
}
