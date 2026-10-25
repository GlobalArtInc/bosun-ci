use std::env::{self, VarError};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bosun_blob::S3Settings;
use bosun_types::slug::SLUG_RULE;
use ipnet::IpNet;
use serde::Deserialize;
use url::Url;

use crate::Error;
use crate::address;
use crate::owner::{self, LOGIN_RULE};
use crate::site::Site;
use crate::slug::{is_slug, reads_as_id};
use crate::tools::Runtimes;

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

const GITHUB_HOST: &str = "github.com";

const GITHUB_API: &str = "https://api.github.com";

const GITHUB_SERVER: &str = "https://github.com";

const ENTERPRISE_API: [&str; 2] = ["api", "v3"];

const DURATION_RULE: &str = "a duration is a whole number followed by ms, s, m or h, such as 30s";

const NODE_UPSTREAM: &str = "https://nodejs.org/dist";

const NODE_MUSL_UPSTREAM: &str = "https://unofficial-builds.nodejs.org/download/release";

const DEFAULT_NODE: &str = "24.22.1";

const OPENID: &str = "openid";

const DEFAULT_SCOPES: [&str; 3] = [OPENID, "profile", "email"];

const DEFAULT_LOGIN_CLAIM: &str = "preferred_username";

const DEFAULT_EMAIL_CLAIM: &str = "email";

const DEFAULT_GROUPS_CLAIM: &str = "groups";

pub struct Config {
    pub(crate) listen: SocketAddr,
    pub(crate) data_dir: PathBuf,
    pub(crate) trusted_proxies: Vec<IpNet>,
    pub(crate) site: Site,
    pub(crate) database: Database,
    pub(crate) keys: Keys,
    pub(crate) bootstrap_admin: Option<String>,
    pub(crate) agents: Agents,
    pub(crate) queue: Queue,
    pub(crate) store: Store,
    pub(crate) agent_binaries: PathBuf,
    pub(crate) failed_sign_ins: u32,
    pub(crate) github: Option<GitHub>,
    pub(crate) tools: Tools,
    pub(crate) oidc: Vec<Oidc>,
}

#[derive(Clone)]
pub(crate) struct Tools {
    pub(crate) node_upstream: Url,
    pub(crate) node_musl_upstream: Url,
    pub(crate) runtimes: Runtimes,
}

#[derive(Clone)]
pub(crate) struct Oidc {
    pub(crate) name: String,
    pub(crate) issuer: String,
    pub(crate) client_id: String,
    pub(crate) client_secret_file: PathBuf,
    pub(crate) scopes: Vec<String>,
    pub(crate) login_claim: String,
    pub(crate) email_claim: String,
    pub(crate) groups_claim: String,
    pub(crate) allowed_groups: Vec<String>,
    pub(crate) allowed_email_domains: Vec<String>,
    pub(crate) teams: Vec<TeamMapping>,
}

#[derive(Clone)]
pub(crate) struct TeamMapping {
    pub(crate) group: String,
    pub(crate) organisation: String,
    pub(crate) team: String,
}

