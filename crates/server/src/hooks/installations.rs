use bosun_db::installations::Account;
use bosun_forge::Forge as _;
use bosun_types::api::Repository;
use sea_orm::DatabaseTransaction;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{info, warn};

use crate::forge::Forge;
use crate::retry::Failure;
use crate::slug;

#[derive(Deserialize)]
struct Event {
    action: Option<String>,
    installation: Installation,
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
    account: Option<Owner>,
}

#[derive(Deserialize)]
struct Owner {
    id: u64,
    login: String,
}

pub(super) async fn installation(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    payload: &Value,
) -> Result<(), Failure> {
    let event: Event = read(payload)?;
    let id = event.installation.id;
    match event.action.as_deref() {
        Some("deleted") => {
            let removed = bosun_db::installations::remove(transaction, id).await?;
            info!(
                installation_id = id,
                removed, "an installation of the GitHub App was removed"
            );
            Ok(())
        }
        Some("suspend") => record(transaction, &event.installation).await,
        _ => {
            record(transaction, &event.installation).await?;
            sync(transaction, forge, id).await
        }
    }
}

pub(super) async fn installation_repositories(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    payload: &Value,
) -> Result<(), Failure> {
    let event: Event = read(payload)?;
    record(transaction, &event.installation).await?;
    sync(transaction, forge, event.installation.id).await
}

pub(super) async fn repository(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    payload: &Value,
) -> Result<(), Failure> {
    let event: Event = read(payload)?;
    let id = event.installation.id;
    if bosun_db::installations::lock(transaction, id).await? {
        sync(transaction, forge, id).await
    } else {
        info!(
            installation_id = id,
            "a repository event came from an installation of the GitHub App that Bosun does not \
             know, and changes nothing"
        );
        Ok(())
    }
}

fn read<T: DeserializeOwned>(payload: &Value) -> Result<T, Failure> {
    T::deserialize(payload).map_err(|error| {
        Failure::Refused(format!(
            "the payload does not have the shape of its event: {error}"
        ))
    })
}

async fn record(
    transaction: &DatabaseTransaction,
    installation: &Installation,
) -> Result<(), Failure> {
    let account = installation.account.as_ref().ok_or_else(|| {
        Failure::Refused("the installation of the payload names no account".to_owned())
    })?;
    let mapped = bosun_db::installations::record(
        transaction,
        installation.id,
        &Account {
            id: account.id,
            login: &account.login,
        },
    )
    .await?;
    if mapped.is_none() {
        map(transaction, installation.id, &account.login).await?;
    }
    Ok(())
}

async fn map(
    transaction: &DatabaseTransaction,
    installation: u64,
    login: &str,
) -> Result<(), Failure> {
    let slug = login.to_lowercase();
    let created = if slug::is_slug(&slug) && !slug::reads_as_id(&slug) {
        bosun_db::organisations::create_unless_taken(transaction, &slug, login).await?
    } else {
        None
    };
    match created {
        Some(organisation) => {
            bosun_db::installations::map(transaction, installation, organisation).await?;
            info!(
                installation_id = installation,
                account = login,
                organisation = slug,
                "created the organisation of an installation of the GitHub App"
            );
        }
        None => warn!(
            installation_id = installation,
            account = login,
            slug,
            "an installation of the GitHub App maps to no organisation, because the slug of its \
             account is taken or is not a slug; an owner binds it to one with PATCH \
             /api/v1/organisations/<organisation>"
        ),
    }
    Ok(())
}

async fn sync(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    installation: u64,
) -> Result<(), Failure> {
    let repositories: Vec<Repository> = forge
        .app
        .installation_repositories(installation)
        .await?
        .into_iter()
        .map(|repository| Repository {
            id: repository.id,
            owner: repository.owner,
            name: repository.name,
            default_branch: repository.default_branch,
        })
        .collect();
    let synced = bosun_db::installations::sync(transaction, installation, &repositories).await?;
    info!(
        installation_id = installation,
        visible = synced.visible,
        hidden = synced.hidden,
        "read the repositories an installation of the GitHub App can see"
    );
    Ok(())
}
