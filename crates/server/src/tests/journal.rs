use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use bosun_types::command::Command;
use bosun_types::event::{Event, JobStatusChanged};
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::ir::{Evaluable, JobTemplate};
use bosun_types::status::{
    Conclusion, ExecutionStatus, JobStatus, RunConclusion, RunStatus, StatusReason, StepStatus,
};
use proptest::bool::weighted;
use proptest::collection::vec;
use proptest::prelude::{Just, Strategy, any, prop_oneof};
use proptest::test_runner::{Config, TestCaseError, TestRunner};

use super::{Projection, World, finished, job_cancel, replay, run_cancel, template, workflow};
use crate::agents::progress::{self, Progress, StepEnd};
use crate::commands::Applied;
use crate::queue;
use crate::site::Site;

const MOST_JOBS: usize = 6;

const MOST_ACTIONS: usize = 400;

#[derive(Debug, Clone)]
struct Case {
    jobs: Vec<Planned>,
    order: Vec<usize>,
    commands: bool,
    cancel_after: Option<usize>,
    choices: Vec<u32>,
}

#[derive(Debug, Clone)]
struct Planned {
    needs: Vec<usize>,
    steps: Vec<bool>,
    condition: Condition,
    continue_on_error: bool,
    computed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    Implicit,
    Success,
    Failure,
    Always,
    Cancelled,
    NotCancelled,
    FailureOrCancelled,
    NotSuccess,
    Holds,
    Fails,
}

impl Condition {
    fn text(self) -> Option<&'static str> {
        match self {
            Condition::Implicit => None,
            Condition::Success => Some("success()"),
            Condition::Failure => Some("${{ failure() }}"),
            Condition::Always => Some("always()"),
            Condition::Cancelled => Some("cancelled()"),
            Condition::NotCancelled => Some("!cancelled()"),
            Condition::FailureOrCancelled => Some("failure() || cancelled()"),
            Condition::NotSuccess => Some("${{ !success() }}"),
            Condition::Holds => Some("github.event_name == 'workflow_dispatch'"),
            Condition::Fails => Some("${{ github.event_name == 'push' }}"),
        }
    }

    fn calls_status(self) -> bool {
        !matches!(
            self,
            Condition::Implicit | Condition::Holds | Condition::Fails
        )
    }

    fn holds(self, succeeded: bool, failed: bool, cancelling: bool) -> bool {
        let success = !cancelling && succeeded;
        let failure = !cancelling && failed;
        match self {
            Condition::Implicit | Condition::Success | Condition::Holds => success,
            Condition::Failure => failure,
            Condition::Always => true,
            Condition::Cancelled => cancelling,
            Condition::NotCancelled => !cancelling,
            Condition::FailureOrCancelled => failure || cancelling,
            Condition::NotSuccess => !success,
            Condition::Fails => false,
        }
    }
}

fn conditions() -> impl Strategy<Value = Condition> {
    prop_oneof![
        4 => Just(Condition::Implicit),
        1 => Just(Condition::Success),
        3 => Just(Condition::Failure),
        2 => Just(Condition::Always),
        1 => Just(Condition::Cancelled),
        1 => Just(Condition::NotCancelled),
        1 => Just(Condition::FailureOrCancelled),
        1 => Just(Condition::NotSuccess),
        1 => Just(Condition::Holds),
        1 => Just(Condition::Fails),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
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
                .map(|index| {
                    (
                        vec(any::<bool>(), index),
                        vec(weighted(0.75), 1..=3),
                        conditions(),
                        weighted(0.3),
                        any::<bool>(),
                    )
                })
                .collect();
            (
                jobs,
                Just((0..count).collect::<Vec<usize>>()).prop_shuffle(),
                any::<bool>(),
                proptest::option::of(0..16_usize),
                vec(any::<u32>(), 0..64),
            )
        })
        .prop_map(|(jobs, order, commands, cancel_after, choices)| Case {
            jobs: jobs
                .into_iter()
                .map(
                    |(needs, steps, condition, continue_on_error, computed)| Planned {
                        needs: needs
                            .iter()
                            .enumerate()
                            .filter(|(_, needed)| **needed)
                            .map(|(index, _)| index)
                            .collect(),
                        steps,
                        condition,
                        continue_on_error,
                        computed,
                    },
                )
                .collect(),
            order,
            commands,
            cancel_after,
            choices,
        })
}