pub(crate) struct GitHub {
    pub(crate) app_id: u64,
    pub(crate) private_key_file: PathBuf,
    pub(crate) webhook_secret_file: PathBuf,
    pub(crate) server_url: Url,
    pub(crate) api_url: Url,
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
    pub(crate) previous_kek_files: Vec<PathBuf>,
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
    github: GitHubFile,
    tools: ToolsFile,
    oidc: Vec<OidcFile>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerFile {
    listen: Option<String>,
    data_dir: Option<PathBuf>,
    trusted_proxies: Option<Vec<String>>,
    public_url: Option<String>,
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
    previous_kek_files: Option<Vec<PathBuf>>,
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
struct GitHubFile {
    app_id: Option<u64>,
    private_key_file: Option<PathBuf>,
    webhook_secret_file: Option<PathBuf>,
    server_url: Option<String>,
    api_url: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ToolsFile {
    node_upstream: Option<String>,
    node_musl_upstream: Option<String>,
    node12: Option<String>,
    node16: Option<String>,
    node20: Option<String>,
    node24: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OidcFile {
    name: Option<String>,
    issuer: Option<String>,
    client_id: Option<String>,
    client_secret_file: Option<PathBuf>,
    scopes: Option<Vec<String>>,
    login_claim: Option<String>,
    email_claim: Option<String>,
    groups_claim: Option<String>,
    allowed_groups: Vec<String>,
    allowed_email_domains: Vec<String>,
    teams: Vec<TeamFile>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TeamFile {
    group: Option<String>,
    team: Option<String>,
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
        let site = Site::new(
            variable("server", "public_url")?
                .or(file.server.public_url)
                .map(|text| public_url(&text))
                .transpose()?,
        );
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
            previous_kek_files: previous_kek_files(file.keys.previous_kek_files)?,
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
        let github = github(file.github)?;
        let site = match &github {
            Some(github) => site.with_forge(&github.server_url, &github.api_url),
            None => site,
        };
        let tools = tools(file.tools)?;
        let agent_binaries = path("agents", "binary_dir", file.agents.binary_dir)?
            .unwrap_or_else(|| PathBuf::from(DEFAULT_AGENT_BINARIES));
        let oidc = oidc(file.oidc)?;
        if !oidc.is_empty() && site.url().is_empty() {
            return Err(invalid(
                "server",
                "public_url",
                "is not set, and the identity providers of [[oidc]] need it for the address \
                 their sign-ins return to"
                    .to_owned(),
            ));
        }
        Ok(Config {
            listen,
            data_dir,
            trusted_proxies,
            site,
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
            github,
            tools,
            oidc,
        })
    }
}

fn tools(file: ToolsFile) -> Result<Tools, Error> {
    let node_upstream = upstream("node_upstream", file.node_upstream, NODE_UPSTREAM)?;
    let node_musl_upstream = upstream(
        "node_musl_upstream",
        file.node_musl_upstream,
        NODE_MUSL_UPSTREAM,
    )?;
    let runtimes = Runtimes {
        node12: node("node12", file.node12)?,
        node16: node("node16", file.node16)?,
        node20: node("node20", file.node20)?,
        node24: node("node24", file.node24)?,
    };
    Ok(Tools {
        node_upstream,
        node_musl_upstream,
        runtimes,
    })
}

fn node(key: &str, file: Option<String>) -> Result<String, Error> {
    let version = variable("tools", key)?
        .or(file)
        .unwrap_or_else(|| DEFAULT_NODE.to_owned());
    if Runtimes::is_version(&version) {
        Ok(version)
    } else {
        Err(invalid(
            "tools",
            key,
            format!("is `{version}`, which is not a version of Node such as {DEFAULT_NODE}"),
        ))
    }
}

fn upstream(key: &str, file: Option<String>, default: &str) -> Result<Url, Error> {
    let text = variable("tools", key)?
        .or(file)
        .unwrap_or_else(|| default.to_owned());
    let refused =
        |problem: &str| invalid("tools", key, format!("is `{text}`, which is not {problem}"));
    let url = Url::parse(&text).map_err(|_| refused("an address"))?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => {}
        "file" if url.to_file_path().is_ok() => {}
        _ => {
            return Err(refused(
                "an http or https address with a host, or a file address of a directory",
            ));
        }
    }
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(refused(
            "an address without a user, a password, a query or a fragment",
        ));
    }
    Ok(url)
}

fn oidc(files: Vec<OidcFile>) -> Result<Vec<Oidc>, Error> {
    let mut providers: Vec<Oidc> = Vec::with_capacity(files.len());
    for (index, file) in files.into_iter().enumerate() {
        let label = match &file.name {
            Some(name) => format!("`{name}`"),
            None => format!("number {}", index + 1),
        };
        let refused = |key: &str, problem: String| Error::Oidc {
            provider: label.clone(),
            key: key.to_owned(),
            problem,
        };
        let required = |key: &str, value: Option<String>| {
            value
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| refused(key, "is not set".to_owned()))
        };
        let name = required("name", file.name.clone())?;
        if !is_slug(&name) || reads_as_id(&name) {
            return Err(refused(
                "name",
                format!("is `{name}`, and the name of a provider is {SLUG_RULE} and unlike an id"),
            ));
        }
        if providers.iter().any(|provider| provider.name == name) {
            return Err(refused(
                "name",
                "is the name of an earlier provider, and each provider has a name of its own"
                    .to_owned(),
            ));
        }
        let issuer = required("issuer", file.issuer)?;
        if let Err(Error::Setting { problem, .. }) = address("oidc", "issuer", &issuer) {
            return Err(refused("issuer", problem));
        }
        let client_id = required("client_id", file.client_id)?;
        let client_secret_file = match file.client_secret_file {
            Some(path) if !path.as_os_str().is_empty() => path,
            Some(_) => return Err(refused("client_secret_file", "is empty".to_owned())),
            None => return Err(refused("client_secret_file", "is not set".to_owned())),
        };
        let scopes = file
            .scopes
            .unwrap_or_else(|| DEFAULT_SCOPES.map(str::to_owned).to_vec());
        if let Some(scope) = scopes
            .iter()
            .find(|scope| scope.is_empty() || scope.chars().any(char::is_whitespace))
        {
            return Err(refused(
                "scopes",
                format!("holds `{scope}`, and a scope is a word without white space"),
            ));
        }
        if !scopes.iter().any(|scope| scope == OPENID) {
            return Err(refused(
                "scopes",
                format!("lacks `{OPENID}`, which asks the provider for the ID token"),
            ));
        }
        let claim = |key: &str, value: Option<String>, default: &str| match value {
            Some(claim) if claim.trim().is_empty() => Err(refused(key, "is empty".to_owned())),
            Some(claim) => Ok(claim),
            None => Ok(default.to_owned()),
        };
        let login_claim = claim("login_claim", file.login_claim, DEFAULT_LOGIN_CLAIM)?;
        let email_claim = claim("email_claim", file.email_claim, DEFAULT_EMAIL_CLAIM)?;
        let groups_claim = claim("groups_claim", file.groups_claim, DEFAULT_GROUPS_CLAIM)?;
        if file.allowed_groups.iter().any(String::is_empty) {
            return Err(refused("allowed_groups", "holds an empty group".to_owned()));
        }
        let mut allowed_email_domains = Vec::with_capacity(file.allowed_email_domains.len());
        for domain in file.allowed_email_domains {
            if domain.is_empty() || domain.contains('@') || domain.chars().any(char::is_whitespace)
            {
                return Err(refused(
                    "allowed_email_domains",
                    format!("holds `{domain}`, which is not a domain such as example.org"),
                ));
            }
            allowed_email_domains.push(domain.to_lowercase());
        }
        let mut teams = Vec::with_capacity(file.teams.len());
        for (number, mapping) in file.teams.into_iter().enumerate() {
            let key = format!("teams.{number}");
            let group = mapping
                .group
                .filter(|group| !group.is_empty())
                .ok_or_else(|| refused(&format!("{key}.group"), "is not set".to_owned()))?;
            let team = mapping
                .team
                .ok_or_else(|| refused(&format!("{key}.team"), "is not set".to_owned()))?;
            let Some((organisation, slug)) = team.split_once('/').filter(|(organisation, slug)| {
                [organisation, slug]
                    .iter()
                    .all(|part| is_slug(part) && !reads_as_id(part))
            }) else {
                return Err(refused(
                    &format!("{key}.team"),
                    format!(
                        "is `{team}`, which is not `<organisation>/<team>` with the slugs of \
                         both"
                    ),
                ));
            };
            teams.push(TeamMapping {
                group,
                organisation: organisation.to_owned(),
                team: slug.to_owned(),
            });
        }
        providers.push(Oidc {
            name,
            issuer,
            client_id,
            client_secret_file,
            scopes,
            login_claim,
            email_claim,
            groups_claim,
            allowed_groups: file.allowed_groups,
            allowed_email_domains,
            teams,
        });
    }
    Ok(providers)
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

fn previous_kek_files(file: Option<Vec<PathBuf>>) -> Result<Vec<PathBuf>, Error> {
    let paths: Vec<PathBuf> = match variable("keys", "previous_kek_files")? {
        Some(text) => text
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect(),
        None => file.unwrap_or_default(),
    };
    if paths.iter().any(|path| path.as_os_str().is_empty()) {
        return Err(invalid(
            "keys",
            "previous_kek_files",
            "holds an empty path".to_owned(),
        ));
    }
    Ok(paths)
}

fn github(file: GitHubFile) -> Result<Option<GitHub>, Error> {
    let app_id = match variable("github", "app_id")? {
        Some(text) => Some(text.parse().map_err(|_| {
            invalid(
                "github",
                "app_id",
                format!("is `{text}`, which is not the number of a GitHub App"),
            )
        })?),
        None => file.app_id,
    };
    let private_key_file = path("github", "private_key_file", file.private_key_file)?;
    let webhook_secret_file = path("github", "webhook_secret_file", file.webhook_secret_file)?;
    let server_url = variable("github", "server_url")?
        .or(file.server_url)
        .map(|text| address("github", "server_url", &text))
        .transpose()?;
    let api_url = variable("github", "api_url")?
        .or(file.api_url)
        .map(|text| address("github", "api_url", &text))
        .transpose()?;
    let set = [
        ("app_id", app_id.is_some()),
        ("private_key_file", private_key_file.is_some()),
        ("webhook_secret_file", webhook_secret_file.is_some()),
        ("server_url", server_url.is_some()),
        ("api_url", api_url.is_some()),
    ];
    let (Some(app_id), Some(private_key_file), Some(webhook_secret_file)) =
        (app_id, private_key_file, webhook_secret_file)
    else {
        let Some((given, _)) = set.iter().find(|(_, set)| *set) else {
            return Ok(None);
        };
        let missing = set[..3]
            .iter()
            .find(|(_, set)| !*set)
            .map_or("app_id", |(key, _)| *key);
        return Err(invalid(
            "github",
            missing,
            format!(
                "is not set, and github.{given} is: the GitHub App needs app_id, \
                 private_key_file and webhook_secret_file together"
            ),
        ));
    };
    let api_url = match (api_url, &server_url) {
        (Some(api_url), _) => api_url,
        (None, Some(server_url)) if server_url.host_str() != Some(GITHUB_HOST) => {
            let mut api_url = server_url.clone();
            if let Ok(mut segments) = api_url.path_segments_mut() {
                segments.pop_if_empty().extend(ENTERPRISE_API);
            }
            api_url
        }
        (None, _) => Url::parse(GITHUB_API)
            .map_err(|error| invalid("github", "api_url", format!("has no default: {error}")))?,
    };
    let server_url = match server_url {
        Some(server_url) => server_url,
        None => Url::parse(GITHUB_SERVER)
            .map_err(|error| invalid("github", "server_url", format!("has no default: {error}")))?,
    };
    Ok(Some(GitHub {
        app_id,
        private_key_file,
        webhook_secret_file,
        server_url,
        api_url,
    }))
}

fn address(section: &str, key: &str, text: &str) -> Result<Url, Error> {
    let refused =
        |problem: &str| invalid(section, key, format!("is `{text}`, which is not {problem}"));
    let url = Url::parse(text).map_err(|_| refused("an address"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(refused("an http or https address with a host"));
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err(refused("an address without a user, a query or a fragment"));
    }
    Ok(url)
}

fn public_url(text: &str) -> Result<String, Error> {
    address("server", "public_url", text).map(|_| text.to_owned())
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
    use std::fs;
    use std::time::Duration;

    use super::{Config, parse_duration};
    use crate::tools::Runtimes;

    const DATABASE: &str = "[database]\nurl = \"postgres://bosun@127.0.0.1:1/bosun\"\n";

    fn load(text: &str) -> Config {
        let directory = tempfile::tempdir().expect("a directory for the configuration");
        let path = directory.path().join("server.toml");
        fs::write(&path, text).expect("a configuration file");
        Config::load(Some(&path)).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn without_tools_every_node_runtime_runs_on_node_24_from_the_builds_of_the_node_project() {
        let tools = load(DATABASE).tools;
        assert_eq!(
            tools.runtimes,
            Runtimes {
                node12: "24.22.1".to_owned(),
                node16: "24.22.1".to_owned(),
                node20: "24.22.1".to_owned(),
                node24: "24.22.1".to_owned(),
            }
        );
        assert_eq!(tools.node_upstream.as_str(), "https://nodejs.org/dist");
        assert_eq!(
            tools.node_musl_upstream.as_str(),
            "https://unofficial-builds.nodejs.org/download/release"
        );
    }

    #[test]
    fn each_setting_of_tools_reaches_its_own_runtime_and_upstream() {
        let tools = load(&format!(
            "{DATABASE}\n[tools]\nnode12 = \"12.22.12\"\nnode16 = \"16.20.2\"\nnode20 = \
             \"20.19.5\"\nnode24 = \"24.11.0\"\nnode_upstream = \
             \"https://mirror.example.org/node/\"\nnode_musl_upstream = \"file:///srv/node-musl\"\n"
        ))
        .tools;
        assert_eq!(
            tools.runtimes,
            Runtimes {
                node12: "12.22.12".to_owned(),
                node16: "16.20.2".to_owned(),
                node20: "20.19.5".to_owned(),
                node24: "24.11.0".to_owned(),
            }
        );
        assert_eq!(
            tools.node_upstream.as_str(),
            "https://mirror.example.org/node/"
        );
        assert_eq!(tools.node_musl_upstream.as_str(), "file:///srv/node-musl");
    }

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
