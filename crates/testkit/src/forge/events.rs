use serde_json::{Value, json};

use super::{APP_ID, APP_SLUG, FakeForge, Installation, Repository, Shared, State};

pub const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sender<'a> {
    pub login: &'a str,
    pub id: u64,
}

impl FakeForge {
    pub fn event(
        &self,
        full_name: &str,
        action: Option<&str>,
        fields: Value,
        sender: Sender<'_>,
    ) -> Value {
        let state = self.shared.lock();
        let repository = state
            .repositories
            .get(full_name)
            .unwrap_or_else(|| panic!("the fake forge has no repository {full_name}"));
        let installation = repository.installation;
        let mut payload = match fields {
            Value::Object(fields) => fields,
            other => panic!("the fields of an event are an object, not {other}"),
        };
        if let Some(action) = action {
            payload.insert("action".to_owned(), json!(action));
        }
        payload.insert(
            "repository".to_owned(),
            self::repository(&self.shared, &state, repository),
        );
        payload.insert(
            "installation".to_owned(),
            json!({ "id": installation, "node_id": node_id("I", installation) }),
        );
        payload.insert("sender".to_owned(), user(sender));
        Value::Object(payload)
    }

    pub fn push_event(
        &self,
        full_name: &str,
        git_ref: &str,
        before: &str,
        after: &str,
        sender: Sender<'_>,
    ) -> Value {
        let created = before == NO_COMMIT;
        let deleted = after == NO_COMMIT;
        self.event(
            full_name,
            None,
            json!({
                "ref": git_ref,
                "before": before,
                "after": after,
                "created": created,
                "deleted": deleted,
                "forced": false,
                "base_ref": null,
                "commits": [],
                "head_commit": null,
                "pusher": { "name": sender.login, "email": null },
            }),
            sender,
        )
    }

    pub fn pull_request_event(
        &self,
        full_name: &str,
        number: u64,
        action: &str,
        sender: Sender<'_>,
    ) -> Value {
        let pull_request = {
            let state = self.shared.lock();
            let repository = state
                .repositories
                .get(full_name)
                .unwrap_or_else(|| panic!("the fake forge has no repository {full_name}"));
            let pull = repository
                .pulls
                .get(&number)
                .unwrap_or_else(|| panic!("{full_name} has no pull request {number}"));
            let base_repository = self::repository(&self.shared, &state, repository);
            let head_repository = match &pull.fork {
                Some(fork) => json!({
                    "id": fork.id,
                    "node_id": node_id("R", fork.id),
                    "name": repository.name,
                    "full_name": format!("{}/{}", fork.owner, repository.name),
                    "private": false,
                    "fork": true,
                    "owner": { "login": fork.owner, "id": fork.id, "type": "User" },
                }),
                None => base_repository.clone(),
            };
            let owner = pull
                .fork
                .as_ref()
                .map_or(repository.owner.as_str(), |fork| fork.owner.as_str());
            let commit = |reference: &str| repository.refs.get(reference).cloned();
            json!({
                "number": number,
                "state": "open",
                "merged": false,
                "merge_commit_sha": commit(&format!("refs/pull/{number}/merge")),
                "user": user(sender),
                "head": {
                    "label": format!("{owner}:{}", pull.head),
                    "ref": pull.head,
                    "sha": commit(&format!("refs/pull/{number}/head")),
                    "repo": head_repository,
                },
                "base": {
                    "label": format!("{}:{}", repository.owner, pull.base),
                    "ref": pull.base,
                    "sha": commit(&format!("refs/heads/{}", pull.base)),
                    "repo": base_repository,
                },
            })
        };
        self.event(
            full_name,
            Some(action),
            json!({ "number": number, "pull_request": pull_request }),
            sender,
        )
    }

    pub fn installation_event(&self, installation: u64, action: &str) -> Value {
        let state = self.shared.lock();
        let repositories: Vec<Value> = state
            .repositories
            .values()
            .filter(|repository| repository.installation == installation)
            .map(listed)
            .collect();
        json!({
            "action": action,
            "installation": described(installation, state.installation(installation)),
            "repositories": repositories,
            "sender": sender(state.installation(installation)),
        })
    }

    pub fn installation_repositories_event(
        &self,
        installation: u64,
        action: &str,
        repositories: &[(u64, &str)],
    ) -> Value {
        let state = self.shared.lock();
        let listed: Vec<Value> = repositories
            .iter()
            .map(|(id, full_name)| {
                let name = full_name.rsplit('/').next().unwrap_or(full_name);
                json!({
                    "id": id,
                    "node_id": node_id("R", *id),
                    "name": name,
                    "full_name": full_name,
                    "private": true,
                })
            })
            .collect();
        let (added, removed) = match action {
            "removed" => (Vec::new(), listed),
            _ => (listed, Vec::new()),
        };
        json!({
            "action": action,
            "installation": described(installation, state.installation(installation)),
            "repository_selection": "selected",
            "repositories_added": added,
            "repositories_removed": removed,
            "requester": null,
            "sender": sender(state.installation(installation)),
        })
    }

    pub fn repository_event(&self, full_name: &str, action: &str) -> Value {
        let state = self.shared.lock();
        let repository = state
            .repositories
            .get(full_name)
            .unwrap_or_else(|| panic!("the fake forge has no repository {full_name}"));
        let installation = repository.installation;
        json!({
            "action": action,
            "repository": self::repository(&self.shared, &state, repository),
            "installation": { "id": installation, "node_id": node_id("I", installation) },
            "sender": sender(state.installation(installation)),
        })
    }
}

pub(super) fn repository(shared: &Shared, state: &State, repository: &Repository) -> Value {
    let account = state.installation(repository.installation);
    let mut html = shared.api.clone();
    if let Ok(mut segments) = html.path_segments_mut() {
        segments
            .clear()
            .extend([repository.owner.as_str(), repository.name.as_str()]);
    }
    json!({
        "id": repository.id,
        "node_id": node_id("R", repository.id),
        "name": repository.name,
        "full_name": format!("{}/{}", repository.owner, repository.name),
        "private": true,
        "owner": {
            "login": repository.owner,
            "id": account.account_id,
            "type": "Organization",
        },
        "html_url": html.as_str(),
        "default_branch": repository.default_branch,
    })
}

fn listed(repository: &Repository) -> Value {
    json!({
        "id": repository.id,
        "node_id": node_id("R", repository.id),
        "name": repository.name,
        "full_name": format!("{}/{}", repository.owner, repository.name),
        "private": true,
    })
}

fn described(id: u64, installation: &Installation) -> Value {
    json!({
        "id": id,
        "account": {
            "login": installation.account,
            "id": installation.account_id,
            "type": "Organization",
        },
        "app_id": APP_ID,
        "app_slug": APP_SLUG,
        "repository_selection": "selected",
        "permissions": installation.permissions,
        "target_type": "Organization",
    })
}

fn user(sender: Sender<'_>) -> Value {
    let kind = if sender.login.ends_with("[bot]") {
        "Bot"
    } else {
        "User"
    };
    json!({ "login": sender.login, "id": sender.id, "type": kind })
}

fn sender(installation: &Installation) -> Value {
    json!({
        "login": installation.account,
        "id": installation.account_id,
        "type": "Organization",
    })
}

fn node_id(prefix: &str, id: u64) -> String {
    format!("{prefix}_{id:010}")
}
