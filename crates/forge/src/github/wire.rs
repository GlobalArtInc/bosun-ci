use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{Entry, EntryKind, InstalledRepository};

#[derive(Deserialize)]
pub(super) struct Failure {
    pub(super) message: String,
}

#[derive(Deserialize)]
pub(super) struct App {
    pub(super) slug: String,
}

#[derive(Deserialize)]
pub(super) struct Installation {
    #[serde(default)]
    pub(super) permissions: BTreeMap<String, String>,
}

#[derive(Deserialize)]
pub(super) struct User {
    pub(super) id: u64,
    pub(super) login: String,
}

#[derive(Deserialize)]
pub(super) struct AccessToken {
    pub(super) token: String,
    pub(super) expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Contents {
    Listing(Vec<Listed>),
    Item(Item),
}

#[derive(Deserialize)]
pub(super) struct Item {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) sha: String,
    #[serde(default)]
    pub(super) content: String,
    pub(super) encoding: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Listed {
    name: String,
    sha: String,
    #[serde(rename = "type")]
    kind: Kind,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    File,
    Dir,
    Symlink,
    Submodule,
}

impl From<Listed> for Entry {
    fn from(listed: Listed) -> Entry {
        Entry {
            name: listed.name,
            kind: match listed.kind {
                Kind::File => EntryKind::File,
                Kind::Dir => EntryKind::Directory,
                Kind::Symlink => EntryKind::Symlink,
                Kind::Submodule => EntryKind::Submodule,
            },
            sha: listed.sha,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct Blob {
    pub(super) content: String,
    pub(super) encoding: String,
}

#[derive(Deserialize)]
pub(super) struct Comparison {
    #[serde(default)]
    pub(super) files: Vec<Changed>,
}

#[derive(Deserialize)]
pub(super) struct Changed {
    pub(super) filename: String,
}

#[derive(Deserialize)]
pub(super) struct Commit {
    pub(super) parents: Vec<Parent>,
}

#[derive(Deserialize)]
pub(super) struct Parent {
    pub(super) sha: String,
}

#[derive(Deserialize)]
pub(super) struct Created {
    pub(super) id: u64,
}

#[derive(Deserialize)]
pub(super) struct InstallationRepositories {
    pub(super) repositories: Vec<Repository>,
}

#[derive(Deserialize)]
pub(super) struct Repository {
    id: u64,
    name: String,
    owner: Owner,
    default_branch: String,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

impl From<Repository> for InstalledRepository {
    fn from(repository: Repository) -> InstalledRepository {
        InstalledRepository {
            id: repository.id,
            owner: repository.owner.login,
            name: repository.name,
            default_branch: repository.default_branch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Contents;
    use crate::{Entry, EntryKind};

    #[test]
    fn a_listing_names_every_kind_of_entry() {
        let body = br#"[
            {"type": "file", "name": "ci.yml", "path": ".bosun/workflows/ci.yml",
             "sha": "ce013625030ba8dba906f756967f9e9ca394464a", "size": 6},
            {"type": "dir", "name": "shared", "path": ".bosun/workflows/shared",
             "sha": "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "size": 0},
            {"type": "symlink", "name": "link.yml", "path": ".bosun/workflows/link.yml",
             "sha": "a1e8f1c1b4b2f7a3c6d8e9f0a1b2c3d4e5f60718", "size": 6},
            {"type": "submodule", "name": "vendor", "path": ".bosun/workflows/vendor",
             "sha": "0123456789abcdef0123456789abcdef01234567", "size": 0}
        ]"#;
        let Ok(Contents::Listing(listed)) = serde_json::from_slice::<Contents>(body) else {
            panic!("a JSON array is a listing");
        };
        let entries: Vec<Entry> = listed.into_iter().map(Entry::from).collect();
        let kinds: Vec<(&str, EntryKind)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("ci.yml", EntryKind::File),
                ("shared", EntryKind::Directory),
                ("link.yml", EntryKind::Symlink),
                ("vendor", EntryKind::Submodule),
            ]
        );
        assert_eq!(entries[0].sha, "ce013625030ba8dba906f756967f9e9ca394464a");
    }
}
