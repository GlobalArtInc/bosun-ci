use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use bosun_types::event::Event;
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::status::{
    Conclusion, ExecutionStatus, JobStatus, RunConclusion, RunStatus, StepStatus,
};
use proptest::bool::weighted;
use proptest::collection::vec;
use proptest::prelude::{Just, Strategy, any};
use proptest::test_runner::{Config, TestCaseError, TestRunner};

use super::{Projection, World, job_cancel, replay, run_cancel, template, workflow};
use crate::agents::progress::{self, Progress, StepEnd};
use crate::commands::Applied;
use crate::queue;

const MOST_JOBS: usize = 6;

const MOST_ACTIONS: usize = 400;

#[derive(Debug, Clone)]
struct Case {
    jobs: Vec<Planned>,
    order: Vec<usize>,
    commands: bool,
    choices: Vec<u32>,
}

#[derive(Debug, Clone)]
struct Planned {
    needs: Vec<usize>,
    steps: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Assign(JobId),
    Accept(ExecutionId),
    StartStep(ExecutionId, u32),
    FinishStep(ExecutionId, u32, Conclusion),
    FinishJob(ExecutionId, Conclusion),
    Lose(ExecutionId),
    Expire(ExecutionId),
    Reject(ExecutionId),
    TimeOut(JobId),
    CancelRun,
    CancelJob(JobId),
}

fn cases() -> impl Strategy<Value = Case> {
    (1..=MOST_JOBS)
        .prop_flat_map(|count| {
            let jobs: Vec<_> = (0..count)
                .map(|index| (vec(any::<bool>(), index), vec(weighted(0.9), 1..=3)))
                .collect();
            (
                jobs,
                Just((0..count).collect::<Vec<usize>>()).prop_shuffle(),
                any::<bool>(),
                vec(any::<u32>(), 0..64),
            )
        })
        .prop_map(|(jobs, order, commands, choices)| Case {
            jobs: jobs
                .into_iter()
                .map(|(needs, steps)| Planned {
                    needs: needs
                        .iter()
                        .enumerate()
                        .filter(|(_, needed)| **needed)
                        .map(|(index, _)| index)
                        .collect(),
                    steps,
                })
                .collect(),
            order,
            commands,
            choices,
        })
}

fn key(index: usize) -> String {
    format!("j{index}")
}

#[test]
fn projected_statuses_always_equal_a_replay_of_the_journal() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let world = runtime.block_on(World::new());
    let mut runner = TestRunner::new(Config {
        cases: 128,
        failure_persistence: None,
        ..Config::default()
    });
    let result = runner.run(&cases(), |case| runtime.block_on(drive(&world, &case)));
    runtime.block_on(async move { drop(world) });
    if let Err(failure) = result {
        panic!("{failure}");
    }
}

async fn drive(world: &World, case: &Case) -> Result<(), TestCaseError> {
    let templates = case
        .order
        .iter()
        .map(|&index| {
            let planned = &case.jobs[index];
            let needs: Vec<String> = planned.needs.iter().map(|&need| key(need)).collect();
            let needs: Vec<&str> = needs.iter().map(String::as_str).collect();
            template(&key(index), &needs, planned.steps.len())
        })
        .collect();
    let run = world.dispatch(&workflow(templates)).await;
    let mut choices = case.choices.iter().copied();
    let mut cancelled_at = None;
    let mut settled = false;
    for _ in 0..MOST_ACTIONS {
        let projection = consistent(world, run).await?;
        let controls = world.controls().await;
        let (progress, extra) = actions(case, &projection, &controls);
        let Some(&first) = progress.first() else {
            settled = true;
            break;
        };
        let action = match choices.next() {
            Some(choice) if case.commands && !extra.is_empty() && choice % 5 == 0 => {
                extra[(choice as usize / 5) % extra.len()]
            }
            Some(choice) => progress[(choice as usize / 5) % progress.len()],
            None => first,
        };
        let accepted_cancel = perform(world, run, action).await?;
        if accepted_cancel && cancelled_at.is_none() {
            cancelled_at = world.journal(run).await.last().map(|event| event.seq);
            right_after_cancel(world, run).await?;
        }
    }

    if !settled {
        return Err(TestCaseError::fail(format!(
            "the run was still going after {MOST_ACTIONS} actions"
        )));
    }
    let projection = consistent(world, run).await?;
    let run_row = projection.run.clone().expect("the run");
    if run_row.status != RunStatus::Completed {
        return Err(TestCaseError::fail(format!(
            "the run is stuck without anything left to do: {projection:#?}"
        )));
    }
    let open: Vec<String> = projection
        .jobs
        .values()
        .filter(|job| job.status != JobStatus::Completed)
        .map(|job| job.key.clone())
        .chain(
            projection
                .executions
                .values()
                .filter(|execution| execution.status != ExecutionStatus::Completed)
                .map(|execution| execution.job.to_string()),
        )
        .chain(
            projection
                .steps
                .values()
                .filter(|step| step.status != StepStatus::Completed)
                .map(|step| step.name.clone()),
        )
        .collect();
    if !open.is_empty() {
        return Err(TestCaseError::fail(format!(
            "a completed run left {open:?} outside completed"
        )));
    }
    let expected = conclusion(&projection);
    if run_row.conclusion != Some(expected) {
        return Err(TestCaseError::fail(format!(
            "the run concluded {:?}, and its jobs say {expected:?}: {projection:#?}",
            run_row.conclusion
        )));
    }
    dependencies_respected(case, &projection)?;
    if let Some(cancelled_at) = cancelled_at {
        nothing_started_after(world, run, cancelled_at).await?;
    }

    match world.command(run, &run_cancel()).await {
        Applied::Refused(_) => {}
        Applied::Accepted(event) => {
            return Err(TestCaseError::fail(format!(
                "a completed run accepted run.cancel: {event:?}"
            )));
        }
    }
    let after = consistent(world, run).await?;
    if after != projection {
        return Err(TestCaseError::fail(
            "a refused command changed the run".to_owned(),
        ));
    }
    Ok(())
}

