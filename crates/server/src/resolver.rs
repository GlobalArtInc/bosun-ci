use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bosun_db::Error;
use bosun_db::actions::Pin;
use bosun_db::resolutions::{self, Claimed, Renewal, Standing, Unreadable, Use};
use bosun_forge::{Forge as _, Repository};
use bosun_types::action_policy::ActionPolicy;
use bosun_types::id::JobId;
use bosun_types::ir::ActionReference;
use bosun_types::status::StatusReason;
use sea_orm::prelude::Uuid;
use sea_orm::{DatabaseConnection, TransactionTrait};
use tokio::sync::Notify;
use tokio::task::{JoinError, JoinSet};
use tokio::time::{MissedTickBehavior, interval, timeout};
use tracing::{error, info, warn};

use crate::config::Resolution;
use crate::forge::Forge;
use crate::planner::{self, NOWHERE, Refusal, UNRESOLVED};
use crate::site::Site;

const MOST_AT_ONCE: u32 = 16;

const POLL: Duration = Duration::from_secs(1);

const FORBIDDEN: &str = "action_forbidden";

const MOST_PATTERNS_SHOWN: usize = 10;

enum Answer {
    Pinned(Vec<Pin>),
    Missing { at: usize, message: String },
    Unanswered { at: usize, error: String },
    Skipped,
}

pub(crate) struct Resolver {
    db: DatabaseConnection,
    forge: Option<Arc<Forge>>,
    site: Site,
    settings: Resolution,
}

impl Resolver {
    pub(crate) fn new(
        db: DatabaseConnection,
        forge: Option<Arc<Forge>>,
        site: Site,
        settings: Resolution,
    ) -> Arc<Resolver> {
        Arc::new(Resolver {
            db,
            forge,
            site,
            settings,
        })
    }

    fn lease(&self) -> Duration {
        self.settings.request_timeout.saturating_mul(2)
    }

    async fn claim(&self, room: u32) -> Result<Vec<Claimed>, Error> {
        let taken = resolutions::claim(&self.db, room, self.lease()).await?;
        for unreadable in taken.unreadable {
            self.postpone(unreadable).await;
        }
        Ok(taken.claimed)
    }

    async fn postpone(&self, unreadable: Unreadable) {
        let Unreadable { job, token, error } = unreadable;
        warn!(
            job_id = %job,
            %error,
            "a job waiting to be resolved cannot be read"
        );
        let overdue = resolutions::overdue(&self.db, job, token, self.settings.give_up).await;
        match (overdue, JobId::try_from(job)) {
            (Ok(true), Ok(id)) => self.abandon(id, token, &error).await,
            (Ok(_), _) => self.defer(job, token).await,
            (Err(error), _) => warn!(
                job_id = %job,
                %error,
                "the time an unreadable job has waited could not be read; its lease returns it to the resolvers"
            ),
        }
    }

    async fn defer(&self, job: Uuid, token: Uuid) {
        let deferred = async {
            let transaction = self.db.begin().await?;
            resolutions::retry(
                &transaction,
                job,
                token,
                self.settings.backoff_min,
                self.settings.backoff_max,
            )
            .await?;
            transaction.commit().await?;
            Ok::<(), Error>(())
        }
        .await;
        if let Err(error) = deferred {
            warn!(
                job_id = %job,
                %error,
                "a job that cannot be read could not be postponed; its lease returns it to the resolvers"
            );
        }
    }

    async fn abandon(&self, job: JobId, token: Uuid, unreadable: &Error) {
        match self.abandoned(job, token, true).await {
            Ok(true) => error!(
                job_id = %job,
                error = %unreadable,
                "a job whose record cannot be read was failed with action_unresolved after the give-up limit"
            ),
            Ok(false) => {}
            Err(failure @ (Error::Row { .. } | Error::Json(_))) => {
                match self.abandoned(job, token, false).await {
                    Ok(true) => error!(
                        job_id = %job,
                        error = %unreadable,
                        %failure,
                        "a job whose record cannot be read was failed with action_unresolved after the give-up limit, but its run could not be planned and may wait for ever"
                    ),
                    Ok(false) => {}
                    Err(error) => warn!(
                        job_id = %job,
                        %error,
                        "a job that cannot be read could not be failed; its lease returns it to the resolvers"
                    ),
                }
            }
            Err(error) => warn!(
                job_id = %job,
                %error,
                "a job that cannot be read could not be failed; its lease returns it to the resolvers"
            ),
        }
    }

