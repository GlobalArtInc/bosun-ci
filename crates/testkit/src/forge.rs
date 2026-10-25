mod api;
mod events;
mod git;
mod jwt;
mod transport;
mod webhook;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use ring::signature::RsaKeyPair;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use url::Url;
use uuid::Uuid;

pub use events::{NO_COMMIT, Sender};
pub use webhook::Webhook;

use self::git::Tree;

pub const APP_ID: u64 = 4242;

pub const APP_SLUG: &str = "bosun-fake";

pub const BOT_ID: u64 = 424_242;

pub const BOT: Sender<'static> = Sender {
    login: "bosun-fake[bot]",
    id: BOT_ID,
};

pub const APP_KEY: &str = include_str!("../keys/app.pem");

pub const FOREIGN_KEY: &str = include_str!("../keys/foreign.pem");

pub const COMPARE_FILES: usize = 300;

pub const PULL_REQUEST_FILES: usize = 3000;

const DEFAULT_BRANCH: &str = "main";

const ALL_REPOSITORIES: &str = "all";

const TOKEN_LIFETIME: TimeDelta = TimeDelta::hours(1);

pub struct FakeForge {
    shared: Arc<Shared>,
    server: JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedToken {
    pub token: String,
    pub installation: u64,
    pub repository_selection: String,
    pub repositories: Vec<String>,
    pub permissions: BTreeMap<String, String>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedCheckRun {
    pub id: u64,
    pub repository: String,
    pub head_sha: String,
    pub name: String,
    pub details_url: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
    pub output: Option<RecordedOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedOutput {
    pub title: String,
    pub summary: String,
    pub annotations: Vec<RecordedAnnotation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedAnnotation {
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub start_column: Option<u64>,
    pub end_column: Option<u64>,
    pub annotation_level: String,
    pub message: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub body: Option<serde_json::Value>,
    pub status: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDownload {
    pub repository: String,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedStatus {
    pub repository: String,
    pub sha: String,
    pub context: String,
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
}

struct Shared {
    api: Url,
    git: Option<PathBuf>,
    public_key: Vec<u8>,
    webhook_secret: String,
    http: reqwest::Client,
    state: Mutex<State>,
}

struct State {
    next_id: u64,
    delay: Duration,
    failing: bool,
    requests: Vec<RecordedRequest>,
    token_lifetime: TimeDelta,
    installations: BTreeMap<u64, Installation>,
    repositories: BTreeMap<String, Repository>,
    tokens: Vec<MintedToken>,
    check_runs: Vec<RecordedCheckRun>,
    statuses: Vec<RecordedStatus>,
    downloads: Vec<RecordedDownload>,
    buckets: HashMap<u64, Bucket>,
}

struct Installation {
    account_id: u64,
    account: String,
    permissions: BTreeMap<String, String>,
}

struct Repository {
    id: u64,
    installation: u64,
    owner: String,
    name: String,
    default_branch: String,
    public: bool,
    refs: BTreeMap<String, String>,
    commits: HashMap<String, Tree>,
    parents: HashMap<String, Vec<String>>,
    blobs: HashMap<String, Vec<u8>>,
    pulls: BTreeMap<u64, Pull>,
    merges_held: bool,
}

struct Pull {
    base: String,
    head: String,
    fork: Option<Fork>,
}

struct Fork {
    id: u64,
    owner: String,
}

struct Bucket {
    limit: u32,
    remaining: u32,
    reset: DateTime<Utc>,
}

impl FakeForge {
    pub async fn start() -> FakeForge {
        FakeForge::launch(None).await
    }

    pub async fn serving_git(root: PathBuf) -> FakeForge {
        fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("{} cannot be created: {error}", root.display()));
        FakeForge::launch(Some(root)).await
    }

    async fn launch(git: Option<PathBuf>) -> FakeForge {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("the fake forge cannot listen: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("the fake forge has no address: {error}"));
        let api = Url::parse(&format!("http://{address}/api/v3"))
            .unwrap_or_else(|error| panic!("the fake forge's address is not a URL: {error}"));
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap_or_else(|error| panic!("the fake forge has no HTTP client: {error}"));
        let shared = Arc::new(Shared {
            api,
            git,
            public_key: public_key(APP_KEY),
            webhook_secret: Uuid::now_v7().simple().to_string(),
            http,
            state: Mutex::new(State {
                next_id: 1,
                delay: Duration::ZERO,
                failing: false,
                requests: Vec::new(),
                token_lifetime: TOKEN_LIFETIME,
                installations: BTreeMap::new(),
                repositories: BTreeMap::new(),
                tokens: Vec::new(),
                check_runs: Vec::new(),
                statuses: Vec::new(),
                downloads: Vec::new(),
                buckets: HashMap::new(),
            }),
        });
        let router = api::router(Arc::clone(&shared));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        FakeForge { shared, server }
    }

    pub fn api_url(&self) -> Url {
        self.shared.api.clone()
    }

    pub fn webhook_secret(&self) -> &str {
        &self.shared.webhook_secret
    }

    pub fn install(&self, account: &str, permissions: &[(&str, &str)]) -> u64 {
        let mut state = self.shared.lock();
        let id = state.allocate();
        let account_id = state.allocate();
        state.installations.insert(
            id,
            Installation {
                account_id,
                account: account.to_owned(),
                permissions: permissions
                    .iter()
                    .map(|(name, access)| ((*name).to_owned(), (*access).to_owned()))
                    .collect(),
            },
        );
        id
    }

    pub fn create_repository(&self, installation: u64, full_name: &str) -> u64 {
        let (owner, name) = full_name
            .split_once('/')
            .unwrap_or_else(|| panic!("`{full_name}` is not <owner>/<name>"));
        let mut state = self.shared.lock();
        let account = &state
            .installations
            .get(&installation)
            .unwrap_or_else(|| panic!("the fake forge has no installation {installation}"))
            .account;
        assert!(
            account.eq_ignore_ascii_case(owner),
            "installation {installation} belongs to `{account}`, which does not own {full_name}"
        );
        let id = state.allocate();
        if let Some(root) = &self.shared.git {
            transport::create(root, id, DEFAULT_BRANCH);
        }
        state.repositories.insert(
            full_name.to_owned(),
            Repository {
                id,
                installation,
                owner: owner.to_owned(),
                name: name.to_owned(),
                default_branch: DEFAULT_BRANCH.to_owned(),
                public: false,
                refs: BTreeMap::new(),
                commits: HashMap::new(),
                parents: HashMap::new(),
                blobs: HashMap::new(),
                pulls: BTreeMap::new(),
                merges_held: false,
            },
        );
        id
    }

    pub fn publish(&self, full_name: &str) {
        self.shared.lock().repository(full_name).public = true;
    }

    pub fn rename_repository(&self, full_name: &str, name: &str) {
        let mut state = self.shared.lock();
        let mut repository = state
            .repositories
            .remove(full_name)
            .unwrap_or_else(|| panic!("the fake forge has no repository {full_name}"));
        let renamed = format!("{}/{name}", repository.owner);
        assert!(
            !state.repositories.contains_key(&renamed),
            "the fake forge has a repository {renamed} already"
        );
        for token in &mut state.tokens {
            if token.installation == repository.installation {
                for listed in &mut token.repositories {
                    if *listed == repository.name {
                        name.clone_into(listed);
                    }
                }
            }
        }
        name.clone_into(&mut repository.name);
        state.repositories.insert(renamed, repository);
    }

    pub fn commit<P, C>(
        &self,
        repository: &str,
        branch: &str,
        files: impl IntoIterator<Item = (P, C)>,
    ) -> String
    where
        P: AsRef<str>,
        C: AsRef<[u8]>,
    {
        let mut state = self.shared.lock();
        let repository = state.repository(repository);
        let reference = format!("refs/heads/{branch}");
        let parent = repository
            .refs
            .get(&reference)
            .or_else(|| {
                repository
                    .refs
                    .get(&format!("refs/heads/{}", repository.default_branch))
            })
            .cloned();
        let mut tree = parent
            .as_ref()
            .and_then(|parent| repository.commits.get(parent))
            .cloned()
            .unwrap_or_default();
        for (path, content) in files {
            let content = content.as_ref();
            let sha = git::blob_sha(content);
            repository.blobs.insert(sha.clone(), content.to_vec());
            tree.insert(path.as_ref().to_owned(), sha);
        }
        let sha = git::commit_sha(parent.as_deref(), &tree, repository.commits.len());
        repository.commits.insert(sha.clone(), tree);
        repository
            .parents
            .insert(sha.clone(), parent.into_iter().collect());
        repository.refs.insert(reference, sha.clone());
        let touched: Vec<u64> = repository
            .pulls
            .iter()
            .filter(|(_, pull)| pull.base == branch || pull.head == branch)
            .map(|(number, _)| *number)
            .collect();
        for number in touched {
            repository.merge(number);
        }
        sha
    }

    pub fn vendor<P, C>(
        &self,
        repository: &str,
        tag: &str,
        commit: &str,
        files: impl IntoIterator<Item = (P, C)>,
    ) where
        P: AsRef<str>,
        C: AsRef<[u8]>,
    {
        let mut state = self.shared.lock();
        let repository = state.repository(repository);
        let mut tree = Tree::new();
        for (path, content) in files {
            let content = content.as_ref();
            let sha = git::blob_sha(content);
            repository.blobs.insert(sha.clone(), content.to_vec());
            tree.insert(path.as_ref().to_owned(), sha);
        }
        repository.commits.insert(commit.to_owned(), tree);
        repository.parents.insert(commit.to_owned(), Vec::new());
        repository
            .refs
            .insert(format!("refs/tags/{tag}"), commit.to_owned());
    }

    pub fn tag(&self, repository: &str, tag: &str, commit: &str) {
        let mut state = self.shared.lock();
        let repository = state.repository(repository);
        assert!(
            repository.commits.contains_key(commit),
            "the fake forge has no commit {commit}"
        );
        repository
            .refs
            .insert(format!("refs/tags/{tag}"), commit.to_owned());
    }

    pub fn open_pull_request(&self, repository: &str, base: &str, head: &str) -> u64 {
        self.open(repository, base, head, None)
    }

    pub fn open_fork_pull_request(
        &self,
        repository: &str,
        owner: &str,
        base: &str,
        head: &str,
    ) -> u64 {
        self.open(repository, base, head, Some(owner))
    }

    fn open(&self, repository: &str, base: &str, head: &str, fork: Option<&str>) -> u64 {
        let mut state = self.shared.lock();
        let number = state.allocate();
        let fork = fork.map(|owner| Fork {
            id: state.allocate(),
            owner: owner.to_owned(),
        });
        let repository = state.repository(repository);
        repository.pulls.insert(
            number,
            Pull {
                base: base.to_owned(),
                head: head.to_owned(),
                fork,
            },
        );
        repository.merge(number);
        number
    }

    pub fn hold_merges(&self, repository: &str, held: bool) {
        let mut state = self.shared.lock();
        let repository = state.repository(repository);
        repository.merges_held = held;
        let numbers: Vec<u64> = repository.pulls.keys().copied().collect();
        for number in numbers {
            repository.merge(number);
        }
    }

    pub fn merge_commit(&self, repository: &str, number: u64) -> String {
        let mut state = self.shared.lock();
        state
            .repository(repository)
            .refs
            .get(&format!("refs/pull/{number}/merge"))
            .cloned()
            .unwrap_or_else(|| panic!("pull request {number} has no merge commit"))
    }

    pub fn limit_rate(&self, installation: u64, remaining: u32, reset: DateTime<Utc>) {
        self.shared.lock().buckets.insert(
            installation,
            Bucket {
                limit: remaining,
                remaining,
                reset,
            },
        );
    }

    pub fn delay(&self, delay: Duration) {
        self.shared.lock().delay = delay;
    }

    pub fn fail(&self, failing: bool) {
        self.shared.lock().failing = failing;
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.lock().requests.clone()
    }

    pub fn set_token_lifetime(&self, lifetime: Duration) {
        self.shared.lock().token_lifetime = TimeDelta::from_std(lifetime)
            .unwrap_or_else(|error| panic!("{lifetime:?} is not a token lifetime: {error}"));
    }

    pub fn mint(&self, installation: u64, permissions: &[(&str, &str)]) -> String {
        let mut state = self.shared.lock();
        assert!(
            state.installations.contains_key(&installation),
            "the fake forge has no installation {installation}"
        );
        let token = format!("ghs_{}", Uuid::now_v7().simple());
        let repositories = state.installation_repositories(installation);
        let expires_at = Utc::now() + state.token_lifetime;
        state.tokens.push(MintedToken {
            token: token.clone(),
            installation,
            repository_selection: ALL_REPOSITORIES.to_owned(),
            repositories: repositories.into_iter().collect(),
            permissions: permissions
                .iter()
                .map(|(name, access)| ((*name).to_owned(), (*access).to_owned()))
                .collect(),
            expires_at,
            revoked: false,
        });
        token
    }

    pub fn tokens(&self) -> Vec<MintedToken> {
        self.shared.lock().tokens.clone()
    }

    pub fn check_runs(&self) -> Vec<RecordedCheckRun> {
        self.shared.lock().check_runs.clone()
    }

    pub fn statuses(&self) -> Vec<RecordedStatus> {
        self.shared.lock().statuses.clone()
    }

    pub fn downloads(&self) -> Vec<RecordedDownload> {
        self.shared.lock().downloads.clone()
    }

    pub fn webhook(&self, event: &str, payload: &serde_json::Value) -> Webhook {
        Webhook::sign(
            self.shared.http.clone(),
            &self.shared.webhook_secret,
            event,
            payload,
        )
    }
}

impl Drop for FakeForge {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn allocate(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn repository(&mut self, full_name: &str) -> &mut Repository {
        self.repositories
            .get_mut(full_name)
            .unwrap_or_else(|| panic!("the fake forge has no repository {full_name}"))
    }

    fn installation_repositories(&self, installation: u64) -> BTreeSet<String> {
        self.repositories
            .values()
            .filter(|repository| repository.installation == installation)
            .map(|repository| repository.name.clone())
            .collect()
    }

    fn installation(&self, installation: u64) -> &Installation {
        self.installations
            .get(&installation)
            .unwrap_or_else(|| panic!("the fake forge has no installation {installation}"))
    }
}

impl MintedToken {
    fn covers(&self, repository: &Repository) -> bool {
        repository.installation == self.installation
            && (self.repository_selection == ALL_REPOSITORIES
                || self.repositories.contains(&repository.name))
    }
}

impl Repository {
    fn resolve(&self, reference: &str) -> Option<&str> {
        if let Some((sha, _)) = self.commits.get_key_value(reference) {
            return Some(sha);
        }
        let candidates = if reference.starts_with("refs/") {
            vec![reference.to_owned()]
        } else {
            vec![
                format!("refs/heads/{reference}"),
                format!("refs/tags/{reference}"),
            ]
        };
        candidates
            .iter()
            .find_map(|candidate| self.refs.get(candidate))
            .map(String::as_str)
    }

    fn merge(&mut self, number: u64) {
        let Some(pull) = self.pulls.get(&number) else {
            return;
        };
        let branch = |name: &str| self.refs.get(&format!("refs/heads/{name}")).cloned();
        let (Some(base), Some(head)) = (branch(&pull.base), branch(&pull.head)) else {
            return;
        };
        self.refs
            .insert(format!("refs/pull/{number}/head"), head.clone());
        let merged = self
            .refs
            .get(&format!("refs/pull/{number}/merge"))
            .and_then(|merge| self.parents.get(merge))
            .is_some_and(|parents| *parents == [base.clone(), head.clone()]);
        if self.merges_held || merged {
            return;
        }
        let mut tree = self.commits.get(&base).cloned().unwrap_or_default();
        if let Some(changed) = self.commits.get(&head) {
            tree.extend(
                changed
                    .iter()
                    .map(|(path, sha)| (path.clone(), sha.clone())),
            );
        }
        let merge = git::commit_sha(Some(&base), &tree, self.commits.len());
        self.commits.insert(merge.clone(), tree);
        self.parents.insert(merge.clone(), vec![base, head]);
        self.refs.insert(format!("refs/pull/{number}/merge"), merge);
    }

    fn tree(&self, reference: &str) -> Option<(&str, &Tree)> {
        let sha = self.resolve(reference)?;
        self.commits.get(sha).map(|tree| (sha, tree))
    }
}

pub fn fixture_actions() -> Vec<(String, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/fixtures/actions");
    let mut files = Vec::new();
    collect(&root, "", &mut files);
    files.sort();
    files
}

fn collect(directory: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} cannot be listed: {error}", directory.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "node_modules" {
            continue;
        }
        let path = entry.path();
        let relative = format!("{prefix}{name}");
        if path.is_dir() {
            collect(&path, &format!("{relative}/"), files);
        } else {
            let content = fs::read(&path)
                .unwrap_or_else(|error| panic!("{} cannot be read: {error}", path.display()));
            files.push((relative, content));
        }
    }
}

fn public_key(pem: &str) -> Vec<u8> {
    let der = PrivateKeyDer::from_pem_slice(pem.as_bytes())
        .unwrap_or_else(|error| panic!("the fake App's key is not PEM: {error}"));
    let PrivateKeyDer::Pkcs1(key) = der else {
        panic!("the fake App's key is not a PKCS#1 RSA key");
    };
    let pair = RsaKeyPair::from_der(key.secret_pkcs1_der())
        .unwrap_or_else(|error| panic!("the fake App's key is not usable: {error}"));
    pair.public().as_ref().to_vec()
}
