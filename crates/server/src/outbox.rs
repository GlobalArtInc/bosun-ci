use std::sync::Arc;
use std::time::{Duration, Instant};

use bosun_blob::{Key, Store as _};
use bosun_db::outbox::{
    Claimed, ForkStatus, Kind, Report, ReportedCheckRun, State, TokenRevocation,
};
use bosun_forge::{
    ANNOTATIONS_PER_REQUEST, CheckOutput, CheckRun, CheckStatus, CommitStatus, Forge as _,
    Repository, SUMMARY_LIMIT, StatusState,
};
use bosun_types::api::Annotation;
use bosun_types::id::JobId;
use bosun_types::status::{Conclusion, RunConclusion};
use chrono::{TimeDelta, Utc};
use sea_orm::{DatabaseConnection, TransactionTrait};
use serde::de::DeserializeOwned;
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::clock::Clock;
use crate::forge::Forge;
use crate::forge_token;
use crate::keys::Keyring;
use crate::metrics::Meters;
use crate::retry::{Failure, backoff};
use crate::site::Site;
use crate::store::Backend;

const POLL: Duration = Duration::from_secs(1);

const PAUSE: Duration = Duration::from_secs(1);

const LEASE: TimeDelta = TimeDelta::minutes(5);

const DELIVERY: Duration = Duration::from_secs(120);

const DEAD_AFTER: TimeDelta = TimeDelta::hours(24);

const NO_SUMMARY: &str = "The steps of this job wrote no summary.";

const CUT_NOTICE: &str = "\n\n---\n\nThe summaries of this job are longer than a check run \
     holds, so they end here; the job's page on Bosun shows them whole.\n";

const FORK_POLICY: &str = "bosun/fork-policy";

const FORK_REFUSED: &str = "Bosun runs no workflow for pull requests from forks in this project";