    async fn abandoned(&self, job: JobId, token: Uuid, plan: bool) -> Result<bool, Error> {
        let transaction = self.db.begin().await?;
        let Some(mut run) = bosun_db::runs::lock_job(&transaction, job).await? else {
            return Ok(false);
        };
        if !resolutions::overdue(&transaction, job.into(), token, self.settings.give_up).await? {
            return Ok(false);
        }
        let message = format!(
            "the actions of the job cannot be resolved: the server cannot read the record of \
             the job; gave up after {:?}",
            self.settings.give_up
        );
        planner::abandon(&run, job, message).await?;
        if plan {
            planner::plan(&mut run, &self.site).await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    pub(crate) async fn work(self: Arc<Self>, wake: Arc<Notify>) {
        let mut running = JoinSet::new();
        let mut ticks = interval(POLL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            while let Some(done) = running.try_join_next() {
                reaped(done);
            }
            let busy = u32::try_from(running.len()).unwrap_or(u32::MAX);
            let room = MOST_AT_ONCE.saturating_sub(busy);
            if room > 0 {
                match self.claim(room).await {
                    Ok(claimed) => {
                        for job in claimed {
                            let resolver = Arc::clone(&self);
                            running.spawn(async move { resolver.resolve(job).await });
                        }
                    }
                    Err(error) => warn!(%error, "a claim of jobs to resolve failed"),
                }
            }
            tokio::select! {
                _ = ticks.tick() => {}
                _ = wake.notified() => {}
                Some(done) = running.join_next(), if !running.is_empty() => reaped(done),
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn once(self: &Arc<Self>) -> usize {
        let claimed = self
            .claim(u32::MAX)
            .await
            .expect("a claim of jobs to resolve");
        let count = claimed.len();
        let mut running = JoinSet::new();
        for job in claimed {
            let resolver = Arc::clone(self);
            running.spawn(async move { resolver.resolve(job).await });
        }
        while let Some(done) = running.join_next().await {
            done.expect("a resolution");
        }
        count
    }

    pub(crate) async fn resolve(&self, claimed: Claimed) {
        let Some(answer) = self.ask(&claimed).await else {
            return;
        };
        if let Err(error) = self.settle(&claimed, answer).await {
            warn!(
                job_id = %claimed.job_id,
                run_id = %claimed.run_id,
                %error,
                "the resolution of a job could not be recorded; its lease returns it to the resolvers"
            );
        }
    }

    async fn ask(&self, claimed: &Claimed) -> Option<Answer> {
        if forbidden(&claimed.policy, claimed.account.as_deref(), &claimed.uses).is_some() {
            return Some(Answer::Skipped);
        }
        let Some(forge) = &self.forge else {
            return Some(Answer::Missing {
                at: 0,
                message: format!(
                    "the action `{}` cannot be resolved: this server has no GitHub App to \
                     resolve actions with",
                    named(claimed, 0)
                ),
            });
        };
        self.look(forge, claimed).await
    }

    async fn look(&self, forge: &Forge, claimed: &Claimed) -> Option<Answer> {
        let mut seen = HashSet::new();
        let mut pins = Vec::new();
        for (at, used) in claimed.uses.iter().enumerate() {
            let reference = &used.reference;
            let owner = reference.owner.to_lowercase();
            let repository = reference.repository.to_lowercase();
            if !seen.insert((owner.clone(), repository.clone(), reference.r#ref.clone())) {
                continue;
            }
            if !pins.is_empty() {
                match self.keep(claimed).await {
                    Renewal::Held => {}
                    Renewal::Lost => return None,
                    Renewal::Expired => {
                        return Some(Answer::Unanswered {
                            at,
                            error: format!(
                                "the action `{reference}` cannot be resolved: the forge did not \
                                 finish within the limit"
                            ),
                        });
                    }
                }
            }
            let request = async {
                match claimed.installation {
                    Some(installation) => {
                        forge
                            .app
                            .resolve(
                                &Repository {
                                    installation,
                                    owner: reference.owner.clone(),
                                    name: reference.repository.clone(),
                                },
                                &reference.r#ref,
                            )
                            .await
                    }
                    None => {
                        forge
                            .app
                            .resolve_public(
                                &reference.owner,
                                &reference.repository,
                                &reference.r#ref,
                            )
                            .await
                    }
                }
            };
            let resolved = match timeout(self.settings.request_timeout, request).await {
                Ok(resolved) => resolved,
                Err(_) => {
                    return Some(Answer::Unanswered {
                        at,
                        error: format!(
                            "the action `{reference}` cannot be resolved: the forge did not \
                             answer within {:?}",
                            self.settings.request_timeout
                        ),
                    });
                }
            };
            match resolved {
                Ok(Some(commit)) => pins.push(Pin {
                    owner,
                    repository,
                    r#ref: reference.r#ref.clone(),
                    commit: commit.to_lowercase(),
                }),
                Ok(None) => {
                    return Some(Answer::Missing {
                        at,
                        message: missing(claimed, reference),
                    });
                }
                Err(error) => {
                    return Some(Answer::Unanswered {
                        at,
                        error: format!("the action `{reference}` cannot be resolved: {error}"),
                    });
                }
            }
        }
        Some(Answer::Pinned(pins))
    }

    async fn keep(&self, claimed: &Claimed) -> Renewal {
        match resolutions::renew(
            &self.db,
            claimed.job_id,
            claimed.token,
            self.lease(),
            self.settings.give_up,
        )
        .await
        {
            Ok(Renewal::Lost) => {
                info!(
                    job_id = %claimed.job_id,
                    "the job was decided or claimed by another resolver while its actions were resolved; the pass ends"
                );
                Renewal::Lost
            }
            Ok(renewal) => renewal,
            Err(error) => {
                warn!(
                    job_id = %claimed.job_id,
                    %error,
                    "the lease of a claim could not be renewed; its lease returns the job to the resolvers"
                );
                Renewal::Lost
            }
        }
    }

    async fn settle(&self, claimed: &Claimed, answer: Answer) -> Result<(), Error> {
        let transaction = self.db.begin().await?;
        let Some(mut run) = bosun_db::runs::lock_job(&transaction, claimed.job_id).await? else {
            return Ok(());
        };
        let Some(standing) = resolutions::standing(
            &transaction,
            claimed.job_id,
            claimed.token,
            self.settings.give_up,
        )
        .await?
        else {
            return Ok(());
        };
        let job = (claimed.job_id, &claimed.key);
        if let Some(refusals) =
            forbidden(&standing.policy, standing.account.as_deref(), &claimed.uses)
                .map(|uses| refusals(claimed, &standing, &uses))
        {
            planner::refuse_resolution(
                &mut run,
                &self.site,
                job,
                StatusReason::ActionForbidden,
                &refusals,
            )
            .await?;
            info!(job_id = %claimed.job_id, "a job was failed because its actions are forbidden");
            transaction.commit().await?;
            return Ok(());
        }
        match answer {
            Answer::Pinned(pins) => {
                planner::proceed(&mut run, &self.site, claimed.job_id, &pins).await?;
            }
            Answer::Missing { at, message } => {
                let refusal = unresolved(claimed, at, message);
                planner::refuse_resolution(
                    &mut run,
                    &self.site,
                    job,
                    StatusReason::ActionUnresolved,
                    &[refusal],
                )
                .await?;
            }
            Answer::Unanswered { at, error } if standing.expired => {
                let message = format!("{error}; gave up after {:?}", self.settings.give_up);
                let refusal = unresolved(claimed, at, message);
                planner::refuse_resolution(
                    &mut run,
                    &self.site,
                    job,
                    StatusReason::ActionUnresolved,
                    &[refusal],
                )
                .await?;
            }
            Answer::Unanswered { at, error } => {
                warn!(
                    job_id = %claimed.job_id,
                    action = %named(claimed, at),
                    %error,
                    "the forge did not resolve an action; the job is asked again after a backoff"
                );
                resolutions::retry(
                    &transaction,
                    claimed.job_id,
                    claimed.token,
                    self.settings.backoff_min,
                    self.settings.backoff_max,
                )
                .await?;
            }
            Answer::Skipped => {
                resolutions::release(&transaction, claimed.job_id, claimed.token).await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }
}

fn reaped(done: Result<(), JoinError>) {
    if let Err(error) = done {
        error!(%error, "a task that resolves the actions of a job panicked or was cancelled");
    }
}

fn missing(claimed: &Claimed, reference: &ActionReference) -> String {
    let (owner, name, wanted) = (
        reference.owner.as_str(),
        reference.repository.as_str(),
        reference.r#ref.as_str(),
    );
    let problem = match claimed.installation {
        Some(_) => format!(
            "the installation of the GitHub App of organisation `{}` sees no repository \
             `{owner}/{name}` with a ref `{wanted}`",
            claimed.organisation
        ),
        None => format!(
            "organisation `{}` has no installation of the GitHub App, so only public actions \
             resolve, and no public repository `{owner}/{name}` has a ref `{wanted}`",
            claimed.organisation
        ),
    };
    format!("the action `{reference}` cannot be resolved: {problem}")
}

fn named(claimed: &Claimed, at: usize) -> String {
    claimed
        .uses
        .get(at)
        .map_or_else(String::new, |used| used.reference.to_string())
}

fn unresolved(claimed: &Claimed, at: usize, message: String) -> Refusal {
    let used = claimed.uses.get(at).or_else(|| claimed.uses.first());
    Refusal {
        step: used.map_or(0, |used| used.step),
        span: used.map_or(NOWHERE, |used| used.span),
        code: UNRESOLVED,
        message,
    }
}

fn forbidden<'a>(
    policy: &ActionPolicy,
    account: Option<&str>,
    uses: &'a [Use],
) -> Option<Vec<&'a Use>> {
    let mut seen = HashSet::new();
    let found: Vec<&Use> = uses
        .iter()
        .filter(|used| !policy.permits(account, &used.reference))
        .filter(|used| seen.insert(used.reference.to_string()))
        .collect();
    (!found.is_empty()).then_some(found)
}

pub(crate) fn permitted(policy: &ActionPolicy, account: Option<&str>) -> String {
    match policy {
        ActionPolicy::All {} => String::new(),
        ActionPolicy::Organisation {} => match account {
            Some(account) => {
                format!("it permits only actions owned by the account `{account}`")
            }
            None => "it permits only actions owned by the account of the installation, and \
                     the installation has no account login"
                .to_owned(),
        },
        ActionPolicy::Listed { patterns } if patterns.is_empty() => {
            "it lists no pattern, so it permits no action".to_owned()
        }
        ActionPolicy::Listed { patterns } => {
            let shown: Vec<String> = patterns
                .iter()
                .take(MOST_PATTERNS_SHOWN)
                .map(ToString::to_string)
                .collect();
            let more = patterns.len().saturating_sub(MOST_PATTERNS_SHOWN);
            let tail = if more == 0 {
                String::new()
            } else {
                format!(", and {more} more")
            };
            format!(
                "it permits only actions that match {}{tail}",
                shown.join(", ")
            )
        }
    }
}

fn refusals(claimed: &Claimed, standing: &Standing, uses: &[&Use]) -> Vec<Refusal> {
    let allowed = permitted(&standing.policy, standing.account.as_deref());
    uses.iter()
        .map(|used| Refusal {
            step: used.step,
            span: used.span,
            code: FORBIDDEN,
            message: format!(
                "the action `{}` is forbidden by the action policy of organisation `{}`: {allowed}",
                used.reference, claimed.organisation
            ),
        })
        .collect()
}
