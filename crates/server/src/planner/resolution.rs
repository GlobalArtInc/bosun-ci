use bosun_db::Error;
use bosun_db::actions::Pin;
use bosun_db::runs::{JobState, Locked};
use bosun_types::actor::Actor;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::id::JobId;
use bosun_types::ir::{Action, JobKind, JobTemplate, Span};
use bosun_types::key::JobKey;
use bosun_types::status::{Conclusion, StatusReason};

use super::{Later, admit, follow, key, planned};
use crate::site::Site;

pub(crate) const UNRESOLVED: &str = "action_unresolved";

pub(crate) const NOWHERE: Span = Span {
    line: 1,
    column: 1,
    end_line: 1,
    end_column: 1,
};

pub(crate) struct Refusal {
    pub(crate) step: u64,
    pub(crate) span: Span,
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

pub(super) fn remote(template: &JobTemplate) -> bool {
    let JobKind::Steps(steps) = &template.kind else {
        return false;
    };
    steps
        .iter()
        .any(|step| matches!(step.action, Action::Uses(_)))
}

pub(crate) async fn proceed(
    run: &mut Locked<'_>,
    site: &Site,
    job: JobId,
    pins: &[Pin],
) -> Result<(), Error> {
    bosun_db::actions::pin(run.transaction(), job, pins).await?;
    let jobs = run.jobs().await?;
    let state = jobs
        .iter()
        .find(|state| state.id == job)
        .ok_or_else(|| Error::Vanished(format!("job {job}")))?;
    if !runs_in_its_group(run, state).await? {
        return Err(Error::Row {
            table: "concurrency_member",
            message: format!(
                "job {job} resolved its actions without running in the group of its `concurrency`"
            ),
        });
    }
    let mut later = Later::default();
    admit(run, state, None, None, false, &Actor::System, &mut later).await?;
    planned(run, site, None, &mut later).await?;
    follow(run, site, later).await
}

async fn runs_in_its_group(run: &Locked<'_>, job: &JobState) -> Result<bool, Error> {
    if job.concurrency.is_none() {
        return Ok(true);
    }
    Ok(run
        .memberships()
        .await?
        .iter()
        .any(|member| member.job_id == Some(job.id) && member.running))
}

pub(crate) async fn refuse(
    run: &mut Locked<'_>,
    site: &Site,
    job: (JobId, &JobKey),
    reason: StatusReason,
    refusals: &[Refusal],
) -> Result<(), Error> {
    let (id, job_key) = job;
    let facts = run.facts().await?;
    let file = super::file_of(
        &run.ir().await?,
        &facts.workflow,
        facts.repository.as_ref(),
        job_key,
    );
    for refusal in refusals {
        let diagnostic = Diagnostic {
            severity: Severity::Error,
            code: refusal.code.to_owned(),
            message: refusal.message.clone(),
            file: file.clone(),
            span: refusal.span,
            path: vec![
                key("jobs"),
                key(job_key.local()),
                key("steps"),
                PathSegment::Index(refusal.step),
                key("uses"),
            ],
        };
        run.notice(Some(id), &diagnostic).await?;
    }
    if reason == StatusReason::ActionForbidden {
        run.clear_continue_on_error(id).await?;
    }
    run.complete_job(id, Conclusion::Failure, Some(reason), &Actor::System)
        .await?;
    super::plan(run, site).await
}

pub(crate) async fn abandon(run: &Locked<'_>, job: JobId, message: String) -> Result<(), Error> {
    let file = run.facts().await?.workflow;
    let diagnostic = Diagnostic {
        severity: Severity::Error,
        code: UNRESOLVED.to_owned(),
        message,
        file,
        span: NOWHERE,
        path: Vec::new(),
    };
    run.notice(Some(job), &diagnostic).await?;
    run.complete_job(
        job,
        Conclusion::Failure,
        Some(StatusReason::ActionUnresolved),
        &Actor::System,
    )
    .await
}
