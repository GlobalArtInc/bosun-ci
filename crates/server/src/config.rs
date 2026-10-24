use std::env::{self, VarError};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Error;
use crate::owner::{self, LOGIN_RULE};

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080);

const DEFAULT_MAX_CONNECTIONS: u32 = 32;

const MIGRATION_CONNECTIONS: u32 = 2;

pub struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) database: Database,
    pub(crate) keys: Keys,
    pub(crate) bootstrap_admin: Option<String>,
}

pub(crate) struct Database {
    pub(crate) url: String,
    pub(crate) max_connections: u32,
}

pub(crate) struct Keys {
    pub(crate) kek: Option<String>,
    pub(crate) kek_file: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    server: ServerFile,
    database: DatabaseFile,
    keys: KeysFile,
    bootstrap: BootstrapFile,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerFile {
    listen: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DatabaseFile {
    url: Option<String>,
    max_connections: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct KeysFile {
    kek: Option<String>,
    kek_file: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BootstrapFile {
    admin: Option<String>,
}

impl Config {
    pub fn load(file: Option<&Path>) -> Result<Config, Error> {
        let file = match file {
            Some(path) => read(path)?,
            None => File::default(),
        };
        let listen = match variable("server", "listen")?.or(file.server.listen) {
            Some(text) => text.parse().map_err(|_| {
                invalid(
                    "server",
                    "listen",
                    format!("is `{text}`, which is not an address and a port such as 0.0.0.0:8080"),
                )
            })?,
            None => DEFAULT_LISTEN,
        };
        let url = variable("database", "url")?
            .or(file.database.url)
            .ok_or_else(|| invalid("database", "url", "is not set".to_owned()))?;
        let max_connections = match variable("database", "max_connections")? {
            Some(text) => text.parse().map_err(|_| {
                invalid(
                    "database",
                    "max_connections",
                    format!("is `{text}`, which is not a number of connections"),
                )
            })?,
            None => file
                .database
                .max_connections
                .unwrap_or(DEFAULT_MAX_CONNECTIONS),
        };
        if max_connections < MIGRATION_CONNECTIONS {
            return Err(invalid(
                "database",
                "max_connections",
                format!(
                    "is {max_connections}, and the migrations at start hold \
                     {MIGRATION_CONNECTIONS} connections of the pool at once"
                ),
            ));
        }
        let keys = Keys {
            kek: variable("keys", "kek")?.or(file.keys.kek),
            kek_file: variable("keys", "kek_file")?
                .map(PathBuf::from)
                .or(file.keys.kek_file),
        };
        let bootstrap_admin = variable("bootstrap", "admin")?.or(file.bootstrap.admin);
        if let Some(login) = bootstrap_admin
            .as_deref()
            .filter(|login| !owner::is_login(login))
        {
            return Err(invalid(
                "bootstrap",
                "admin",
                format!("is `{login}`, which is not a login: {LOGIN_RULE}"),
            ));
        }
        Ok(Config {
            listen,
            database: Database {
                url,
                max_connections,
            },
            keys,
            bootstrap_admin,
        })
    }
}

fn read(path: &Path) -> Result<File, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::ReadConfig {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| Error::ParseConfig {
        path: path.to_owned(),
        source,
    })
}

fn name(section: &str, key: &str) -> String {
    format!("BOSUN_{section}_{key}").to_uppercase()
}

fn variable(section: &str, key: &str) -> Result<Option<String>, Error> {
    match env::var(name(section, key)) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(invalid(section, key, "is not UTF-8".to_owned())),
    }
}

fn invalid(section: &str, key: &str, problem: String) -> Error {
    Error::Setting {
        key: format!("{section}.{key}"),
        variable: name(section, key),
        problem,
    }
}
