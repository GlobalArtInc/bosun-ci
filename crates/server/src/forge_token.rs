use std::sync::Arc;

use bosun_db::forge_tokens::{Grant, Holding, NewForgeToken, StoredForgeToken};
use bosun_db::queue::{ClaimedJob, ClaimedRepository};
use bosun_forge::{Access, Forge as _, InstallationToken, Permissions};
use bosun_proto::agent::v1::ForgeToken;
use bosun_types::api::{ForgeAccess, ForgePermission};
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::id::{AgentId, ExecutionId};
use bosun_types::ir::{self, Evaluable, Expr, JobTemplate, Scope, Segment, Template};
use bosun_types::status::ExecutionStatus;
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use tracing::{info, warn};

use crate::forge::Forge;
use crate::keys::{FORGE_TOKEN, Keyring};

pub(crate) const UNAVAILABLE: &str = "token_unavailable";

const PERMISSION_UNAVAILABLE: &str = "permission_unavailable";

const FRESH: TimeDelta = TimeDelta::minutes(50);

const READ_ONLY: [ForgePermission; 2] = [ForgePermission::Metadata, ForgePermission::Models];

pub(crate) struct Tokens {
    db: DatabaseConnection,
    forge: Option<Arc<Forge>>,
    keys: Arc<Keyring>,
}

pub(crate) struct Fresh {
    id: Uuid,
    token: InstallationToken,
    repository_id: u64,
    granted: Grant,
}

pub(crate) struct Lacking {
    permission: ForgePermission,
    wanted: ForgeAccess,
    held: Option<ForgeAccess>,
}

pub(crate) enum Minted {
    Tokenless,
    Token {
        fresh: Box<Fresh>,
        lacking: Vec<Lacking>,
    },
    Failed(String),
}

pub(crate) struct Issued {
    pub(crate) token: String,
    pub(crate) expires_at: DateTime<Utc>,
}

enum Source {
    Job,
    Workflow,
    Setting,
}

impl Issued {
    pub(crate) fn message(self) -> ForgeToken {
        let left = (self.expires_at - Utc::now()).max(TimeDelta::zero());
        ForgeToken {
            token: self.token,
            expires_in: Some(prost_types::Duration {
                seconds: left.num_seconds(),
                nanos: left.subsec_nanos(),
            }),
        }
    }
}

impl Fresh {
    pub(crate) fn issued(&self) -> Issued {
        Issued {
            token: self.token.token.clone(),
            expires_at: self.token.expires_at,
        }
    }
}

impl Tokens {
    pub(crate) fn new(
        db: DatabaseConnection,
        forge: Option<Arc<Forge>>,
        keys: Arc<Keyring>,
    ) -> Tokens {
        Tokens { db, forge, keys }
    }

    pub(crate) fn keys(&self) -> &Keyring {
        &self.keys
    }

    pub(crate) async fn mint(&self, claimed: &ClaimedJob) -> Minted {
        let Some(repository) = &claimed.repository else {
            return Minted::Tokenless;
        };
        let full_name = format!("{}/{}", repository.owner, repository.name);
        let Some(forge) = &self.forge else {
            return Minted::Failed(format!(
                "the forge token of job `{}` cannot be minted for the repository `{full_name}`: \
                 this server has no GitHub App to mint it with",
                claimed.key
            ));
        };
        match mint(forge, claimed, repository).await {
            Ok((token, granted, lacking)) => Minted::Token {
                fresh: Box::new(Fresh {
                    id: Uuid::now_v7(),
                    token,
                    repository_id: repository.id,
                    granted,
                }),
                lacking,
            },
            Err(error) => Minted::Failed(format!(
                "the forge token of job `{}` cannot be minted for the repository `{full_name}`: \
                 {error}",
                claimed.key
            )),
        }
    }

    pub(crate) fn discard(&self, minted: Minted) {
        if let Minted::Token { fresh, .. } = minted {
            self.revoke_later(fresh.token);
        }
    }

    fn revoke_later(&self, token: InstallationToken) {
        let Some(forge) = self.forge.clone() else {
            return;
        };
        tokio::spawn(async move {
            match forge.app.revoke_token(&token.token).await {
                Ok(()) => info!("revoked a forge token that no job received"),
                Err(error) => warn!(
                    %error,
                    expires_at = %token.expires_at,
                    "a forge token that no job received could not be revoked, so it lives until \
                     it expires"
                ),
            }
        });
    }