async fn consistent(world: &World, run: RunId) -> Result<Projection, TestCaseError> {
    let projection = world.project(run).await;
    let replayed = replay(&world.journal(run).await).map_err(TestCaseError::fail)?;
    if replayed != projection {
        return Err(TestCaseError::fail(format!(
            "the status columns are {projection:#?}, and a replay of the journal gives \
             {replayed:#?}"
        )));
    }
    Ok(projection)
}

fn actions(
    case: &Case,
    projection: &Projection,
    controls: &BTreeSet<ExecutionId>,
) -> (Vec<Action>, Vec<Action>) {
    let mut progress = Vec::new();
    let mut extra = vec![Action::CancelRun];
    for (&job, row) in &projection.jobs {
        extra.push(Action::CancelJob(job));
        if row.status == JobStatus::Queued {
            progress.push(Action::Assign(job));
            extra.push(Action::TimeOut(job));
        }
    }
    for (&execution, row) in &projection.executions {
        match row.status {
            ExecutionStatus::Assigned => {
                progress.push(Action::Accept(execution));
                extra.push(Action::Expire(execution));
                extra.push(Action::Reject(execution));
            }
            ExecutionStatus::InProgress => {
                let results = &case.jobs[index_of(&projection.jobs[&row.job].key)].steps;
                progress.push(next_report(projection, execution, results));
                if controls.contains(&execution) {
                    progress.push(Action::FinishJob(execution, Conclusion::Cancelled));
                }
                extra.push(Action::Lose(execution));
            }
            _ => {}
        }
    }
    (progress, extra)
}

fn next_report(projection: &Projection, execution: ExecutionId, results: &[bool]) -> Action {
    let steps = projection.steps_of(execution);
    if steps
        .iter()
        .any(|step| step.conclusion == Some(Conclusion::Failure))
    {
        return Action::FinishJob(execution, Conclusion::Failure);
    }
    match steps
        .iter()
        .find(|step| step.status != StepStatus::Completed)
    {
        Some(step) if step.status == StepStatus::InProgress => {
            let result = if results[step.index as usize] {
                Conclusion::Success
            } else {
                Conclusion::Failure
            };
            Action::FinishStep(execution, step.index, result)
        }
        Some(step) => Action::StartStep(execution, step.index),
        None => Action::FinishJob(execution, Conclusion::Success),
    }
}

fn index_of(key: &str) -> usize {
    key.trim_start_matches('j')
        .parse()
        .unwrap_or_else(|_| panic!("`{key}` is not a key of the case"))
}