fn key(index: usize) -> String {
    format!("j{index}")
}

fn planned_template(index: usize, planned: &Planned) -> JobTemplate {
    let needs: Vec<String> = planned.needs.iter().map(|&need| key(need)).collect();
    let needs: Vec<&str> = needs.iter().map(String::as_str).collect();
    let mut job = template(&key(index), &needs, planned.steps.len());
    job.r#if = planned.condition.text().map(|text| {
        bosun_expr::condition(text)
            .expect("a condition that parses")
            .expect("a condition")
    });
    job.continue_on_error = if planned.computed {
        let event = if planned.continue_on_error {
            "workflow_dispatch"
        } else {
            "push"
        };
        Evaluable::Template(
            bosun_expr::template(&format!("${{{{ github.event_name == '{event}' }}}}"))
                .expect("a template"),
        )
    } else {
        Evaluable::Literal(planned.continue_on_error)
    };
    job
}

#[test]
fn projected_statuses_always_equal_a_replay_of_the_journal() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let world = runtime.block_on(World::new());
    let mut runner = TestRunner::new(Config {
        cases: 192,
        failure_persistence: None,
        ..Config::default()
    });
    let result = runner.run(&cases(), |case| runtime.block_on(drive(&world, &case)));
    runtime.block_on(async move { drop(world) });
    if let Err(failure) = result {
        panic!("{failure}");
    }
}

#[tokio::test]
async fn a_job_is_decided_by_its_ancestors_at_the_moment_its_needs_complete()
-> Result<(), TestCaseError> {
    let world = World::new().await;
    let planned = |needs: &[usize], condition| Planned {
        needs: needs.to_vec(),
        steps: vec![false],
        condition,
        continue_on_error: false,
        computed: false,
    };
    let case = Case {
        jobs: vec![
            planned(&[], Condition::Implicit),
            planned(&[0], Condition::Implicit),
            planned(&[1], Condition::Failure),
        ],
        order: vec![0, 1, 2],
        commands: true,
        cancel_after: None,
        choices: Vec::new(),
    };
    let templates = case
        .order
        .iter()
        .map(|&index| planned_template(index, &case.jobs[index]))
        .collect();
    let run = world.dispatch(&workflow(templates)).await;
    let projection = consistent(&world, run).await?;
    let (first, _) = projection.job("j0");
    let (second, _) = projection.job("j1");
    let execution = world.assign(run, first).await;
    perform(&world, run, Action::Accept(execution)).await?;
    perform(&world, run, Action::CancelJob(second)).await?;

    let projection = consistent(&world, run).await?;
    assert_eq!(projection.job("j0").1.status, JobStatus::InProgress);
    assert_eq!(
        projection.job("j1").1.conclusion,
        Some(Conclusion::Cancelled)
    );
    assert_eq!(projection.job("j2").1.conclusion, Some(Conclusion::Skipped));

    for action in [
        Action::StartStep(execution, 0),
        Action::FinishStep(execution, 0, Conclusion::Failure),
        Action::FinishJob(execution, Conclusion::Failure),
    ] {
        perform(&world, run, action).await?;
    }
    let projection = consistent(&world, run).await?;
    assert_eq!(
        projection.run.as_ref().and_then(|run| run.conclusion),
        Some(RunConclusion::Failure)
    );
    conditions_decided(&world, &case, run, &projection, None).await
}

