use std::env::{self, VarError};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bosun_blob::S3Settings;
use ipnet::IpNet;
use serde::Deserialize;

use crate::Error;
use crate::address;
use crate::owner::{self, LOGIN_RULE};

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080);

const DEFAULT_MAX_CONNECTIONS: u32 = 32;

const MIGRATION_CONNECTIONS: u32 = 2;

const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

const DEFAULT_OFFLINE_AFTER: Duration = Duration::from_secs(60);

const DEFAULT_AGENT_GRACE: Duration = Duration::from_secs(600);

const DEFAULT_ASSIGNMENT_LEASE: Duration = Duration::from_secs(30);

const DEFAULT_MAX_QUEUE_TIME: Duration = Duration::from_secs(24 * 3600);

const DEFAULT_DATA_DIR: &str = "/var/lib/bosun";

const DEFAULT_AGENT_BINARIES: &str = "/usr/lib/bosun/agent";

const SYSTEM_FILE: &str = "/etc/bosun/server.toml";

const DATA_FILE: &str = "etc/bosun.toml";

const STORE: &str = "store";

const DEFAULT_FAILED_SIGN_INS: u32 = 10;

const DIRECTORY: &str = "directory";

const S3: &str = "s3";

const DEFAULT_REGION: &str = "us-east-1";

const AWS_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";

const AWS_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";

const DURATION_RULE: &str = "a duration is a whole number followed by ms, s, m or h, such as 30s";

pub struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) data_dir: PathBuf,
    pub(crate) trusted_proxies: Vec<IpNet>,
    pub(crate) database: Database,
    pub(crate) keys: Keys,
    pub(crate) bootstrap_admin: Option<String>,
    pub(crate) agents: Agents,
    pub(crate) queue: Queue,
    pub(crate) store: Store,
    pub(crate) agent_binaries: PathBuf,
    pub(crate) failed_sign_ins: u32,
}

pub(crate) enum Store {
    Directory(PathBuf),
    S3(S3Settings),
}

#[derive(Clone, Copy)]
pub(crate) struct Queue {
    pub(crate) lease: Duration,
    pub(crate) max_queue_time: Duration,
}

