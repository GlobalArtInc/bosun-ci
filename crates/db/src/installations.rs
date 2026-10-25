use bosun_types::api::Repository;
use bosun_types::id::OrganisationId;
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction};

use crate::row::statement;
use crate::{Error, row};

const INSTALLATIONS: &str = "forge_installation";

const REPOSITORIES: &str = "forge_repository";

pub struct Account<'a> {
    pub id: u64,
    pub login: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synced {
    pub visible: usize,
    pub hidden: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    NoInstallation,
    Hidden { account: String },
    Visible(Repository),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    Bound,
    Unknown(u64),
    Taken {
        installation: u64,
        organisation: String,
    },
}

pub async fn record(
    transaction: &DatabaseTransaction,
    installation: u64,
    account: &Account<'_>,
) -> Result<Option<OrganisationId>, Error> {
    let found = transaction
        .query_one(statement(
            "INSERT INTO forge_installation (id, account_id, account_login) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET account_id = excluded.account_id, \
             account_login = excluded.account_login RETURNING organisation_id",
            [
                row::signed(INSTALLATIONS, installation)?.into(),
                row::signed(INSTALLATIONS, account.id)?.into(),
                account.login.into(),
            ],
        ))
        .await?
        .ok_or(Error::Missing(INSTALLATIONS))?;
    row::optional_id(INSTALLATIONS, found.try_get("", "organisation_id")?)
}

pub async fn lock(transaction: &DatabaseTransaction, installation: u64) -> Result<bool, Error> {
    Ok(transaction
        .query_one(statement(
            "SELECT id FROM forge_installation WHERE id = $1 FOR UPDATE",
            [row::signed(INSTALLATIONS, installation)?.into()],
        ))
        .await?
        .is_some())
}

pub async fn map(
    transaction: &DatabaseTransaction,
    installation: u64,
    organisation: OrganisationId,
) -> Result<(), Error> {
    transaction
        .execute(statement(
            "UPDATE forge_installation SET organisation_id = $2 WHERE id = $1",
            [
                row::signed(INSTALLATIONS, installation)?.into(),
                Uuid::from(organisation).into(),
            ],
        ))
        .await?;
    Ok(())
}

pub async fn remove(transaction: &DatabaseTransaction, installation: u64) -> Result<bool, Error> {
    let removed = transaction
        .execute(statement(
            "DELETE FROM forge_installation WHERE id = $1",
            [row::signed(INSTALLATIONS, installation)?.into()],
        ))
        .await?;
    Ok(removed.rows_affected() > 0)
}

pub async fn sync(
    transaction: &DatabaseTransaction,
    installation: u64,
    repositories: &[Repository],
) -> Result<Synced, Error> {
    let owner = row::signed(INSTALLATIONS, installation)?;
    let mut ids = Vec::with_capacity(repositories.len());
    for repository in repositories {
        let id = row::signed(REPOSITORIES, repository.id)?;
        ids.push(id);
        transaction
            .execute(statement(
                "INSERT INTO forge_repository (id, installation_id, owner, name, default_branch) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET \
                 installation_id = excluded.installation_id, owner = excluded.owner, \
                 name = excluded.name, default_branch = excluded.default_branch",
                [
                    id.into(),
                    owner.into(),
                    repository.owner.as_str().into(),
                    repository.name.as_str().into(),
                    repository.default_branch.as_str().into(),
                ],
            ))
            .await?;
    }
    let hidden = transaction
        .execute(statement(
            "UPDATE forge_repository SET installation_id = NULL \
             WHERE installation_id = $1 AND NOT (id = ANY ($2))",
            [owner.into(), ids.into()],
        ))
        .await?;
    Ok(Synced {
        visible: repositories.len(),
        hidden: hidden.rows_affected(),
    })
}

pub async fn visibility<C: ConnectionTrait>(
    db: &C,
    organisation: OrganisationId,
    owner: &str,
    name: &str,
) -> Result<Visibility, Error> {
    let Some(installation) = db
        .query_one(statement(
            "SELECT id, account_login FROM forge_installation WHERE organisation_id = $1",
            [Uuid::from(organisation).into()],
        ))
        .await?
    else {
        return Ok(Visibility::NoInstallation);
    };
    let id: i64 = installation.try_get("", "id")?;
    let found = db
        .query_one(statement(
            "SELECT id, owner, name, default_branch FROM forge_repository \
             WHERE installation_id = $1 AND lower(owner) = lower($2) AND lower(name) = lower($3)",
            [id.into(), owner.into(), name.into()],
        ))
        .await?;
    match found {
        Some(found) => Ok(Visibility::Visible(Repository {
            id: row::unsigned(REPOSITORIES, found.try_get("", "id")?)?,
            owner: found.try_get("", "owner")?,
            name: found.try_get("", "name")?,
            default_branch: found.try_get("", "default_branch")?,
        })),
        None => Ok(Visibility::Hidden {
            account: installation.try_get("", "account_login")?,
        }),
    }
}

pub async fn bind(
    transaction: &DatabaseTransaction,
    organisation: OrganisationId,
    installation: Option<u64>,
) -> Result<Bound, Error> {
    let organisation = Uuid::from(organisation);
    let Some(installation) = installation else {
        transaction
            .execute(statement(
                "UPDATE forge_installation SET organisation_id = NULL WHERE organisation_id = $1",
                [organisation.into()],
            ))
            .await?;
        return Ok(Bound::Bound);
    };
    let wanted = installation;
    let installation = row::signed(INSTALLATIONS, installation)?;
    let Some(found) = transaction
        .query_one(statement(
            "SELECT i.organisation_id, o.slug FROM forge_installation i \
             LEFT JOIN organisation o ON o.id = i.organisation_id WHERE i.id = $1 \
             FOR UPDATE OF i",
            [installation.into()],
        ))
        .await?
    else {
        return Ok(Bound::Unknown(wanted));
    };
    let current: Option<Uuid> = found.try_get("", "organisation_id")?;
    match current {
        Some(current) if current == organisation => return Ok(Bound::Bound),
        Some(_) => {
            return Ok(Bound::Taken {
                installation: wanted,
                organisation: found.try_get("", "slug")?,
            });
        }
        None => {}
    }
    transaction
        .execute(statement(
            "UPDATE forge_installation SET organisation_id = NULL WHERE organisation_id = $1",
            [organisation.into()],
        ))
        .await?;
    transaction
        .execute(statement(
            "UPDATE forge_installation SET organisation_id = $2 WHERE id = $1",
            [installation.into(), organisation.into()],
        ))
        .await?;
    Ok(Bound::Bound)
}