pub(crate) struct Outbox {
    pub(crate) db: DatabaseConnection,
    pub(crate) store: Arc<Backend>,
    pub(crate) forge: Arc<Forge>,
    pub(crate) keys: Arc<Keyring>,
    pub(crate) site: Site,
    pub(crate) clock: Clock,
    pub(crate) meters: Arc<Meters>,
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
    if result.is_err() {
        outbox.meters.failed_report(claimed.kind);
    }
    let now = outbox.clock.now();
    let transaction = outbox.db.begin().await?;
    let row = claimed.id;
    let kind = name(claimed.kind);
    let key = claimed.collapse_key.as_str();
    let kept = match result {
        Ok(()) => {
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

pub(crate) fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::CheckRun => "check_run",
        Kind::CommitStatus => "commit_status",
        Kind::ForkStatus => "fork_status",
        Kind::TokenRevocation => "token_revocation",
    }
}

fn payload<T: DeserializeOwned>(claimed: &Claimed) -> Result<T, Failure> {
    serde_json::from_value(claimed.payload.clone()).map_err(|error| {
        Failure::Refused(format!("the payload of the row cannot be read: {error}"))
    })
}

async fn repository(outbox: &Outbox, id: u64) -> Result<Repository, Failure> {
    let Some(seen) = bosun_db::installations::seen(&outbox.db, id).await? else {
        return Err(Failure::Refused(format!(
            "no installation of the GitHub App sees the repository {id}"
        )));
    };
    Ok(Repository {
        installation: seen.installation,
        owner: seen.owner,
        name: seen.name,
    })
}

async fn deliver(outbox: &Outbox, claimed: &Claimed) -> Result<(), Failure> {
    match claimed.kind {
        Kind::CheckRun => check_run(outbox, payload(claimed)?).await,
        Kind::CommitStatus => commit_status(outbox, payload(claimed)?).await,
        Kind::ForkStatus => fork_status(outbox, payload(claimed)?).await,
        Kind::TokenRevocation => revoke(outbox, payload(claimed)?).await,
    }
}

async fn revoke(outbox: &Outbox, revocation: TokenRevocation) -> Result<(), Failure> {
    let execution = revocation.execution_id;
    for token in bosun_db::forge_tokens::of_execution(&outbox.db, execution).await? {
        if token.expires_at > Utc::now() {
            match forge_token::open(&outbox.keys, &token) {
                Ok(secret) => match outbox.forge.app.revoke_token(&secret).await {
                    Ok(()) | Err(bosun_forge::Error::Status { status: 401, .. }) => {}
                    Err(error @ bosun_forge::Error::Status { status, .. })
                        if (400..500).contains(&status) =>
                    {
                        warn!(
                            %execution,
                            %error,
                            expires_at = %token.expires_at,
                            "the forge refused to revoke a forge token, so it lives until it expires"
                        );
                    }
                    Err(error) => return Err(error.into()),
                },
                Err(error) => warn!(
                    %execution,
                    %error,
                    expires_at = %token.expires_at,
                    "a forge token cannot be revoked, so it lives until it expires"
                ),
            }
        }
        bosun_db::forge_tokens::forget(&outbox.db, token.id).await?;
    }
    Ok(())
}

fn details(outbox: &Outbox, report: &Report) -> Option<String> {
    let address = match report.job_id {
        Some(job) => outbox.site.job_url(report.run_id, job),
        None => outbox.site.run_url(report.run_id),
    };
    Some(address).filter(|address| !address.is_empty())
}

async fn check_run(outbox: &Outbox, report: Report) -> Result<(), Failure> {
    let repository = repository(outbox, report.repository_id).await?;
    let app = &outbox.forge.app;
    let check_run = CheckRun {
        name: report.name.clone(),
        details_url: details(outbox, &report),
        status: check_status(report.state),
        output: None,
    };
    let Some(job) = report.job_id else {
        app.create_check_run(&repository, &report.sha, &check_run)
            .await?;
        return Ok(());
    };
    let output = match report.state {
        State::Completed(_) => results(outbox, job, &report.name).await?,
        State::Queued | State::InProgress => None,
    };
    check_run_of_job(outbox, &repository, &report.sha, job, check_run, output).await
}

async fn commit_status(outbox: &Outbox, report: Report) -> Result<(), Failure> {
    let repository = repository(outbox, report.repository_id).await?;
    let context = match report.job_id {
        Some(_) => format!("bosun/{}/{}", report.workflow, report.name),
        None => format!("bosun/{}", report.workflow),
    };
    let status = CommitStatus {
        context,
        state: status_state(report.state),
        description: Some(description(report.state).to_owned()),
        target_url: details(outbox, &report),
    };
    outbox
        .forge
        .app
        .create_status(&repository, &report.sha, &status)
        .await?;
    Ok(())
}

async fn fork_status(outbox: &Outbox, refused: ForkStatus) -> Result<(), Failure> {
    let repository = repository(outbox, refused.repository_id).await?;
    let status = CommitStatus {
        context: FORK_POLICY.to_owned(),
        state: StatusState::Error,
        description: Some(FORK_REFUSED.to_owned()),
        target_url: None,
    };
    outbox
        .forge
        .app
        .create_status(&repository, &refused.sha, &status)
        .await?;
    Ok(())
}

async fn check_run_of_job(
    outbox: &Outbox,
    repository: &Repository,
    sha: &str,
    job: JobId,
    mut check_run: CheckRun,
    output: Option<CheckOutput>,
) -> Result<(), Failure> {
    let app = &outbox.forge.app;
    let recorded = bosun_db::outbox::check_run(&outbox.db, job).await?;
    let (summary, annotations) = match output {
        Some(output) => (Some((output.title, output.summary)), output.annotations),
        None => (None, Vec::new()),
    };
    let sent = recorded.map_or(0, |recorded| recorded.annotations);
    let sent = usize::try_from(sent)
        .unwrap_or(usize::MAX)
        .min(annotations.len());
    let mut batches = annotations[sent..].chunks(ANNOTATIONS_PER_REQUEST);
    let mut batch = batches.next();
    let mut reported = recorded;
    loop {
        let annotations = batch.map(<[Annotation]>::to_vec).unwrap_or_default();
        let count = annotations.len() as u64;
        check_run.output = summary.as_ref().map(|(title, summary)| CheckOutput {
            title: title.clone(),
            summary: summary.clone(),
            annotations,
        });
        let next = match reported {
            Some(known) => {
                app.update_check_run(repository, known.id, &check_run)
                    .await?;
                ReportedCheckRun {
                    id: known.id,
                    annotations: known.annotations + count,
                }
            }
            None => ReportedCheckRun {
                id: app.create_check_run(repository, sha, &check_run).await?,
                annotations: count,
            },
        };
        bosun_db::outbox::record_check_run(&outbox.db, job, &next).await?;
        reported = Some(next);
        batch = batches.next();
        if batch.is_none() {
            return Ok(());
        }
    }
}

async fn results(outbox: &Outbox, job: JobId, name: &str) -> Result<Option<CheckOutput>, Failure> {
    let results = bosun_db::results::of_job(&outbox.db, job).await?;
    if results.summaries.is_empty() && results.annotations.is_empty() {
        return Ok(None);
    }
    let mut written: Vec<u8> = Vec::new();
    for summary in &results.summaries {
        if written.len() > SUMMARY_LIMIT {
            break;
        }
        let wanted = summary.size.min((SUMMARY_LIMIT + 1 - written.len()) as u64);
        let unreadable = |error: String| Failure::Retry {
            error: format!(
                "the step summary `{}` could not be read: {error}",
                summary.key
            ),
            at_least: Duration::ZERO,
        };
        let key = Key::new(summary.key.clone()).map_err(|error| unreadable(error.to_string()))?;
        let mut object = match outbox.store.get(&key, 0..wanted).await {
            Ok(object) => object,
            Err(bosun_blob::Error::NotFound(_)) => continue,
            Err(error) => return Err(unreadable(error.to_string())),
        };
        let mut text = Vec::new();
        object
            .read_to_end(&mut text)
            .await
            .map_err(|error| unreadable(error.to_string()))?;
        written.extend_from_slice(&text);
        if !written.ends_with(b"\n") {
            written.push(b'\n');
        }
    }
    let summary = if written.is_empty() {
        NO_SUMMARY.to_owned()
    } else {
        cut(String::from_utf8_lossy(&written).into_owned())
    };
    Ok(Some(CheckOutput {
        title: name.to_owned(),
        summary,
        annotations: results.annotations,
    }))
}

fn cut(mut summary: String) -> String {
    if summary.len() <= SUMMARY_LIMIT {
        return summary;
    }
    let mut end = SUMMARY_LIMIT - CUT_NOTICE.len();
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    summary.truncate(end);
    summary.push_str(CUT_NOTICE);
    summary
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

#[cfg(test)]
mod tests {
    use bosun_forge::SUMMARY_LIMIT;

    use super::{CUT_NOTICE, cut};

    #[test]
    fn a_summary_longer_than_the_forge_holds_is_cut_at_a_character_with_a_notice() {
        assert_eq!(cut("## Built\n".to_owned()), "## Built\n");
        let whole = "\u{e9}".repeat(SUMMARY_LIMIT / 2);
        assert_eq!(cut(whole.clone()), whole);
        let long = "\u{20ac}".repeat(SUMMARY_LIMIT / 3 + 1);
        let shown = cut(long.clone());
        assert!(shown.len() <= SUMMARY_LIMIT, "{}", shown.len());
        let kept = shown
            .strip_suffix(CUT_NOTICE)
            .expect("the notice at the end");
        assert!(long.starts_with(kept));
        assert!(SUMMARY_LIMIT - CUT_NOTICE.len() - kept.len() < 3);
    }
}