async fn drive(world: &World, case: &Case) -> Result<(), TestCaseError> {
    let templates = case
        .order
        .iter()
        .map(|&index| planned_template(index, &case.jobs[index]))
        .collect();
    let run = world.dispatch(&workflow(templates)).await;
    let mut choices = case.choices.iter().copied();
    let mut cancelled_at = None;
    let mut settled = false;
    for performed in 0..MOST_ACTIONS {
        let projection = consistent(world, run).await?;
        let controls = world.controls().await;
        let (progress, extra) = actions(case, &projection, &controls);
        let Some(&first) = progress.first() else {
            settled = true;
            break;
        };
        let action = match choices.next() {
            _ if case.cancel_after == Some(performed) => Action::CancelRun,
            Some(choice) if case.commands && !extra.is_empty() && choice % 5 == 0 => {
                extra[(choice as usize / 5) % extra.len()]
            }
            Some(choice) => progress[(choice as usize / 5) % progress.len()],
            None => first,
        };
        let accepted_cancel = perform(world, run, action).await?;
        if accepted_cancel && cancelled_at.is_none() {
            cancelled_at =
                world
                    .journal(run)
                    .await
                    .iter()
                    .rev()
                    .find_map(|event| match &event.event {
                        Event::CommandAccepted(accepted)
                            if accepted.command.command == Command::RunCancel =>
                        {
                            Some(event.seq)
                        }
                        _ => None,
                    });
            right_after_cancel(world, case, run).await?;
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
    let expected = conclusion(case, &projection);
    if run_row.conclusion != Some(expected) {
        return Err(TestCaseError::fail(format!(
            "the run concluded {:?}, and its jobs say {expected:?}: {projection:#?}",
            run_row.conclusion
        )));
    }
    conditions_decided(world, case, run, &projection, cancelled_at).await?;
    if let Some(cancelled_at) = cancelled_at {
        only_admitted_jobs_started_after(world, case, run, cancelled_at).await?;
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

pub(super) async fn consistent(world: &World, run: RunId) -> Result<Projection, TestCaseError> {
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

pub(super) fn next_report(
    projection: &Projection,
    execution: ExecutionId,
    results: &[bool],
) -> Action {
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

pub(super) async fn perform(
    world: &World,
    run: RunId,
    action: Action,
) -> Result<bool, TestCaseError> {
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
            progress::lose_forgotten(db, agent.id, execution, &Site::default())
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
            let cancelled = queue::cancel_unclaimed(db, job, Duration::ZERO, &Site::default())
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
        Action::Accept(execution) => {
            progress::accepted(db, agent, execution, &Site::default()).await
        }
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
                    outputs: BTreeMap::new(),
                },
            )
            .await
        }
        Action::FinishJob(execution, result) => {
            progress::job_finished(db, agent, execution, finished(result), &Site::default()).await
        }
    };
    match reported.map_err(|error| TestCaseError::fail(error.to_string()))? {
        Progress::Applied => Ok(false),
        Progress::Ignored(reason) => Err(TestCaseError::fail(format!(
            "{action:?} was ignored: {reason}"
        ))),
    }
}

async fn right_after_cancel(world: &World, case: &Case, run: RunId) -> Result<(), TestCaseError> {
    let projection = world.project(run).await;
    let controls = world.controls().await;
    for (&job, row) in &projection.jobs {
        let told = projection
            .executions_of(job)
            .iter()
            .any(|(execution, current)| {
                current.status == ExecutionStatus::InProgress && controls.contains(execution)
            });
        let admits = case.jobs[index_of(&row.key)].condition.calls_status();
        let kept = admits && matches!(row.status, JobStatus::Pending | JobStatus::Queued);
        if row.status != JobStatus::Completed
            && !(row.status == JobStatus::InProgress && told)
            && !kept
        {
            return Err(TestCaseError::fail(format!(
                "run.cancel left job `{}` {:?} without stopping it",
                row.key, row.status
            )));
        }
    }
    Ok(())
}

async fn only_admitted_jobs_started_after(
    world: &World,
    case: &Case,
    run: RunId,
    cancelled_at: u64,
) -> Result<(), TestCaseError> {
    let projection = world.project(run).await;
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
            let job = event.job_id.and_then(|job| projection.jobs.get(&job));
            let admits =
                job.is_some_and(|job| case.jobs[index_of(&job.key)].condition.calls_status());
            if !admits {
                return Err(TestCaseError::fail(format!(
                    "a job whose `if` calls no status function went to {:?} after run.cancel",
                    changed.to
                )));
            }
        }
    }
    Ok(())
}

