mod github;

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bosun_types::status::Conclusion;
use chrono::{DateTime, Utc};
use tokio::io::{AsyncRead, ReadBuf};

pub use github::GitHubApp;

pub trait Forge: Send + Sync {
    fn app(&self) -> impl Future<Output = Result<App, Error>> + Send;

    fn resolve(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> impl Future<Output = Result<Option<String>, Error>> + Send;

    fn file(
        &self,
        repository: &Repository,
        commit: &str,
        path: &str,
    ) -> impl Future<Output = Result<Option<File>, Error>> + Send;

    fn directory(
        &self,
        repository: &Repository,
        commit: &str,
        path: &str,
    ) -> impl Future<Output = Result<Option<Vec<Entry>>, Error>> + Send;

    fn compare(
        &self,
        repository: &Repository,
        base: &str,
        head: &str,
    ) -> impl Future<Output = Result<Comparison, Error>> + Send;

    fn pull_request_files(
        &self,
        repository: &Repository,
        number: u64,
    ) -> impl Future<Output = Result<Vec<String>, Error>> + Send;

    fn installation_repositories(
        &self,
        installation: u64,
    ) -> impl Future<Output = Result<Vec<InstalledRepository>, Error>> + Send;

    fn mint_token(
        &self,
        installation: u64,
        repositories: &[String],
        permissions: &Permissions,
    ) -> impl Future<Output = Result<InstallationToken, Error>> + Send;

    fn revoke_token(&self, token: &str) -> impl Future<Output = Result<(), Error>> + Send;

    fn create_check_run(
        &self,
        repository: &Repository,
        head_sha: &str,
        check_run: &CheckRun,
    ) -> impl Future<Output = Result<u64, Error>> + Send;

    fn update_check_run(
        &self,
        repository: &Repository,
        id: u64,
        check_run: &CheckRun,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    fn create_status(
        &self,
        repository: &Repository,
        commit: &str,
        status: &CommitStatus,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    fn tarball(
        &self,
        repository: &Repository,
        commit: &str,
    ) -> impl Future<Output = Result<Download, Error>> + Send;

    fn resolve_public(
        &self,
        owner: &str,
        name: &str,
        reference: &str,
    ) -> impl Future<Output = Result<Option<String>, Error>> + Send;

    fn public_tarball(
        &self,
        owner: &str,
        name: &str,
        commit: &str,
    ) -> impl Future<Output = Result<Download, Error>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Repository {
    pub installation: u64,
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRepository {
    pub id: u64,
    pub owner: String,
    pub name: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub slug: String,
    pub bot: Account,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: u64,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub sha: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Submodule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub files: Vec<String>,
    pub truncated: bool,
}

pub type Permissions = BTreeMap<String, Access>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Clone, PartialEq, Eq)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for InstallationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallationToken")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    pub details_url: Option<String>,
    pub status: CheckStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Queued,
    InProgress,
    Completed(Conclusion),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStatus {
    pub context: String,
    pub state: StatusState,
    pub description: Option<String>,
    pub target_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Pending,
    Success,
    Failure,
    Error,
}

pub struct Download {
    body: Pin<Box<dyn AsyncRead + Send>>,
}

impl Download {
    fn new(body: impl AsyncRead + Send + 'static) -> Download {
        Download {
            body: Box::pin(body),
        }
    }
}

impl AsyncRead for Download {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.body.as_mut().poll_read(cx, buf)
    }
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the forge is not configured correctly: {0}")]
    Config(String),
    #[error("an installation token needs at least one repository and one permission")]
    Unscoped,
    #[error("the forge refuses requests until {reset}")]
    RateLimited { reset: DateTime<Utc> },
    #[error("the forge answered {status}: {message}")]
    Status { status: u16, message: String },
    #[error("the forge could not be reached: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("the forge answered with a body Bosun cannot read: {0}")]
    Body(String),
}
