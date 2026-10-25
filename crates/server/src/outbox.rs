use std::sync::Arc;
use std::time::{Duration, Instant};

use bosun_db::outbox::{Claimed, Kind, Report, State};
use bosun_forge::{CheckRun, CheckStatus, CommitStatus, Forge as _, Repository, StatusState};
use bosun_types::id::JobId;
use bosun_types::status::{Conclusion, RunConclusion};
use chrono::TimeDelta;
use sea_orm::{DatabaseConnection, TransactionTrait};
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::clock::Clock;
use crate::forge::Forge;
use crate::retry::{Failure, backoff};
use crate::site::Site;

const POLL: Duration = Duration::from_secs(1);

const PAUSE: Duration = Duration::from_secs(1);

const LEASE: TimeDelta = TimeDelta::minutes(5);

const DELIVERY: Duration = Duration::from_secs(120);

const DEAD_AFTER: TimeDelta = TimeDelta::hours(24);

pub(crate) struct Outbox {
    pub(crate) db: DatabaseConnection,
    pub(crate) forge: Arc<Forge>,
    pub(crate) site: Site,
    pub(crate) clock: Clock,
}

pub(crate) async fn work(outbox: Outbox, wake: Arc<Notify>) {
    loop {
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        match next(&outbox).await {
            Ok(true) => {}
            Ok(false) => {
                tokio::select! {
                    () = notified => {}
                    () = tokio::time::sleep(POLL) => {}
                }
            }
            Err(error) => {
                warn!(%error, "the outbox could not be delivered");
                tokio::time::sleep(PAUSE).await;
            }
        }
    }
}

pub(crate) async fn next(outbox: &Outbox) -> Result<bool, bosun_db::Error> {
    let now = outbox.clock.now();
    let transaction = outbox.db.begin().await?;
    let claimed = bosun_db::outbox::claim(&transaction, now, now + LEASE).await?;
    transaction.commit().await?;
    let Some(claimed) = claimed else {
        return Ok(false);
    };
    let started = Instant::now();
    let result = match tokio::time::timeout(DELIVERY, deliver(outbox, &claimed)).await {
        Ok(result) => result,
        Err(_) => Err(Failure::Retry {
            error: format!("the delivery took longer than {}s", DELIVERY.as_secs()),
            at_least: Duration::ZERO,
        }),
    };
    let now = outbox.clock.now();
    let transaction = outbox.db.begin().await?;
    let row = claimed.id;
    let kind = name(claimed.kind);
    let key = claimed.collapse_key.as_str();
    let kept = match result {
        Ok(created) => {
            if let Some((job, id)) = created {
                bosun_db::outbox::record_check_run(&transaction, job, id).await?;
            }
            let kept = bosun_db::outbox::delivered(&transaction, &claimed).await?;
            if kept {
                info!(
                    row,
                    kind,
                    key,
                    dropped = claimed.dropped,
                    duration_ms = started.elapsed().as_millis(),
                    "delivered an outbox row"
                );
            }
            kept
        }
        Err(Failure::Retry { error, at_least }) => {
            let failing_since = claimed.failing_since.unwrap_or(now);
            if now - failing_since >= DEAD_AFTER {
                let kept =
                    bosun_db::outbox::kill(&transaction, &claimed, &error, failing_since, now)
                        .await?;
                if kept {
                    dead(&claimed, &error);
                }
                kept
            } else {
                let wait = backoff(claimed.attempts).max(at_least);
                let at = now + TimeDelta::from_std(wait).unwrap_or(LEASE);
                let kept =
                    bosun_db::outbox::postpone(&transaction, &claimed, &error, failing_since, at)
                        .await?;
                if kept {
                    warn!(
                        row,
                        kind,
                        key,
                        error,
                        wait_ms = wait.as_millis(),
                        "an outbox row could not be delivered and is tried again"
                    );
                }
                kept
            }
        }
        Err(Failure::Refused(error)) => {
            let failing_since = claimed.failing_since.unwrap_or(now);
            let kept =
                bosun_db::outbox::kill(&transaction, &claimed, &error, failing_since, now).await?;
            if kept {
                dead(&claimed, &error);
            }
            kept
        }
    };
    transaction.commit().await?;
    if !kept {
        warn!(
            row,
            kind,
            key,
            "an outbox row was claimed again before its delivery ended, and the later claim settles it"
        );
    }
    Ok(true)
}

