mod instants;

use std::time::Duration;

use bosun_db::schedules::{Due, Entry, Origin, Scope};
use bosun_types::api::{Run, WorkflowSource};
use bosun_types::id::{RevisionId, ScheduleId, WorkflowId};
use bosun_types::ir::{Schedule, Trigger};
use bosun_workflow::github;
use chrono::{DateTime, Timelike, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, TransactionTrait};
use tracing::{info, warn};

use crate::clock::Clock;
use crate::forge_events::{self, Scheduled};
use crate::site::Site;

const UTC: &str = "UTC";

const MINUTE: Duration = Duration::from_secs(60);

const MARGIN: Duration = Duration::from_secs(1);

pub(crate) struct Source<'a> {
    pub(crate) workflow_id: WorkflowId,
    pub(crate) revision_id: RevisionId,
    pub(crate) triggers: &'a [Trigger],
}

pub(crate) async fn rebuild<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    sources: &[Source<'_>],
    origin: Origin<'_>,
    now: DateTime<Utc>,
) -> Result<(), bosun_db::Error> {
    let entries: Vec<Entry<'_>> = sources
        .iter()
        .flat_map(|source| {
            source
                .triggers
                .iter()
                .filter_map(|trigger| match trigger {
                    Trigger::Schedule(schedules) => Some(schedules),
                    _ => None,
                })
                .flatten()
                .map(move |schedule| entry(source, schedule, now))
        })
        .collect();
    let (project_id, workflow_id) = match &scope {
        Scope::Repository(project) => (Some(*project), None),
        Scope::Workflow(workflow) => (None, Some(*workflow)),
    };
    let replaced = bosun_db::schedules::replace(db, scope, &entries, origin).await?;
    if replaced.added > 0 || replaced.removed > 0 {
        info!(
            project_id = project_id.map(|id| id.to_string()),
            workflow_id = workflow_id.map(|id| id.to_string()),
            added = replaced.added,
            kept = replaced.kept,
            removed = replaced.removed,
            "rebuilt the schedules of workflows"
        );
    }
    Ok(())
}

fn entry<'a>(source: &Source<'_>, schedule: &'a Schedule, now: DateTime<Utc>) -> Entry<'a> {
    let timezone = schedule.timezone.as_deref().unwrap_or(UTC);
    Entry {
        workflow_id: source.workflow_id,
        revision_id: source.revision_id,
        expression: &schedule.cron.expression,
        timezone,
        cron: &schedule.cron,
        next_at: instants::zone(timezone)
            .and_then(|zone| instants::next(&schedule.cron, zone, now)),
    }
}

pub(crate) async fn work(db: DatabaseConnection, site: Site, clock: Clock) {
    loop {
        tokio::time::sleep(until_next_minute(clock.now())).await;
        if let Err(error) = round(&db, &site, clock.now()).await {
            warn!(%error, "the schedules that came due could not be fired");
        }
    }
}

fn until_next_minute(now: DateTime<Utc>) -> Duration {
    let into = Duration::from_secs(u64::from(now.second()))
        + Duration::from_nanos(u64::from(now.nanosecond()));
    MINUTE.saturating_sub(into) + MARGIN
}

pub(crate) async fn round(
    db: &DatabaseConnection,
    site: &Site,
    now: DateTime<Utc>,
) -> Result<Vec<Run>, bosun_db::Error> {
    let Some(due) = bosun_db::schedules::due(db, now).await? else {
        return Ok(Vec::new());
    };
    let mut runs = Vec::new();
    for id in due {
        match fire(db, site, id, now).await {
            Ok(Some(run)) => runs.push(run),
            Ok(None) => {}
            Err(error) => warn!(
                schedule_id = %id,
                %error,
                "a schedule that came due could not be fired, and the next round tries it again"
            ),
        }
    }
    Ok(runs)
}

async fn fire(
    db: &DatabaseConnection,
    site: &Site,
    id: ScheduleId,
    now: DateTime<Utc>,
) -> Result<Option<Run>, bosun_db::Error> {
    let transaction = db.begin().await?;
    let Some(due) = bosun_db::schedules::lock_due(&transaction, id, now).await? else {
        return Ok(None);
    };
    let next_at =
        instants::zone(&due.timezone).and_then(|zone| instants::next(&due.cron, zone, now));
    bosun_db::schedules::advance(&transaction, id, next_at).await?;
    let run = planned(&transaction, site, &due).await?;
    transaction.commit().await?;
    Ok(run)
}

async fn planned(
    transaction: &DatabaseTransaction,
    site: &Site,
    due: &Due,
) -> Result<Option<Run>, bosun_db::Error> {
    let workflow = bosun_db::workflows::find(transaction, due.workflow_id)
        .await?
        .ok_or_else(|| bosun_db::Error::Vanished(format!("workflow {}", due.workflow_id)))?;
    if !workflow.enabled {
        info!(
            workflow_id = %workflow.id,
            cron = due.expression,
            timezone = due.timezone,
            "the schedule of a disabled workflow came due and starts no run"
        );
        return Ok(None);
    }
    if workflow.source == WorkflowSource::Repository {
        let seen = bosun_db::projects::binding(transaction, workflow.project_id)
            .await?
            .is_some_and(|binding| binding.installation.is_some());
        if !seen {
            info!(
                workflow_id = %workflow.id,
                cron = due.expression,
                timezone = due.timezone,
                "the schedule of a repository's workflow came due while no installation of the \
                 GitHub App sees the repository, and starts no run"
            );
            return Ok(None);
        }
    }
    let revision = bosun_db::workflows::revision(transaction, due.revision_id)
        .await?
        .ok_or_else(|| bosun_db::Error::Vanished(format!("revision {}", due.revision_id)))?;
    let payload = github::schedule_payload(&due.expression);
    let run = forge_events::scheduled(
        transaction,
        site,
        Scheduled {
            workflow: &workflow,
            revision: &revision,
            git_ref: due.git_ref.as_deref(),
            sha: due.sha.as_deref(),
            payload: &payload,
            actor: &due.actor,
            at: due.next_at,
        },
    )
    .await?;
    if let Some(run) = &run {
        info!(
            run_id = %run.id,
            workflow_id = %workflow.id,
            cron = due.expression,
            timezone = due.timezone,
            "started a run on a schedule"
        );
    }
    Ok(run)
}
