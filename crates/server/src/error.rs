use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::owner::LOGIN_RULE;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read the configuration {}: {source}", path.display())]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the configuration {} is not valid: {source}", path.display())]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("the setting {key} ({variable}) {problem}")]
    Setting {
        key: String,
        variable: String,
        problem: String,
    },
    #[error("both keys.kek and keys.kek_file are set, and the KEK comes from exactly one of them")]
    TwoKeks,
    #[error("could not read the KEK from {}: {source}", path.display())]
    ReadKek {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the KEK in {origin} is not 32 bytes written as 64 hexadecimal digits")]
    MalformedKek { origin: String },
    #[error("could not generate the KEK in {}: {source}", path.display())]
    GenerateKek {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "the configuration names no KEK and {} does not exist, so there is no current KEK to          encrypt the data keys under",
        path.display()
    )]
    NoKek { path: PathBuf },
    #[error("a secret could not be encrypted")]
    Seal,
    #[error("the data keys of the secrets could not be re-encrypted: {0}")]
    Rewrap(#[source] bosun_db::Error),
    #[error(
        "re-encrypted the data keys of {rewrapped} secrets, but not of {}: {}",
        failures.len(),
        failures.join("; ")
    )]
    Unrewrapped {
        rewrapped: u64,
        failures: Vec<String>,
    },
    #[error("could not read the {what} of the GitHub App from {}: {source}", path.display())]
    ReadGitHubFile {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the webhook secret of the GitHub App in {} is empty", path.display())]
    EmptyWebhookSecret { path: PathBuf },
    #[error("the setting oidc.{key} of the identity provider {provider} {problem}")]
    Oidc {
        provider: String,
        key: String,
        problem: String,
    },
    #[error(
        "could not read the client secret of the identity provider `{provider}` from {}: {source}",
        path.display()
    )]
    ReadClientSecret {
        provider: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the client secret of the identity provider `{provider}` in {} is empty", path.display())]
    EmptyClientSecret { provider: String, path: PathBuf },
    #[error("the identity providers cannot be used: {0}")]
    IdentityProviders(String),
    #[error("the GitHub App cannot be used: {0}")]
    GitHub(#[source] bosun_forge::Error),
    #[error("the object store {location} cannot be used: {problem}")]
    Store { location: String, problem: String },
    #[error("could not connect to the database: {0}")]
    Connect(#[source] bosun_db::Error),
    #[error("the schema could not be brought up to date: {0}")]
    Migrate(#[source] bosun_db::Error),
    #[error("could not listen for the notifications of other replicas: {0}")]
    Notifications(#[source] bosun_db::Error),
    #[error("could not listen on {address}: {source}")]
    Listen {
        address: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("the server stopped on an error: {0}")]
    Serve(#[source] io::Error),
    #[error("`{login}` is not a login: {LOGIN_RULE}")]
    InvalidLogin { login: String },
    #[error("could not create the owner: {0}")]
    CreateOwner(#[source] bosun_db::Error),
    #[error("could not create the owner of bootstrap.admin (BOSUN_BOOTSTRAP_ADMIN): {0}")]
    Bootstrap(#[source] bosun_db::Error),
    #[error("could not create the first owner admin: {0}")]
    FirstOwner(#[source] bosun_db::Error),
    #[error(
        "created the owner admin, but could not write {}: {source}; create another owner with \
         bosun-server admin create <login>",
        path.display()
    )]
    FirstStartFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not make the password of the first owner admin: {0}")]
    Password(String),
    #[error("server.listen names port 0, so there is no port at which to check the server")]
    CheckAnyPort,
    #[error("the server at {address} does not answer: {reason}")]
    Unanswered { address: SocketAddr, reason: String },
    #[error("the server at {address} is not ready: {reason}")]
    Unready { address: SocketAddr, reason: String },
    #[error("the operating system gave no random bytes: {0}")]
    Random(#[source] getrandom::Error),
    #[error("the downloads of actions and tools cannot start: {0}")]
    Tools(String),
}
