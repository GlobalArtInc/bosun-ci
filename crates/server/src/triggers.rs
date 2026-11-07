use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bosun_db::outbox::{Claimed, RunTrigger, Transition};
use bosun_db::triggers::{Entry, Key, Origin, Scope, Source, Subscription, Unreadable};
use bosun_types::actor::Actor;
use bosun_types::api::{Project, ProjectRef, Run, UpstreamEvent, Workflow, WorkflowRef};
use bosun_types::event::{SkipReason, TriggerKind, TriggerSkipped};
use bosun_types::id::{ProjectId, UserId};
use bosun_types::permission::Permission;
use bosun_types::status::RunStatus;
use bosun_workflow::github::{self, Account, FinishedRun};
use bosun_workflow::{Event, matches};
use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde_json::Value;
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::access;
use crate::clock::Clock;
use crate::forge::Forge;
use crate::forge_events::{self, Followed};
use crate::metrics::Meters;
use crate::outbox::{DELIVERY, LEASE, PAUSE, POLL, settle};
use crate::planner;
use crate::retry::Failure;
use crate::schedules::Source as Current;
use crate::site::Site;

const LEVELS: u32 = 3;

const WORKFLOW_RUN: &str = "workflow_run";

const UPSTREAM: &str = "upstream";

pub(crate) struct Triggers {
    pub(crate) db: DatabaseConnection,
    pub(crate) site: Site,
    pub(crate) forge: Option<Arc<Forge>>,
    pub(crate) clock: Clock,
    pub(crate) meters: Arc<Meters>,
}

pub(crate) async fn rebuild<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    sources: &[Current<'_>],
    origin: Origin<'_>,
) -> Result<(), bosun_db::Error> {
    let entries: Vec<Entry<'_>> = sources
        .iter()
        .map(|source| Entry {
            workflow_id: source.workflow_id,
            revision_id: source.revision_id,
            triggers: source.triggers,
        })
        .collect();
    let replaced = bosun_db::triggers::replace(db, scope, &entries, origin).await?;
    if replaced.added > 0 || replaced.removed > 0 {
        info!(
            added = replaced.added,
            kept = replaced.kept,
            removed = replaced.removed,
            "rebuilt the subscriptions of workflows to the runs of others"
        );
    }
    Ok(())
}

pub(crate) async fn work(triggers: Triggers, wake: Arc<Notify>) {
    loop {
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        match next(&triggers).await {
            Ok(true) => {}
            Ok(false) => {
                tokio::select! {
                    () = notified => {}
                    () = tokio::time::sleep(POLL) => {}
                }
            }
            Err(error) => {
                warn!(%error, "the runs that others trigger could not be created");
                tokio::time::sleep(PAUSE).await;
            }
        }
    }
}

pub(crate) async fn next(triggers: &Triggers) -> Result<bool, bosun_db::Error> {
    let now = triggers.clock.now();
    let transaction = triggers.db.begin().await?;
    let claimed = bosun_db::outbox::claim_triggers(&transaction, now, now + LEASE).await?;
    transaction.commit().await?;
    let Some(claimed) = claimed else {
        return Ok(false);
    };
    let started = Instant::now();
    let result = match tokio::time::timeout(DELIVERY, process(triggers, &claimed)).await {
        Ok(result) => result,
        Err(_) => Err(Failure::Retry {
            error: format!("the triggers took longer than {}s", DELIVERY.as_secs()),
            at_least: Duration::ZERO,
        }),
    };
    settle(
        &triggers.db,
        &triggers.clock,
        &triggers.meters,
        &claimed,
        result,
        started,
    )
    .await?;
    Ok(true)
}

