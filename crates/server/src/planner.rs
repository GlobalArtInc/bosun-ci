use std::collections::{HashMap, HashSet};

use bosun_db::Error;
use bosun_db::runs::{JobState, Locked, NewRun};
use bosun_types::actor::Actor;
use bosun_types::api::Run;
use bosun_types::ir::{self, Evaluable, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason};
use sea_orm::DatabaseTransaction;

pub(crate) async fn dispatch(
    transaction: &DatabaseTransaction,
    new: NewRun<'_>,
) -> Result<Run, Error> {
    let run = bosun_db::runs::dispatch(transaction, new).await?;
    let mut locked = bosun_db::runs::lock(transaction, run.id)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {}", run.id)))?;
    plan(&mut locked).await?;
    Ok(run)
}

pub(crate) async fn plan(run: &mut Locked<'_>) -> Result<(), Error> {
    let mut jobs = run.jobs().await?;
    let mut workflow = None;
    while let Some(index) = ready(&jobs) {
        let job = &jobs[index];
        if ancestors_succeeded(&jobs, index) {
            let workflow = match &workflow {
                Some(workflow) => workflow,
                None => workflow.insert(run.ir().await?),
            };
            let (labels, group) = runs_on(workflow, &job.key)?;
            let pools = bosun_db::pools::eligible(run.transaction(), group.as_deref()).await?;
            run.queue_job(job.id, &labels, &pools).await?;
            jobs[index].status = JobStatus::Queued;
        } else {
            run.complete_job(
                job.id,
                Conclusion::Skipped,
                Some(StatusReason::IfFalse),
                &Actor::System,
            )
            .await?;
            jobs[index].status = JobStatus::Completed;
            jobs[index].conclusion = Some(Conclusion::Skipped);
        }
    }
    settle(run, &jobs).await
}

fn ready(jobs: &[JobState]) -> Option<usize> {
    let completed: HashSet<&JobKey> = jobs
        .iter()
        .filter(|job| job.status == JobStatus::Completed)
        .map(|job| &job.key)
        .collect();
    jobs.iter().position(|job| {
        job.status == JobStatus::Pending && job.needs.iter().all(|need| completed.contains(need))
    })
}

fn ancestors_succeeded(jobs: &[JobState], index: usize) -> bool {
    let by_key: HashMap<&JobKey, &JobState> = jobs.iter().map(|job| (&job.key, job)).collect();
    let mut seen = HashSet::new();
    let mut open: Vec<&JobKey> = jobs[index].needs.iter().collect();
    while let Some(key) = open.pop() {
        if !seen.insert(key) {
            continue;
        }
        match by_key.get(key) {
            Some(need) if need.conclusion == Some(Conclusion::Success) => {
                open.extend(need.needs.iter());
            }
            _ => return false,
        }
    }
    true
}

pub(crate) fn unapplied(workflow: &ir::Workflow) -> Option<String> {
    let (what, step) = match &workflow.concurrency {
        Some(_) => ("`concurrency` of the workflow".to_owned(), 17),
        None => workflow.jobs.iter().find_map(|job| {
            [
                (job.r#if.is_some(), "`if`", 15),
                (
                    placement(&job.runs_on).is_none(),
                    "an expression in `runs-on`",
                    15,
                ),
                (
                    job.continue_on_error != Evaluable::Literal(false),
                    "`continue-on-error`",
                    15,
                ),
                (job.strategy.is_some(), "`strategy`", 16),
                (job.concurrency.is_some(), "`concurrency`", 17),
                (uses_an_action(job), "a `uses` step", 33),
            ]
            .into_iter()
            .find(|(used, ..)| *used)
            .map(|(_, key, step)| (format!("{key} of job `{}`", job.key), step))
        })?,
    };
    Some(format!(
        "{what}, which the planner applies from M1 step {step}"
    ))
}

fn uses_an_action(job: &ir::JobTemplate) -> bool {
    let ir::JobKind::Steps(steps) = &job.kind;
    steps
        .iter()
        .any(|step| matches!(step.action, ir::Action::Uses(_)))
}

fn placement(runs_on: &Evaluable<ir::RunsOn>) -> Option<(Vec<String>, Option<String>)> {
    let Evaluable::Literal(runs_on) = runs_on else {
        return None;
    };
    let labels = runs_on
        .labels
        .iter()
        .map(Template::as_literal)
        .collect::<Option<Vec<_>>>()?;
    let group = match &runs_on.group {
        Some(group) => Some(group.as_literal()?),
        None => None,
    };
    Some((labels, group))
}

fn runs_on(workflow: &ir::Workflow, key: &JobKey) -> Result<(Vec<String>, Option<String>), Error> {
    let template = workflow
        .jobs
        .iter()
        .find(|template| &template.key == key)
        .ok_or_else(|| Error::Row {
            table: "run",
            message: format!("the IR of the run has no job `{key}`"),
        })?;
    placement(&template.runs_on).ok_or_else(|| Error::Row {
        table: "run",
        message: format!(
            "the IR of the run places job `{key}` by an expression in `runs-on`, which the \
             planner evaluates from M1 step 15"
        ),
    })
}

async fn settle(run: &mut Locked<'_>, jobs: &[JobState]) -> Result<(), Error> {
    let (to, conclusion) = if jobs.iter().all(|job| job.status == JobStatus::Completed) {
        (
            RunStatus::Completed,
            Some(conclude(run.state().cancel_requested, jobs)),
        )
    } else if jobs.iter().any(|job| job.started) {
        (RunStatus::InProgress, None)
    } else {
        (RunStatus::Queued, None)
    };
    if (to, conclusion) != (run.state().status, run.state().conclusion) {
        run.set_status(to, conclusion).await?;
    }
    Ok(())
}

fn conclude(cancel_requested: bool, jobs: &[JobState]) -> RunConclusion {
    let any = |conclusion| jobs.iter().any(|job| job.conclusion == Some(conclusion));
    if any(Conclusion::Failure) || any(Conclusion::TimedOut) {
        RunConclusion::Failure
    } else if cancel_requested || any(Conclusion::Cancelled) {
        RunConclusion::Cancelled
    } else if jobs
        .iter()
        .all(|job| job.conclusion == Some(Conclusion::Skipped))
    {
        RunConclusion::Skipped
    } else {
        RunConclusion::Success
    }
}