    pub(crate) async fn renew(
        &self,
        agent: AgentId,
        execution: ExecutionId,
    ) -> Result<Issued, String> {
        let unreadable = |error: bosun_db::Error| {
            warn!(%execution, %error, "the forge token of an execution could not be read");
            format!("the server could not read the forge token of the execution: {error}")
        };
        let current = match bosun_db::forge_tokens::holding(&self.db, agent, execution)
            .await
            .map_err(unreadable)?
        {
            Holding::Token(current) => current,
            Holding::NoToken => {
                return Err(format!(
                    "the job of the execution `{execution}` has no forge token, since its project \
                     has no repository that an installation of the GitHub App sees"
                ));
            }
            Holding::Ended => return Err(format!("the execution `{execution}` has ended")),
            Holding::Elsewhere => {
                return Err(format!(
                    "the execution `{execution}` does not run on this agent"
                ));
            }
        };
        if current.expires_at - Utc::now() > FRESH {
            return open(&self.keys, &current).map(|token| Issued {
                token,
                expires_at: current.expires_at,
            });
        }
        let fresh = self.mint_again(&current).await?;
        match self.keep(agent, execution, &fresh).await {
            Ok(true) => {
                info!(
                    %execution,
                    expires_at = %fresh.token.expires_at,
                    "renewed the forge token of an execution"
                );
                Ok(fresh.issued())
            }
            Ok(false) => {
                self.revoke_later(fresh.token);
                Err(format!(
                    "the execution `{execution}` ended while its forge token was renewed"
                ))
            }
            Err(error) => {
                self.revoke_later(fresh.token);
                Err(unreadable(error))
            }
        }
    }