pub(crate) async fn process(triggers: &Triggers, claimed: &Claimed) -> Result<(), Failure> {
    let row: RunTrigger = serde_json::from_value(claimed.payload.clone()).map_err(|error| {
        Failure::Refused(format!("the payload of the row cannot be read: {error}"))
    })?;
    let Some(source) = bosun_db::triggers::source(&triggers.db, row.run_id, row.attempt_id).await?
    else {
        info!(
            run_id = %row.run_id,
            "a run that others would follow was removed before they did, so nothing follows it"
        );
        return Ok(());
    };
    if row.awaiting_admission {
        info!(
            run_id = %row.run_id,
            "the run was awaiting its admission when it was requested, so nothing follows that"
        );
        return Ok(());
    }
    if source.awaiting_admission {
        info!(
            run_id = %row.run_id,
            transition = row.transition.name(),
            "a run awaits its admission, so nothing follows it yet"
        );
        return Ok(());
    }
    let subscribers =
        bosun_db::triggers::subscribers(&triggers.db, source.project.id, &source.project.path)
            .await?;
    let mut groups: Vec<Group<'_>> = Vec::new();
    for subscription in &subscribers.found {
        if fires(&source, &row, subscription) {
            group(&mut groups, subscription.project_id)
                .subscriptions
                .push(subscription);
        }
    }
    for unreadable in &subscribers.unreadable {
        group(&mut groups, unreadable.project_id)
            .unreadable
            .push(unreadable);
    }
    let mut failure = None;
    for group in &groups {
        if let Err(error) = follow(triggers, &source, &row, group).await {
            warn!(
                run_id = %row.run_id,
                project_id = %group.project,
                %error,
                "workflows of a project that follow a run could not be started, and the row is \
                 tried again"
            );
            failure.get_or_insert(error);
        }
    }
    match failure {
        None => Ok(()),
        Some(error) => Err(Failure::from(error)),
    }
}

struct Group<'a> {
    project: ProjectId,
    subscriptions: Vec<&'a Subscription>,
    unreadable: Vec<&'a Unreadable>,
}

fn group<'g, 'a>(groups: &'g mut Vec<Group<'a>>, project: ProjectId) -> &'g mut Group<'a> {
    let at = match groups.iter().position(|group| group.project == project) {
        Some(at) => at,
        None => {
            groups.push(Group {
                project,
                subscriptions: Vec::new(),
                unreadable: Vec::new(),
            });
            groups.len() - 1
        }
    };
    &mut groups[at]
}

static WARNED: LazyLock<Mutex<HashSet<Uuid>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn warn_unreadable(unreadable: &Unreadable) {
    let first = WARNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(unreadable.id);
    if first {
        warn!(
            subscription_id = %unreadable.id,
            error = unreadable.error,
            "a subscription to the runs of others cannot be read and is skipped"
        );
    }
}

struct Candidate<'a> {
    subscription: &'a Subscription,
    workflow: Workflow,
}

enum Verdict {
    Fire,
    Skip(SkipReason),
}