fn effective(case: &Case, key: &str, conclusion: Option<Conclusion>) -> Option<Conclusion> {
    match conclusion {
        Some(Conclusion::Failure | Conclusion::TimedOut)
            if case.jobs[index_of(key)].continue_on_error =>
        {
            Some(Conclusion::Success)
        }
        conclusion => conclusion,
    }
}

fn conclusion(case: &Case, projection: &Projection) -> RunConclusion {
    let concluded: Vec<Option<Conclusion>> =
        projection.jobs.values().map(|job| job.conclusion).collect();
    let cancel_requested = projection
        .run
        .as_ref()
        .is_some_and(|run| run.cancel_requested);
    if projection.jobs.values().any(|job| {
        matches!(
            effective(case, &job.key, job.conclusion),
            Some(Conclusion::Failure | Conclusion::TimedOut)
        )
    }) {
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

fn ancestors(case: &Case, index: usize) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    let mut open = case.jobs[index].needs.clone();
    while let Some(need) = open.pop() {
        if found.insert(need) {
            open.extend(case.jobs[need].needs.iter().copied());
        }
    }
    found
}

async fn conditions_decided(
    world: &World,
    case: &Case,
    run: RunId,
    projection: &Projection,
    cancelled_at: Option<u64>,
) -> Result<(), TestCaseError> {
    let mut concluded: HashMap<usize, Conclusion> = HashMap::new();
    let mut targeted = HashSet::new();
    let mut decided = HashSet::new();
    for event in world.journal(run).await {
        match &event.event {
            Event::CommandAccepted(accepted) if accepted.command.command == Command::JobCancel => {
                targeted.extend(accepted.command.target.job);
            }
            Event::JobStatusChanged(changed) => {
                let job = event
                    .job_id
                    .ok_or_else(|| TestCaseError::fail("a job.status without a job"))?;
                let key = &projection.jobs[&job].key;
                let index = index_of(key);
                if changed.from == JobStatus::Pending && decided.insert(job) {
                    let decision = Decision {
                        key,
                        index,
                        targeted: targeted.contains(&job),
                        cancelling: cancelled_at.is_some_and(|cancelled| cancelled < event.seq),
                    };
                    decision.check(case, &concluded, changed)?;
                }
                if changed.to == JobStatus::Completed
                    && let Some(conclusion) = effective(case, key, changed.conclusion)
                {
                    concluded.insert(index, conclusion);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

struct Decision<'a> {
    key: &'a str,
    index: usize,
    targeted: bool,
    cancelling: bool,
}

impl Decision<'_> {
    fn check(
        &self,
        case: &Case,
        concluded: &HashMap<usize, Conclusion>,
        changed: &JobStatusChanged,
    ) -> Result<(), TestCaseError> {
        let planned = &case.jobs[self.index];
        let lineage = ancestors(case, self.index);
        let succeeded = lineage
            .iter()
            .all(|ancestor| concluded.get(ancestor) == Some(&Conclusion::Success));
        let failed = lineage
            .iter()
            .any(|ancestor| concluded.get(ancestor) == Some(&Conclusion::Failure));
        let cancelling = self.cancelling;
        let expected = planned.condition.holds(succeeded, failed, cancelling);
        let cancelled = cancelling && (!planned.condition.calls_status() || !expected);
        match (changed.to, changed.reason) {
            (JobStatus::Queued, None) if expected => Ok(()),
            (JobStatus::Completed, Some(StatusReason::IfFalse)) if !expected && !cancelling => {
                Ok(())
            }
            (JobStatus::Completed, Some(StatusReason::Command)) if self.targeted || cancelled => {
                Ok(())
            }
            (to, reason) => Err(TestCaseError::fail(format!(
                "job `{}` with {:?} after ancestors that succeeded: {succeeded}, failed: \
                 {failed}, while cancelling: {cancelling}, went to {to:?} with {reason:?}",
                self.key, planned.condition
            ))),
        }
    }
}