    async fn keep(
        &self,
        agent: AgentId,
        execution: ExecutionId,
        fresh: &Fresh,
    ) -> Result<bool, bosun_db::Error> {
        let transaction = self.db.begin().await?;
        let running = bosun_db::runs::lock_execution(&transaction, execution)
            .await?
            .is_some_and(|(_, state)| {
                state.agent_id == agent && state.status != ExecutionStatus::Completed
            });
        if !running {
            return Ok(false);
        }
        store(&transaction, &self.keys, execution, fresh).await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn mint_again(&self, current: &StoredForgeToken) -> Result<Fresh, String> {
        let forge = self
            .forge
            .as_ref()
            .ok_or("this server has no GitHub App to mint a forge token with")?;
        let seen = bosun_db::installations::seen(&self.db, current.repository_id)
            .await
            .map_err(|error| {
                format!("the server could not read the repository of the forge token: {error}")
            })?
            .ok_or_else(|| {
                format!(
                    "no installation of the GitHub App sees the repository {} any more",
                    current.repository_id
                )
            })?;
        let token = forge
            .app
            .mint_token(
                seen.installation,
                std::slice::from_ref(&seen.name),
                &requested(&current.permissions),
            )
            .await
            .map_err(|error| {
                format!(
                    "a fresh forge token cannot be minted for the repository `{}/{}`: {error}",
                    seen.owner, seen.name
                )
            })?;
        Ok(Fresh {
            id: Uuid::now_v7(),
            token,
            repository_id: current.repository_id,
            granted: current.permissions.clone(),
        })
    }
}

async fn mint(
    forge: &Forge,
    claimed: &ClaimedJob,
    repository: &ClaimedRepository,
) -> Result<(InstallationToken, Grant, Vec<Lacking>), String> {
    let held = grant(
        &forge
            .app
            .installation_permissions(repository.installation)
            .await
            .map_err(|error| error.to_string())?,
    );
    let (_, permissions) = declared(&claimed.job, claimed.permissions.as_ref());
    let wanted = wanted(permissions, &claimed.token_permissions, claimed.trusted);
    let (mut granted, lacking) = intersect(&wanted, &held);
    if granted.is_empty() {
        if !held.contains_key(&ForgePermission::Metadata) {
            return Err(
                "the installation of the GitHub App holds none of the permissions the job asks \
                 for, not even `metadata`"
                    .to_owned(),
            );
        }
        granted.insert(ForgePermission::Metadata, ForgeAccess::Read);
    }
    let token = forge
        .app
        .mint_token(
            repository.installation,
            std::slice::from_ref(&repository.name),
            &requested(&granted),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok((token, granted, lacking))
}

fn wanted(declared: Option<&ir::Permissions>, setting: &Grant, trusted: bool) -> Grant {
    let mut wanted = match declared {
        None => setting.clone(),
        Some(ir::Permissions::ReadAll) => ForgePermission::ALL
            .into_iter()
            .map(|permission| (permission, ForgeAccess::Read))
            .collect(),
        Some(ir::Permissions::WriteAll) => ForgePermission::ALL
            .into_iter()
            .map(|permission| {
                let access = if READ_ONLY.contains(&permission) {
                    ForgeAccess::Read
                } else {
                    ForgeAccess::Write
                };
                (permission, access)
            })
            .collect(),
        Some(ir::Permissions::Scopes(scopes)) => scopes
            .iter()
            .filter_map(|(scope, access)| {
                let access = match access {
                    ir::Access::Read => ForgeAccess::Read,
                    ir::Access::Write => ForgeAccess::Write,
                    ir::Access::None => return None,
                };
                Some((permission(*scope)?, access))
            })
            .collect(),
    };
    if !trusted {
        for access in wanted.values_mut() {
            *access = ForgeAccess::Read;
        }
    }
    wanted
}

fn declared<'a>(
    job: &'a JobTemplate,
    workflow: Option<&'a ir::Permissions>,
) -> (Source, Option<&'a ir::Permissions>) {
    match (&job.permissions, workflow) {
        (Some(job), _) => (Source::Job, Some(job)),
        (None, Some(workflow)) => (Source::Workflow, Some(workflow)),
        (None, None) => (Source::Setting, None),
    }
}

fn permission(scope: Scope) -> Option<ForgePermission> {
    Some(match scope {
        Scope::Actions => ForgePermission::Actions,
        Scope::Attestations => ForgePermission::Attestations,
        Scope::Checks => ForgePermission::Checks,
        Scope::Contents => ForgePermission::Contents,
        Scope::Deployments => ForgePermission::Deployments,
        Scope::Discussions => ForgePermission::Discussions,
        Scope::IdToken => return None,
        Scope::Issues => ForgePermission::Issues,
        Scope::Models => ForgePermission::Models,
        Scope::Packages => ForgePermission::Packages,
        Scope::Pages => ForgePermission::Pages,
        Scope::PullRequests => ForgePermission::PullRequests,
        Scope::RepositoryProjects => ForgePermission::RepositoryProjects,
        Scope::SecurityEvents => ForgePermission::SecurityEvents,
        Scope::Statuses => ForgePermission::Statuses,
    })
}

fn grant(held: &Permissions) -> Grant {
    ForgePermission::ALL
        .into_iter()
        .filter_map(|permission| {
            let access = match held.get(permission.name())? {
                Access::Read => ForgeAccess::Read,
                Access::Write => ForgeAccess::Write,
            };
            Some((permission, access))
        })
        .collect()
}

fn intersect(wanted: &Grant, held: &Grant) -> (Grant, Vec<Lacking>) {
    let mut granted = Grant::new();
    let mut lacking = Vec::new();
    for (&permission, &access) in wanted {
        let holds = held.get(&permission).copied();
        match holds {
            Some(holds) if holds >= access => {
                granted.insert(permission, access);
            }
            Some(holds) => {
                granted.insert(permission, holds);
                lacking.push(Lacking {
                    permission,
                    wanted: access,
                    held: Some(holds),
                });
            }
            None => lacking.push(Lacking {
                permission,
                wanted: access,
                held: None,
            }),
        }
    }
    (granted, lacking)
}

fn requested(granted: &Grant) -> Permissions {
    granted
        .iter()
        .map(|(permission, access)| {
            let access = match access {
                ForgeAccess::Read => Access::Read,
                ForgeAccess::Write => Access::Write,
            };
            (permission.name().to_owned(), access)
        })
        .collect()
}

pub(crate) fn notices(claimed: &ClaimedJob, lacking: &[Lacking], file: &str) -> Vec<Diagnostic> {
    let (source, _) = declared(&claimed.job, claimed.permissions.as_ref());
    let mut path = vec![PathSegment::Key("jobs".to_owned())];
    path.push(PathSegment::Key(claimed.key.to_string()));
    let asked = match source {
        Source::Job => {
            path.push(PathSegment::Key("permissions".to_owned()));
            format!("the `permissions` of job `{}`", claimed.key)
        }
        Source::Workflow => {
            path = vec![PathSegment::Key("permissions".to_owned())];
            "the `permissions` of the workflow".to_owned()
        }
        Source::Setting => "the project's `token_permissions`".to_owned(),
    };
    lacking
        .iter()
        .map(|lacking| {
            let name = lacking.permission.name();
            let wanted = lacking.wanted.name();
            let message = match lacking.held {
                Some(held) => format!(
                    "{asked} ask the forge token for `{name}: {wanted}`, and the installation of \
                     the GitHub App holds `{name}: {}`, so the token of job `{}` gets `{name}: {}`",
                    held.name(),
                    claimed.key,
                    held.name()
                ),
                None => format!(
                    "{asked} ask the forge token for `{name}: {wanted}`, which the installation of \
                     the GitHub App does not hold, so the token of job `{}` is minted without it",
                    claimed.key
                ),
            };
            Diagnostic {
                severity: Severity::Warning,
                code: PERMISSION_UNAVAILABLE.to_owned(),
                message,
                file: file.to_owned(),
                span: claimed.job.span,
                path: path.clone(),
            }
        })
        .collect()
}

fn binding(id: Uuid, execution: ExecutionId) -> Vec<u8> {
    let mut binding = id.as_bytes().to_vec();
    binding.extend_from_slice(Uuid::from(execution).as_bytes());
    binding
}

pub(crate) async fn store<C: ConnectionTrait>(
    db: &C,
    keys: &Keyring,
    execution: ExecutionId,
    fresh: &Fresh,
) -> Result<(), bosun_db::Error> {
    let sealed = keys
        .seal_as(
            &FORGE_TOKEN,
            &binding(fresh.id, execution),
            fresh.token.token.as_bytes(),
        )
        .map_err(|error| bosun_db::Error::Row {
            table: "forge_token",
            message: error.to_string(),
        })?;
    bosun_db::forge_tokens::store(
        db,
        &NewForgeToken {
            id: fresh.id,
            execution_id: execution,
            repository_id: fresh.repository_id,
            permissions: &fresh.granted,
            sealed: &sealed,
            expires_at: fresh.token.expires_at,
        },
    )
    .await
}

pub(crate) fn open(keys: &Keyring, stored: &StoredForgeToken) -> Result<String, String> {
    keys.open_as(
        &FORGE_TOKEN,
        &binding(stored.id, stored.execution_id),
        &stored.sealed,
    )
    .map_err(|reason| format!("the forge token does not decrypt: {reason}"))
    .and_then(|bytes| {
        String::from_utf8(bytes).map_err(|_| "the forge token is not text".to_owned())
    })
}

pub(crate) fn read_by(job: &JobTemplate) -> bool {
    let shell = match &job.defaults.shell {
        Some(Evaluable::Template(template)) => Some(template),
        Some(Evaluable::Literal(_)) | None => None,
    };
    job.env
        .values()
        .chain(shell)
        .chain(job.defaults.working_directory.as_ref())
        .any(reads_token)
}

fn reads_token(template: &Template) -> bool {
    template.segments().iter().any(|segment| match segment {
        Segment::Expression(expr) => reads(expr),
        Segment::Text(_) => false,
    })
}

fn github(expr: &Expr) -> bool {
    matches!(expr, Expr::Context { name, .. } if name.eq_ignore_ascii_case("github"))
}

fn reads(expr: &Expr) -> bool {
    match expr {
        Expr::Property { target, name, .. } if github(target) => name.eq_ignore_ascii_case("token"),
        Expr::Index { target, index, .. } if github(target) => match index.as_ref() {
            Expr::String { value, .. } => value.eq_ignore_ascii_case("token"),
            Expr::Null { .. } | Expr::Boolean { .. } | Expr::Number { .. } => false,
            _ => true,
        },
        Expr::Context { .. } => github(expr),
        Expr::Null { .. } | Expr::Boolean { .. } | Expr::Number { .. } | Expr::String { .. } => {
            false
        }
        Expr::Property { target, .. } | Expr::Filter { target, .. } => reads(target),
        Expr::Index { target, index, .. } => reads(target) || reads(index),
        Expr::Not { operand, .. } => reads(operand),
        Expr::Compare { left, right, .. } => reads(left) || reads(right),
        Expr::And { operands, .. } | Expr::Or { operands, .. } => operands.iter().any(reads),
        Expr::Call { arguments, .. } => arguments.iter().any(reads),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bosun_types::api::{ForgeAccess, ForgePermission};
    use bosun_types::ir::{Access, Permissions, Scope};

    use super::{Grant, intersect, reads_token, wanted};

    fn grant(pairs: &[(ForgePermission, ForgeAccess)]) -> Grant {
        pairs.iter().copied().collect()
    }

    fn setting() -> Grant {
        grant(&[
            (ForgePermission::Contents, ForgeAccess::Read),
            (ForgePermission::Metadata, ForgeAccess::Read),
        ])
    }

    #[test]
    fn scopes_map_to_the_permissions_of_the_app_without_none_and_id_token() {
        let scopes = Permissions::Scopes(BTreeMap::from([
            (Scope::Contents, Access::Read),
            (Scope::Issues, Access::Write),
            (Scope::PullRequests, Access::Write),
            (Scope::IdToken, Access::Write),
            (Scope::Packages, Access::None),
        ]));
        assert_eq!(
            wanted(Some(&scopes), &setting(), true),
            grant(&[
                (ForgePermission::Contents, ForgeAccess::Read),
                (ForgePermission::Issues, ForgeAccess::Write),
                (ForgePermission::PullRequests, ForgeAccess::Write),
            ])
        );
        assert_eq!(
            wanted(
                Some(&Permissions::Scopes(BTreeMap::new())),
                &setting(),
                true
            ),
            Grant::new()
        );
    }

    #[test]
    fn without_permissions_the_setting_applies_and_the_two_forms_name_every_permission() {
        assert_eq!(wanted(None, &setting(), true), setting());
        let read_all = wanted(Some(&Permissions::ReadAll), &setting(), true);
        assert_eq!(read_all.len(), ForgePermission::ALL.len());
        assert!(read_all.values().all(|access| *access == ForgeAccess::Read));
        let write_all = wanted(Some(&Permissions::WriteAll), &setting(), true);
        assert_eq!(write_all.len(), ForgePermission::ALL.len());
        for (permission, access) in write_all {
            let expected = match permission {
                ForgePermission::Metadata | ForgePermission::Models => ForgeAccess::Read,
                _ => ForgeAccess::Write,
            };
            assert_eq!(access, expected, "{permission:?}");
        }
    }

    #[test]
    fn an_untrusted_run_asks_for_read_whatever_it_declares() {
        let untrusted = wanted(Some(&Permissions::WriteAll), &setting(), false);
        assert_eq!(untrusted.len(), ForgePermission::ALL.len());
        assert!(
            untrusted
                .values()
                .all(|access| *access == ForgeAccess::Read)
        );
        let writing = grant(&[(ForgePermission::Contents, ForgeAccess::Write)]);
        assert_eq!(
            wanted(None, &writing, false),
            grant(&[(ForgePermission::Contents, ForgeAccess::Read)])
        );
    }

    #[test]
    fn what_the_installation_lacks_is_left_out_or_lowered_and_named() {
        let (granted, lacking) = intersect(
            &grant(&[
                (ForgePermission::Contents, ForgeAccess::Write),
                (ForgePermission::Issues, ForgeAccess::Write),
                (ForgePermission::Pages, ForgeAccess::Read),
            ]),
            &grant(&[
                (ForgePermission::Contents, ForgeAccess::Read),
                (ForgePermission::Issues, ForgeAccess::Write),
                (ForgePermission::Metadata, ForgeAccess::Read),
            ]),
        );
        assert_eq!(
            granted,
            grant(&[
                (ForgePermission::Contents, ForgeAccess::Read),
                (ForgePermission::Issues, ForgeAccess::Write),
            ])
        );
        let named: Vec<_> = lacking
            .iter()
            .map(|lacking| (lacking.permission, lacking.wanted, lacking.held))
            .collect();
        assert_eq!(
            named,
            [
                (
                    ForgePermission::Contents,
                    ForgeAccess::Write,
                    Some(ForgeAccess::Read)
                ),
                (ForgePermission::Pages, ForgeAccess::Read, None),
            ]
        );
    }

    #[test]
    fn an_expression_reads_the_token_through_its_property_an_index_or_the_whole_context() {
        let reads = |text: &str| {
            reads_token(
                &bosun_expr::template(text).unwrap_or_else(|error| panic!("{text}: {error:?}")),
            )
        };
        for text in [
            "${{ github.token }}",
            "Bearer ${{ GitHub.Token }}",
            "${{ github['TOKEN'] }}",
            "${{ github[format('to{0}', 'ken')] }}",
            "${{ toJSON(github) }}",
            "${{ join(github.*, ',') }}",
            "${{ github.token != '' && 'set' || 'unset' }}",
        ] {
            assert!(reads(text), "{text}");
        }
        for text in [
            "${{ github.sha }}",
            "${{ github['ref'] }}",
            "${{ secrets.GITHUB_TOKEN }}",
            "${{ env.token }}",
            "github.token",
        ] {
            assert!(!reads(text), "{text}");
        }
    }
}