async fn follow(
    triggers: &Triggers,
    source: &Source,
    row: &RunTrigger,
    group: &Group<'_>,
) -> Result<(), bosun_db::Error> {
    let db = &triggers.db;
    if group.project != source.run.project_id
        && !bosun_db::triggers::follows(db, source.run.project_id, group.project).await?
    {
        return Ok(());
    }
    for unreadable in &group.unreadable {
        warn_unreadable(unreadable);
    }
    let mut failure = None;
    let mut candidates = Vec::new();
    for subscription in &group.subscriptions {
        match bosun_db::workflows::find(db, subscription.workflow_id).await {
            Ok(Some(workflow)) if workflow.enabled => candidates.push(Candidate {
                subscription,
                workflow,
            }),
            Ok(Some(workflow)) => info!(
                workflow_id = %workflow.id,
                run_id = %row.run_id,
                "a disabled workflow follows a run and starts none"
            ),
            Ok(None) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    if !candidates.is_empty() {
        let depth = bosun_db::triggers::depth(db, row.run_id).await? + 1;
        match verdict(db, source, group.project, depth, triggers.clock.now()).await? {
            Verdict::Skip(reason) => {
                skip(triggers, row, group.project, depth, reason, &candidates).await?;
            }
            Verdict::Fire => {
                for candidate in &candidates {
                    if let Err(error) = fire(triggers, source, row, depth, candidate).await {
                        failure.get_or_insert(error);
                    }
                }
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

fn key(row: &RunTrigger, workflow: &Workflow) -> Key {
    Key {
        run_id: row.run_id,
        attempt_id: row.attempt_id,
        workflow_id: workflow.id,
        transition: row.transition,
    }
}

async fn skip(
    triggers: &Triggers,
    row: &RunTrigger,
    project: ProjectId,
    depth: u32,
    reason: SkipReason,
    candidates: &[Candidate<'_>],
) -> Result<(), bosun_db::Error> {
    let Some(first) = candidates.first() else {
        return Ok(());
    };
    let transaction = triggers.db.begin().await?;
    let mut reserved = false;
    for candidate in candidates {
        reserved |= bosun_db::triggers::reserve(
            &transaction,
            &key(row, &candidate.workflow),
            depth,
            Some(reason),
        )
        .await?;
    }
    if reserved {
        let recorded = bosun_db::triggers::record_skip(
            &transaction,
            &Key {
                run_id: row.run_id,
                attempt_id: row.attempt_id,
                workflow_id: first.workflow.id,
                transition: row.transition,
            },
            TriggerSkipped {
                project_id: project,
                reason,
            },
        )
        .await?;
        if recorded {
            info!(
                run_id = %row.run_id,
                project_id = %project,
                ?reason,
                "workflows of a project that follows a run start none"
            );
        }
    }
    transaction.commit().await?;
    Ok(())
}

async fn fire(
    triggers: &Triggers,
    source: &Source,
    row: &RunTrigger,
    depth: u32,
    candidate: &Candidate<'_>,
) -> Result<(), bosun_db::Error> {
    let Candidate {
        subscription,
        workflow,
    } = candidate;
    let key = key(row, workflow);
    let transaction = triggers.db.begin().await?;
    if bosun_db::triggers::reserve(&transaction, &key, depth, None).await? {
        let revision = bosun_db::workflows::revision(&transaction, subscription.revision_id)
            .await?
            .ok_or_else(|| {
                bosun_db::Error::Vanished(format!(
                    "revision {} of workflow {}",
                    subscription.revision_id, workflow.id
                ))
            })?;
        let payload = payload(&triggers.site, source, row, subscription.kind)?;
        let run = forge_events::followed(
            &transaction,
            &triggers.site,
            triggers.forge.as_deref(),
            Followed {
                workflow,
                revision: &revision,
                event: trigger_name(subscription.kind),
                git_ref: subscription.git_ref.as_deref(),
                sha: subscription.sha.as_deref(),
                payload: &payload,
                actor: &source.run.actor,
                at: triggers.clock.now(),
            },
        )
        .await?;
        bosun_db::triggers::attach(&transaction, &key, run.id).await?;
        info!(
            run_id = %run.id,
            source_run_id = %row.run_id,
            workflow_id = %workflow.id,
            trigger = trigger_name(subscription.kind),
            depth,
            "started a run that follows another"
        );
    }
    transaction.commit().await?;
    Ok(())
}

fn trigger_name(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::WorkflowRun => WORKFLOW_RUN,
        TriggerKind::Upstream => UPSTREAM,
    }
}

fn fires(source: &Source, row: &RunTrigger, subscription: &Subscription) -> bool {
    let head = github::head_branch(
        &source.run.event,
        source.run.r#ref.as_deref(),
        &source.payload,
    )
    .map(|branch| format!("refs/heads/{branch}"));
    matches(
        std::slice::from_ref(&subscription.trigger),
        &Event {
            name: trigger_name(subscription.kind),
            action: Some(row.transition.name()),
            r#ref: head.as_deref(),
            base_ref: None,
            changed_files: None,
            workflow: Some(&source.workflow),
            conclusion: row.conclusion.map(|conclusion| conclusion.name()),
        },
    )
}

async fn verdict<C: ConnectionTrait>(
    db: &C,
    source: &Source,
    project: ProjectId,
    depth: u32,
    now: DateTime<Utc>,
) -> Result<Verdict, bosun_db::Error> {
    if depth > LEVELS {
        return Ok(Verdict::Skip(SkipReason::ChainTooLong));
    }
    if project == source.run.project_id {
        return Ok(Verdict::Fire);
    }
    let Some(subscriber) = bosun_db::projects::find(db, project).await? else {
        return Ok(Verdict::Skip(SkipReason::NoPermission));
    };
    Ok(
        match may_dispatch(db, &source.run.actor, &subscriber, now).await? {
            Ok(()) => Verdict::Fire,
            Err(reason) => Verdict::Skip(reason),
        },
    )
}

async fn may_dispatch<C: ConnectionTrait>(
    db: &C,
    actor: &Actor,
    subscriber: &Project,
    now: DateTime<Utc>,
) -> Result<Result<(), SkipReason>, bosun_db::Error> {
    let (user, carried) = match actor {
        Actor::User { id, .. } => (Some(*id), None),
        Actor::Token { id, user_id, .. } => match bosun_db::tokens::carried(db, *id, now).await? {
            Some(carried) => (Some(*user_id), Some(carried)),
            None => return Ok(Err(SkipReason::NoPermission)),
        },
        Actor::Forge { id, .. } => (linked_user(db, *id).await?, None),
        Actor::Agent { .. } | Actor::System => (None, None),
    };
    let Some(user) = user else {
        return Ok(Err(SkipReason::NoUser));
    };
    let Some(user) = bosun_db::users::find(db, user).await? else {
        return Ok(Err(SkipReason::NoUser));
    };
    let permissions = access::load(db, &user, carried.as_ref()).await?;
    let held = permissions.on_project(subscriber.organisation_id, subscriber.id);
    Ok(if held.contains(&Permission::RunDispatch) {
        Ok(())
    } else {
        Err(SkipReason::NoPermission)
    })
}

async fn linked_user<C: ConnectionTrait>(
    _db: &C,
    _forge_account: u64,
) -> Result<Option<UserId>, bosun_db::Error> {
    Ok(None)
}

fn payload(
    site: &Site,
    source: &Source,
    row: &RunTrigger,
    kind: TriggerKind,
) -> Result<Value, bosun_db::Error> {
    let run = &source.run;
    let (status, started_at, completed_at) = match row.transition {
        Transition::Requested => (RunStatus::Queued, None, None),
        Transition::InProgress => (RunStatus::InProgress, run.started_at, None),
        Transition::Completed => (RunStatus::Completed, run.started_at, run.completed_at),
    };
    match kind {
        TriggerKind::WorkflowRun => {
            let stamp =
                |at: chrono::DateTime<chrono::Utc>| at.to_rfc3339_opts(SecondsFormat::Secs, true);
            let status_text = match row.transition {
                Transition::Requested => "queued",
                Transition::InProgress => "in_progress",
                Transition::Completed => "completed",
            };
            Ok(github::workflow_run_payload(&FinishedRun {
                action: row.transition.name().to_owned(),
                id: run.global_number,
                name: source.workflow.clone(),
                path: source.file.clone(),
                display_title: run.display_name.clone(),
                head_branch: github::head_branch(&run.event, run.r#ref.as_deref(), &source.payload),
                head_sha: github::head_sha(&run.event, run.sha.as_deref(), &source.payload),
                event: run.event.clone(),
                status: status_text.to_owned(),
                conclusion: row
                    .conclusion
                    .map(|conclusion| conclusion.name().to_owned()),
                run_number: run.number,
                run_attempt: source.attempt,
                html_url: Some(site.run_url(run.id)).filter(|address| !address.is_empty()),
                created_at: stamp(run.created_at),
                updated_at: stamp(completed_at.or(started_at).unwrap_or(run.created_at)),
                run_started_at: started_at.map(stamp),
                head_repository: github::head_repository(
                    &run.event,
                    source
                        .project
                        .repository
                        .as_ref()
                        .map(|repository| format!("{}/{}", repository.owner, repository.name))
                        .as_deref(),
                    &source.payload,
                ),
                actor: Account {
                    login: planner::login(&run.actor).unwrap_or_default().to_owned(),
                    id: match &run.actor {
                        Actor::Forge { id, .. } => Some(*id),
                        _ => None,
                    },
                },
            }))
        }
        TriggerKind::Upstream => Ok(serde_json::to_value(UpstreamEvent {
            run: Run {
                status,
                conclusion: row.conclusion,
                attempt: source.attempt,
                notices: Vec::new(),
                started_at,
                completed_at,
                ..run.clone()
            },
            project: ProjectRef {
                id: source.project.id,
                organisation_id: source.project.organisation_id,
                path: source.project.path.clone(),
            },
            workflow: WorkflowRef {
                id: run.workflow_id,
                name: source.workflow.clone(),
            },
        })?),
    }
}
