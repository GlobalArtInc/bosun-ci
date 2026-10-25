mod api;
mod events;
mod git;
mod jwt;
mod webhook;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use ring::signature::RsaKeyPair;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCheckRun {
    pub id: u64,
    pub repository: String,
    pub head_sha: String,
    pub name: String,
    pub details_url: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    public_key: Vec<u8>,
    webhook_secret: String,
    http: reqwest::Client,
    state: Mutex<State>,
}

struct State {
    next_id: u64,
    delay: Duration,
    token_lifetime: TimeDelta,
    installations: BTreeMap<u64, Installation>,
    repositories: BTreeMap<String, Repository>,
    tokens: Vec<MintedToken>,
    check_runs: Vec<RecordedCheckRun>,
    statuses: Vec<RecordedStatus>,
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
    refs: BTreeMap<String, String>,
    commits: HashMap<String, Tree>,
    blobs: HashMap<String, Vec<u8>>,
    pulls: BTreeMap<u64, Pull>,
}

struct Pull {
    base: String,
    head: String,
}

struct Bucket {
    limit: u32,
    remaining: u32,
    reset: DateTime<Utc>,
}

impl FakeForge {
    pub async fn start() -> FakeForge {
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
            public_key: public_key(APP_KEY),
            webhook_secret: Uuid::now_v7().simple().to_string(),
            http,
            state: Mutex::new(State {
                next_id: 1,
                delay: Duration::ZERO,
                token_lifetime: TOKEN_LIFETIME,
                installations: BTreeMap::new(),
                repositories: BTreeMap::new(),
                tokens: Vec::new(),
                check_runs: Vec::new(),
                statuses: Vec::new(),
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
        state.repositories.insert(
            full_name.to_owned(),
            Repository {
                id,
                installation,
                owner: owner.to_owned(),
                name: name.to_owned(),
                default_branch: DEFAULT_BRANCH.to_owned(),
                refs: BTreeMap::new(),
                commits: HashMap::new(),
                blobs: HashMap::new(),
                pulls: BTreeMap::new(),
            },
        );
        id
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
        repository.refs.insert(reference, sha.clone());
        sha
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
        let mut state = self.shared.lock();
        let number = state.allocate();
        state.repository(repository).pulls.insert(
            number,
            Pull {
                base: base.to_owned(),
                head: head.to_owned(),
            },
        );
        number
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

    pub fn set_token_lifetime(&self, lifetime: Duration) {
        self.shared.lock().token_lifetime = TimeDelta::from_std(lifetime)
            .unwrap_or_else(|error| panic!("{lifetime:?} is not a token lifetime: {error}"));
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

    fn tree(&self, reference: &str) -> Option<(&str, &Tree)> {
        let sha = self.resolve(reference)?;
        self.commits.get(sha).map(|tree| (sha, tree))
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