#[derive(Clone, Copy)]
pub(crate) struct Agents {
    pub(crate) heartbeat_interval: Duration,
    pub(crate) offline_after: Duration,
    pub(crate) grace: Duration,
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
    agents: AgentsFile,
    limits: LimitsFile,
    store: StoreFile,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerFile {
    listen: Option<String>,
    data_dir: Option<PathBuf>,
    trusted_proxies: Option<Vec<String>>,
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

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AgentsFile {
    heartbeat_interval: Option<String>,
    offline_after: Option<String>,
    assignment_lease: Option<String>,
    binary_dir: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StoreFile {
    kind: Option<String>,
    path: Option<PathBuf>,
    endpoint: Option<String>,
    bucket: Option<String>,
    region: Option<String>,
    path_style: Option<bool>,
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LimitsFile {
    agent_grace: Option<String>,
    max_queue_time: Option<String>,
    failed_sign_ins: Option<u32>,
}

impl Config {
    pub fn default_file() -> Option<PathBuf> {
        let system = PathBuf::from(SYSTEM_FILE);
        if system.is_file() {
            return Some(system);
        }
        let data = env::var_os(name("server", "data_dir"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
            .join(DATA_FILE);
        data.is_file().then_some(data)
    }

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
        let data_dir = path("server", "data_dir", file.server.data_dir)?
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
        let trusted_proxies = trusted_proxies(file.server.trusted_proxies)?;
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
            kek_file: path("keys", "kek_file", file.keys.kek_file)?,
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
        let heartbeat_interval = duration(
            "agents",
            "heartbeat_interval",
            file.agents.heartbeat_interval,
            DEFAULT_HEARTBEAT_INTERVAL,
        )?;
        let offline_after = duration(
            "agents",
            "offline_after",
            file.agents.offline_after,
            DEFAULT_OFFLINE_AFTER,
        )?;
        if offline_after <= heartbeat_interval {
            return Err(invalid(
                "agents",
                "offline_after",
                format!(
                    "is {offline_after:?}, and an agent that sends a heartbeat every \
                     {heartbeat_interval:?} must not be offline before its next one"
                ),
            ));
        }
        let grace = duration(
            "limits",
            "agent_grace",
            file.limits.agent_grace,
            DEFAULT_AGENT_GRACE,
        )?;
        let lease = duration(
            "agents",
            "assignment_lease",
            file.agents.assignment_lease,
            DEFAULT_ASSIGNMENT_LEASE,
        )?;
        let max_queue_time = duration(
            "limits",
            "max_queue_time",
            file.limits.max_queue_time,
            DEFAULT_MAX_QUEUE_TIME,
        )?;
        let failed_sign_ins = match variable("limits", "failed_sign_ins")? {
            Some(text) => text.parse().map_err(|_| {
                invalid(
                    "limits",
                    "failed_sign_ins",
                    format!("is `{text}`, which is not a number of failed sign-ins"),
                )
            })?,
            None => file
                .limits
                .failed_sign_ins
                .unwrap_or(DEFAULT_FAILED_SIGN_INS),
        };
        if failed_sign_ins == 0 {
            return Err(invalid(
                "limits",
                "failed_sign_ins",
                "is 0, and a sign-in is locked only after at least one failure".to_owned(),
            ));
        }
        let store = store(file.store, &data_dir)?;
        let agent_binaries = path("agents", "binary_dir", file.agents.binary_dir)?
            .unwrap_or_else(|| PathBuf::from(DEFAULT_AGENT_BINARIES));
        Ok(Config {
            listen,
            data_dir,
            trusted_proxies,
            database: Database {
                url,
                max_connections,
            },
            keys,
            bootstrap_admin,
            agents: Agents {
                heartbeat_interval,
                offline_after,
                grace,
            },
            queue: Queue {
                lease,
                max_queue_time,
            },
            store,
            agent_binaries,
            failed_sign_ins,
        })
    }
}

fn trusted_proxies(file: Option<Vec<String>>) -> Result<Vec<IpNet>, Error> {
    let entries: Vec<String> = match variable("server", "trusted_proxies")? {
        Some(text) => text
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned)
            .collect(),
        None => file.unwrap_or_default(),
    };
    entries
        .iter()
        .map(|entry| {
            address::network(entry.trim()).ok_or_else(|| {
                invalid(
                    "server",
                    "trusted_proxies",
                    format!(
                        "holds `{entry}`, which is neither an address nor a network such as \
                         10.0.0.0/8"
                    ),
                )
            })
        })
        .collect()
}

fn store(file: StoreFile, data_dir: &Path) -> Result<Store, Error> {
    let kind = variable("store", "kind")?
        .or(file.kind)
        .unwrap_or_else(|| DIRECTORY.to_owned());
    let path = path("store", "path", file.path)?;
    let endpoint = variable("store", "endpoint")?.or(file.endpoint);
    let bucket = variable("store", "bucket")?.or(file.bucket);
    let region = variable("store", "region")?.or(file.region);
    let path_style = match variable("store", "path_style")? {
        Some(text) => Some(boolean("store", "path_style", &text)?),
        None => file.path_style,
    };
    let access_key_id = variable("store", "access_key_id")?.or(file.access_key_id);
    let secret_access_key = variable("store", "secret_access_key")?.or(file.secret_access_key);
    match kind.as_str() {
        DIRECTORY => {
            let s3 = [
                ("endpoint", endpoint.is_some()),
                ("bucket", bucket.is_some()),
                ("region", region.is_some()),
                ("path_style", path_style.is_some()),
                ("access_key_id", access_key_id.is_some()),
                ("secret_access_key", secret_access_key.is_some()),
            ];
            if let Some((key, _)) = s3.into_iter().find(|(_, set)| *set) {
                return Err(misplaced(key, S3, DIRECTORY));
            }
            Ok(Store::Directory(
                path.unwrap_or_else(|| data_dir.join(STORE)),
            ))
        }
        S3 => {
            if path.is_some() {
                return Err(misplaced("path", DIRECTORY, S3));
            }
            let endpoint = endpoint.ok_or_else(|| needed("endpoint"))?;
            let bucket = bucket.ok_or_else(|| needed("bucket"))?;
            let (access_key_id, secret_access_key) = credentials(access_key_id, secret_access_key)?;
            Ok(Store::S3(S3Settings {
                endpoint,
                bucket,
                region: region.unwrap_or_else(|| DEFAULT_REGION.to_owned()),
                path_style: path_style.unwrap_or(false),
                access_key_id,
                secret_access_key,
            }))
        }
        _ => Err(invalid(
            "store",
            "kind",
            format!("is `{kind}`, and the kind of object store is `{DIRECTORY}` or `{S3}`"),
        )),
    }
}

fn credentials(
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
) -> Result<(String, String), Error> {
    match (access_key_id, secret_access_key) {
        (Some(access_key_id), Some(secret_access_key)) => Ok((access_key_id, secret_access_key)),
        (Some(_), None) => Err(invalid(
            "store",
            "secret_access_key",
            "is not set, and store.access_key_id is: the two are set together".to_owned(),
        )),
        (None, Some(_)) => Err(invalid(
            "store",
            "access_key_id",
            "is not set, and store.secret_access_key is: the two are set together".to_owned(),
        )),
        (None, None) => match (
            environment(AWS_ACCESS_KEY_ID)?,
            environment(AWS_SECRET_ACCESS_KEY)?,
        ) {
            (Some(access_key_id), Some(secret_access_key)) => {
                Ok((access_key_id, secret_access_key))
            }
            (None, None) => Err(invalid(
                "store",
                "access_key_id",
                format!(
                    "is not set, and neither are {AWS_ACCESS_KEY_ID} and \
                     {AWS_SECRET_ACCESS_KEY}, so the kind `{S3}` has no credentials"
                ),
            )),
            (Some(_), None) => Err(invalid(
                "store",
                "access_key_id",
                format!(
                    "is not set, and {AWS_ACCESS_KEY_ID} is set without {AWS_SECRET_ACCESS_KEY}"
                ),
            )),
            (None, Some(_)) => Err(invalid(
                "store",
                "access_key_id",
                format!(
                    "is not set, and {AWS_SECRET_ACCESS_KEY} is set without {AWS_ACCESS_KEY_ID}"
                ),
            )),
        },
    }
}

fn misplaced(key: &str, kind: &str, configured: &str) -> Error {
    invalid(
        "store",
        key,
        format!("applies to the kind `{kind}`, and store.kind is `{configured}`"),
    )
}

fn needed(key: &str) -> Error {
    invalid(
        "store",
        key,
        format!("is not set, and the kind `{S3}` needs it"),
    )
}

fn boolean(section: &str, key: &str, text: &str) -> Result<bool, Error> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(
            section,
            key,
            format!("is `{text}`, which is neither `true` nor `false`"),
        )),
    }
}

fn path(section: &str, key: &str, file: Option<PathBuf>) -> Result<Option<PathBuf>, Error> {
    match variable(section, key)?.map(PathBuf::from).or(file) {
        Some(path) if path.as_os_str().is_empty() => {
            Err(invalid(section, key, "is empty".to_owned()))
        }
        path => Ok(path),
    }
}

fn duration(
    section: &str,
    key: &str,
    file: Option<String>,
    default: Duration,
) -> Result<Duration, Error> {
    match variable(section, key)?.or(file) {
        Some(text) => parse_duration(&text).ok_or_else(|| {
            invalid(
                section,
                key,
                format!("is `{text}`, which is not a duration: {DURATION_RULE}"),
            )
        }),
        None => Ok(default),
    }
}

fn parse_duration(text: &str) -> Option<Duration> {
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .filter(|split| *split > 0)?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let duration = match unit {
        "ms" => Duration::from_millis(number),
        "s" => Duration::from_secs(number),
        "m" => Duration::from_secs(number.checked_mul(60)?),
        "h" => Duration::from_secs(number.checked_mul(3600)?),
        _ => return None,
    };
    (!duration.is_zero()).then_some(duration)
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

fn environment(variable: &str) -> Result<Option<String>, Error> {
    match env::var(variable) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(invalid(
            "store",
            "access_key_id",
            format!("is not set, and {variable} is not UTF-8"),
        )),
    }
}

fn invalid(section: &str, key: &str, problem: String) -> Error {
    Error::Setting {
        key: format!("{section}.{key}"),
        variable: name(section, key),
        problem,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_duration;

    #[test]
    fn a_duration_is_a_whole_number_with_a_unit() {
        for (text, expected) in [
            ("250ms", Duration::from_millis(250)),
            ("30s", Duration::from_secs(30)),
            ("10m", Duration::from_secs(600)),
            ("24h", Duration::from_secs(86_400)),
        ] {
            assert_eq!(parse_duration(text), Some(expected), "{text}");
        }
        for text in [
            "", "30", "s", "0s", "1.5s", "-1s", "30 s", "30sec", "1d", " 30s",
        ] {
            assert_eq!(parse_duration(text), None, "`{text}` was accepted");
        }
    }
}
