use std::env::{self, VarError};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::Error;
use crate::owner::{self, LOGIN_RULE};

const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080);

const DEFAULT_MAX_CONNECTIONS: u32 = 32;

const MIGRATION_CONNECTIONS: u32 = 2;

const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

const DEFAULT_OFFLINE_AFTER: Duration = Duration::from_secs(60);

const DEFAULT_AGENT_GRACE: Duration = Duration::from_secs(600);

const DEFAULT_ASSIGNMENT_LEASE: Duration = Duration::from_secs(30);

const DEFAULT_MAX_QUEUE_TIME: Duration = Duration::from_secs(24 * 3600);

const DURATION_RULE: &str = "a duration is a whole number followed by ms, s, m or h, such as 30s";

pub struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) database: Database,
    pub(crate) keys: Keys,
    pub(crate) bootstrap_admin: Option<String>,
    pub(crate) agents: Agents,
    pub(crate) queue: Queue,
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

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AgentsFile {
    heartbeat_interval: Option<String>,
    offline_after: Option<String>,
    assignment_lease: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LimitsFile {
    agent_grace: Option<String>,
    max_queue_time: Option<String>,
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
        Ok(Config {
            listen,
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
        })
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
