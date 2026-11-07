use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use bosun_runner::container::PullPolicy;
use bosun_types::id::AgentId;
use reqwest::Url;
use serde::{Deserialize, Serialize};

const SPOOL_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) url: String,
    pub(crate) dir: PathBuf,
    pub(crate) agent_id: AgentId,
    pub(crate) credential: String,
    #[serde(default)]
    pub(crate) labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) https_proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) no_proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) spool_max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pull_policy: Option<PullPolicy>,
}

impl Config {
    pub(crate) fn load(file: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(file)
            .map_err(|error| format!("{} is not readable: {error}", file.display()))?;
        let config: Config = toml::from_str(&text)
            .map_err(|error| format!("{} is not a valid configuration: {error}", file.display()))?;
        server(&config.url).map_err(|reason| format!("{}: {reason}", file.display()))?;
        if !config.dir.is_absolute() {
            return Err(format!(
                "{}: `dir` is {}, which is not an absolute path",
                file.display(),
                config.dir.display()
            ));
        }
        Ok(config)
    }

    pub(crate) fn create(&self, file: &Path) -> Result<(), String> {
        let text = toml::to_string(self)
            .map_err(|error| format!("the configuration cannot be written as TOML: {error}"))?;
        let mut created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(file)
            .map_err(|error| format!("{} could not be created: {error}", file.display()))?;
        created
            .write_all(text.as_bytes())
            .and_then(|()| created.sync_all())
            .map_err(|error| format!("{} could not be written: {error}", file.display()))
    }

    pub(crate) fn spool_limit(&self) -> u64 {
        self.spool_max_bytes.unwrap_or(SPOOL_LIMIT)
    }

    pub(crate) fn pull_policy(&self) -> PullPolicy {
        self.pull_policy.unwrap_or_default()
    }

    pub(crate) fn proxies(&self) -> Vec<(String, String)> {
        let mut variables = Vec::new();
        for (name, value) in [
            ("HTTP_PROXY", &self.http_proxy),
            ("HTTPS_PROXY", &self.https_proxy),
            ("NO_PROXY", &self.no_proxy),
        ] {
            if let Some(value) = value {
                variables.push((name.to_owned(), value.clone()));
                variables.push((name.to_lowercase(), value.clone()));
            }
        }
        variables
    }
}

pub(crate) fn server(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|error| format!("`{url}` is not a URL: {error}"))?;
    match parsed.scheme() {
        "http" | "https" if parsed.host().is_some() => Ok(parsed),
        _ => Err(format!(
            "`{url}` is not the http or https address of a server"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use bosun_runner::container::PullPolicy;

    use super::Config;

    const ID: &str = "0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3a";

    fn config() -> Config {
        Config {
            url: "https://ci.example.org".to_owned(),
            dir: PathBuf::from("/var/lib/bosun/agents/1"),
            agent_id: ID.parse().expect("an agent id"),
            credential: "bat_secret".to_owned(),
            labels: vec!["deploy".to_owned()],
            http_proxy: Some("http://proxy:3128".to_owned()),
            https_proxy: None,
            no_proxy: Some("localhost".to_owned()),
            spool_max_bytes: None,
            pull_policy: None,
        }
    }

    #[test]
    fn a_created_configuration_loads_back_and_only_its_owner_can_read_it() {
        let directory = tempfile::tempdir().expect("a directory");
        let file = directory.path().join("agent-1.toml");
        config()
            .create(&file)
            .expect("the configuration is written");
        let mode = fs::metadata(&file).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(Config::load(&file), Ok(config()));
        assert!(
            config().create(&file).is_err(),
            "an existing file was overwritten"
        );
        assert_eq!(config().spool_limit(), 256 * 1024 * 1024);
    }

    #[test]
    fn the_pull_policy_is_missing_unless_the_configuration_says_never_and_refuses_other_words() {
        let directory = tempfile::tempdir().expect("a directory");
        let file = directory.path().join("agent.toml");
        let base = format!(
            "agent_id = \"{ID}\"\ncredential = \"bat_x\"\nurl = \"https://ci\"\ndir = \"/a\"\n"
        );
        assert_eq!(config().pull_policy(), PullPolicy::Missing);
        for (line, expected) in [
            ("", PullPolicy::Missing),
            ("pull_policy = \"missing\"\n", PullPolicy::Missing),
            ("pull_policy = \"never\"\n", PullPolicy::Never),
        ] {
            fs::write(&file, format!("{base}{line}")).expect("a file");
            assert_eq!(
                Config::load(&file).expect("a configuration").pull_policy(),
                expected,
                "{line}"
            );
        }
        fs::write(&file, format!("{base}pull_policy = \"always\"\n")).expect("a file");
        let error = Config::load(&file).expect_err("a refusal");
        assert!(
            error.contains("pull_policy") && error.contains("always"),
            "{error}"
        );
    }

    #[test]
    fn proxies_reach_steps_under_both_spellings() {
        assert_eq!(
            config().proxies(),
            [
                ("HTTP_PROXY", "http://proxy:3128"),
                ("http_proxy", "http://proxy:3128"),
                ("NO_PROXY", "localhost"),
                ("no_proxy", "localhost"),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn unknown_keys_relative_directories_and_other_schemes_are_refused() {
        let directory = tempfile::tempdir().expect("a directory");
        let file = directory.path().join("agent.toml");
        let base = format!("agent_id = \"{ID}\"\ncredential = \"bat_x\"\n");
        for (text, expected) in [
            (
                format!("{base}url = \"https://ci\"\ndir = \"/a\"\nallow_terminal = true\n"),
                "unknown field `allow_terminal`",
            ),
            (
                format!("{base}url = \"https://ci\"\ndir = \"agents/1\"\n"),
                "`dir` is agents/1, which is not an absolute path",
            ),
            (
                format!("{base}url = \"ftp://ci\"\ndir = \"/a\"\n"),
                "`ftp://ci` is not the http or https address of a server",
            ),
        ] {
            fs::write(&file, text).expect("a file");
            let error = Config::load(&file).expect_err("a refusal");
            assert!(error.contains(expected), "{error}");
            assert!(error.contains(&file.display().to_string()), "{error}");
        }
    }
}