fn dead(claimed: &Claimed, error: &str) {
    error!(
        row = claimed.id,
        kind = name(claimed.kind),
        key = claimed.collapse_key,
        attempts = claimed.attempts + 1,
        error,
        "an outbox row is dead and is no longer tried"
    );
}

fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::CheckRun => "check_run",
        Kind::CommitStatus => "commit_status",
    }
}

async fn deliver(outbox: &Outbox, claimed: &Claimed) -> Result<Option<(JobId, u64)>, Failure> {
    let report: Report = serde_json::from_value(claimed.payload.clone()).map_err(|error| {
        Failure::Refused(format!("the payload of the row cannot be read: {error}"))
    })?;
    let Some(seen) = bosun_db::installations::seen(&outbox.db, report.repository_id).await? else {
        return Err(Failure::Refused(format!(
            "no installation of the GitHub App sees the repository {}",
            report.repository_id
        )));
    };
    let repository = Repository {
        installation: seen.installation,
        owner: seen.owner,
        name: seen.name,
    };
    let address = match report.job_id {
        Some(job) => outbox.site.job_url(report.run_id, job),
        None => outbox.site.run_url(report.run_id),
    };
    let details = Some(address).filter(|address| !address.is_empty());
    let app = &outbox.forge.app;
    match claimed.kind {
        Kind::CheckRun => {
            let check_run = CheckRun {
                name: report.name.clone(),
                details_url: details,
                status: check_status(report.state),
            };
            let Some(job) = report.job_id else {
                app.create_check_run(&repository, &report.sha, &check_run)
                    .await?;
                return Ok(None);
            };
            match bosun_db::outbox::check_run(&outbox.db, job).await? {
                Some(id) => {
                    app.update_check_run(&repository, id, &check_run).await?;
                    Ok(None)
                }
                None => {
                    let id = app
                        .create_check_run(&repository, &report.sha, &check_run)
                        .await?;
                    Ok(Some((job, id)))
                }
            }
        }
        Kind::CommitStatus => {
            let context = match report.job_id {
                Some(_) => format!("bosun/{}/{}", report.workflow, report.name),
                None => format!("bosun/{}", report.workflow),
            };
            let status = CommitStatus {
                context,
                state: status_state(report.state),
                description: Some(description(report.state).to_owned()),
                target_url: details,
            };
            app.create_status(&repository, &report.sha, &status).await?;
            Ok(None)
        }
    }
}

fn check_status(state: State) -> CheckStatus {
    match state {
        State::Queued => CheckStatus::Queued,
        State::InProgress => CheckStatus::InProgress,
        State::Completed(conclusion) => CheckStatus::Completed(match conclusion {
            RunConclusion::Success => Conclusion::Success,
            RunConclusion::Failure | RunConclusion::StartupFailure => Conclusion::Failure,
            RunConclusion::Cancelled => Conclusion::Cancelled,
            RunConclusion::Skipped => Conclusion::Skipped,
            RunConclusion::TimedOut => Conclusion::TimedOut,
        }),
    }
}

fn status_state(state: State) -> StatusState {
    match state {
        State::Queued | State::InProgress => StatusState::Pending,
        State::Completed(RunConclusion::Success | RunConclusion::Skipped) => StatusState::Success,
        State::Completed(
            RunConclusion::Failure | RunConclusion::TimedOut | RunConclusion::StartupFailure,
        ) => StatusState::Failure,
        State::Completed(RunConclusion::Cancelled) => StatusState::Error,
    }
}

fn description(state: State) -> &'static str {
    match state {
        State::Queued => "Queued",
        State::InProgress => "In progress",
        State::Completed(RunConclusion::Success) => "Succeeded",
        State::Completed(RunConclusion::Failure) => "Failed",
        State::Completed(RunConclusion::Cancelled) => "Cancelled",
        State::Completed(RunConclusion::Skipped) => "Skipped",
        State::Completed(RunConclusion::TimedOut) => "Timed out",
        State::Completed(RunConclusion::StartupFailure) => "The workflow could not start",
    }
}
