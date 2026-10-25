use std::collections::BTreeMap;
use std::env;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::client::Failure;

pub(crate) const DEFAULT_PROFILE: &str = "default";

const PROFILE_RULE: &str = "a profile name is 1 to 64 letters, digits, hyphens and underscores";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    pub(crate) server: String,
    pub(crate) token: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

pub(crate) fn path() -> Result<PathBuf, Failure> {
    let base = match env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(|home| PathBuf::from(home).join(".config"))
            .ok_or_else(|| {
                Failure::new(
                    "neither XDG_CONFIG_HOME nor HOME is set, so there is no configuration",
                )
            })?,
    };
    Ok(base.join("bosun").join("config.toml"))
}

pub(crate) fn stored(path: &Path, name: &str) -> Result<Option<Profile>, Failure> {
    check_name(name)?;
    Ok(read(path)?.profiles.remove(name))
}

pub(crate) fn load(path: &Path, name: &str) -> Result<Profile, Failure> {
    stored(path, name)?.ok_or_else(|| {
        let option = if name == DEFAULT_PROFILE {
            String::new()
        } else {
            format!(" --profile {name}")
        };
        Failure::new(format!(
            "there is no profile `{name}` in {}; sign in with `bosun{option} login --server <url>`",
            path.display()
        ))
    })
}

pub(crate) fn save(path: &Path, name: &str, profile: Profile) -> Result<(), Failure> {
    check_name(name)?;
    let mut file = read(path)?;
    file.profiles.insert(name.to_owned(), profile);
    let text = toml::to_string(&file)
        .map_err(|error| Failure::new(format!("the profiles cannot be written: {error}")))?;
    let directory = path
        .parent()
        .ok_or_else(|| Failure::new(format!("{} has no directory", path.display())))?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|error| written(directory, &error))?;
    let partial = path.with_extension("toml.partial");
    let _ = fs::remove_file(&partial);
    let mut created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .map_err(|error| written(&partial, &error))?;
    created
        .write_all(text.as_bytes())
        .and_then(|()| created.sync_all())
        .map_err(|error| written(&partial, &error))?;
    fs::rename(&partial, path).map_err(|error| written(path, &error))
}

pub(crate) fn server(text: &str) -> Result<String, Failure> {
    let url = Url::parse(text)
        .map_err(|error| Failure::new(format!("`{text}` is not a URL: {error}")))?;
    let usable = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.query().is_none()
        && url.fragment().is_none();
    if !usable {
        return Err(Failure::new(format!(
            "`{text}` is not the http or https address of a server"
        )));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn read(path: &Path) -> Result<File, Failure> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|error| {
            Failure::new(format!(
                "{} is not a valid configuration: {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(File::default()),
        Err(error) => Err(Failure::new(format!(
            "{} is not readable: {error}",
            path.display()
        ))),
    }
}

fn check_name(name: &str) -> Result<(), Failure> {
    let valid = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character));
    if valid {
        Ok(())
    } else {
        Err(Failure::new(format!(
            "`{name}` is not a profile name: {PROFILE_RULE}"
        )))
    }
}

fn written(path: &Path, error: &io::Error) -> Failure {
    Failure::new(format!("{} could not be written: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::{Profile, load, save, server, stored};

    fn profile(server: &str, token: &str) -> Profile {
        Profile {
            server: server.to_owned(),
            token: token.to_owned(),
        }
    }

    #[test]
    fn profiles_are_kept_side_by_side_where_only_their_owner_can_read_them() {
        let home = tempfile::tempdir().expect("a home");
        let path = home.path().join("bosun").join("config.toml");
        assert_eq!(
            stored(&path, "default").expect("no file is no profile"),
            None
        );
        save(&path, "default", profile("http://a:8080", "bst_a")).expect("a saved profile");
        save(&path, "replica-b", profile("http://b:8080", "bst_b")).expect("a second one");
        save(&path, "default", profile("http://a:8080", "bst_c")).expect("a replaced one");
        assert_eq!(
            load(&path, "default").expect("the default profile"),
            profile("http://a:8080", "bst_c")
        );
        assert_eq!(
            load(&path, "replica-b").expect("the second profile"),
            profile("http://b:8080", "bst_b")
        );
        let mode = |path: &std::path::Path| {
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777
        };
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().expect("a directory")), 0o700);
        assert!(!path.with_extension("toml.partial").exists());
    }

    #[test]
    fn a_missing_profile_says_how_to_sign_in() {
        let home = tempfile::tempdir().expect("a home");
        let path = home.path().join("config.toml");
        let missing = load(&path, "default").expect_err("no profile");
        assert!(
            missing
                .message
                .ends_with("sign in with `bosun login --server <url>`"),
            "{}",
            missing.message
        );
        let named = load(&path, "b").expect_err("no profile");
        assert!(
            named
                .message
                .ends_with("sign in with `bosun --profile b login --server <url>`"),
            "{}",
            named.message
        );
        let invalid = load(&path, "a b").expect_err("no such name");
        assert!(
            invalid.message.contains("is not a profile name"),
            "{}",
            invalid.message
        );
    }

    #[test]
    fn a_file_with_unknown_keys_is_refused_by_name() {
        let home = tempfile::tempdir().expect("a home");
        let path = home.path().join("config.toml");
        fs::write(
            &path,
            "[profiles.default]\nserver = \"http://a\"\ntoken = \"bst_a\"\nkeyring = true\n",
        )
        .expect("a file");
        let refused = load(&path, "default").expect_err("an unknown key");
        assert!(
            refused.message.contains("unknown field `keyring`"),
            "{}",
            refused.message
        );
        assert!(
            refused.message.contains(&path.display().to_string()),
            "{}",
            refused.message
        );
    }

    #[test]
    fn a_server_is_an_http_address_without_a_trailing_slash() {
        assert_eq!(
            server("http://127.0.0.1:8080/").ok().as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            server("https://ci.example.org/bosun/").ok().as_deref(),
            Some("https://ci.example.org/bosun")
        );
        for refused in [
            "ci.example.org",
            "ftp://ci.example.org",
            "https://ci.example.org/?a=1",
        ] {
            assert!(server(refused).is_err(), "{refused} was accepted");
        }
    }
}
