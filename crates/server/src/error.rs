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
    #[error(
        "no KEK is configured, and the server keeps every secret under it: set keys.kek_file \
         in server.toml, BOSUN_KEYS_KEK_FILE or BOSUN_KEYS_KEK"
    )]
    NoKek,
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
    #[error("the operating system gave no random bytes for a token: {0}")]
    Random(#[source] getrandom::Error),
}