async fn perform(world: &World, run: RunId, action: Action) -> Result<bool, TestCaseError> {
    let db = world.db();
    let agent = world.agent();
    let reported = match action {
        Action::Assign(job) => {
            world.assign(run, job).await;
            return Ok(false);
        }
        Action::CancelRun => {
            let applied = world.command(run, &run_cancel()).await;
            return Ok(matches!(applied, Applied::Accepted(_)));
        }
        Action::CancelJob(job) => {
            world.command(run, &job_cancel(job)).await;
            return Ok(false);
        }
        Action::Lose(execution) => {
            progress::lose_forgotten(db, agent.id, execution)
                .await
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            return Ok(false);
        }
        Action::Expire(execution) => {
            let returned = queue::expire(db, execution)
                .await
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            if returned.is_none() {
                return Err(TestCaseError::fail(format!(
                    "the lapsed assignment {execution} stayed assigned"
                )));
            }
            return Ok(false);
        }
        Action::TimeOut(job) => {
            let cancelled = queue::cancel_unclaimed(db, job, Duration::ZERO)
                .await
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            if !cancelled {
                return Err(TestCaseError::fail(format!(
                    "the queued job {job} outlived max_queue_time"
                )));
            }
            return Ok(false);
        }
        Action::Reject(execution) => progress::rejected(db, agent, execution).await,
        Action::Accept(execution) => progress::accepted(db, agent, execution).await,
        Action::StartStep(execution, index) => {
            progress::step_started(db, agent, execution, index, None).await
        }
        Action::FinishStep(execution, index, result) => {
            progress::step_finished(
                db,
                agent,
                execution,
                StepEnd {
                    index,
                    at: None,
                    outcome: result,
                    conclusion: result,
                },
            )
            .await
        }
        Action::FinishJob(execution, result) => {
            progress::job_finished(db, agent, execution, result).await
        }
    };
    match reported.map_err(|error| TestCaseError::fail(error.to_string()))? {
        Progress::Applied => Ok(false),
        Progress::Ignored(reason) => Err(TestCaseError::fail(format!(
            "{action:?} was ignored: {reason}"
        ))),
    }
}

async fn right_after_cancel(world: &World, run: RunId) -> Result<(), TestCaseError> {
    let projection = world.project(run).await;
    let controls = world.controls().await;
    for (&job, row) in &projection.jobs {
        let told = projection
            .executions_of(job)
            .iter()
            .any(|(execution, current)| {
                current.status == ExecutionStatus::InProgress && controls.contains(execution)
            });
        if row.status != JobStatus::Completed && !(row.status == JobStatus::InProgress && told) {
            return Err(TestCaseError::fail(format!(
                "run.cancel left job `{}` {:?} without stopping it",
                row.key, row.status
            )));
        }
    }
    Ok(())
}

async fn nothing_started_after(
    world: &World,
    run: RunId,
    cancelled_at: u64,
) -> Result<(), TestCaseError> {
    for event in world.journal(run).await {
        if event.seq <= cancelled_at {
            continue;
        }
        if let Event::JobStatusChanged(changed) = &event.event
            && matches!(
                changed.to,
                JobStatus::Queued | JobStatus::Assigned | JobStatus::InProgress
            )
        {
            return Err(TestCaseError::fail(format!(
                "a job went to {:?} after run.cancel",
                changed.to
            )));
        }
    }
    Ok(())
}

fn conclusion(projection: &Projection) -> RunConclusion {
    let concluded: Vec<Option<Conclusion>> =
        projection.jobs.values().map(|job| job.conclusion).collect();
    let cancel_requested = projection
        .run
        .as_ref()
        .is_some_and(|run| run.cancel_requested);
    if concluded
        .iter()
        .any(|conclusion| matches!(conclusion, Some(Conclusion::Failure | Conclusion::TimedOut)))
    {
        RunConclusion::Failure
    } else if cancel_requested || concluded.contains(&Some(Conclusion::Cancelled)) {
        RunConclusion::Cancelled
    } else if concluded
        .iter()
        .all(|conclusion| *conclusion == Some(Conclusion::Skipped))
    {
        RunConclusion::Skipped
    } else {
        RunConclusion::Success
    }
}

fn dependencies_respected(case: &Case, projection: &Projection) -> Result<(), TestCaseError> {
    let concluded: HashMap<usize, Option<Conclusion>> = projection
        .jobs
        .values()
        .map(|job| (index_of(&job.key), job.conclusion))
        .collect();
    for (&id, job) in &projection.jobs {
        let needs = &case.jobs[index_of(&job.key)].needs;
        let all_succeeded = needs
            .iter()
            .all(|need| concluded.get(need) == Some(&Some(Conclusion::Success)));
        let ran = !projection.executions_of(id).is_empty();
        if ran && !all_succeeded {
            return Err(TestCaseError::fail(format!(
                "job `{}` ran although a job it needs did not succeed",
                job.key
            )));
        }
        if job.conclusion == Some(Conclusion::Skipped) && all_succeeded {
            return Err(TestCaseError::fail(format!(
                "job `{}` was skipped although every job it needs succeeded",
                job.key
            )));
        }
    }
    Ok(())
}
