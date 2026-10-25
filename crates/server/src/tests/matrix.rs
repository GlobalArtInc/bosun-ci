use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use bosun_db::entity::job;
use bosun_types::diagnostic::{PathSegment, Severity};
use bosun_types::event::Event;
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::ir;
use bosun_types::status::{
    Conclusion, ExecutionStatus, JobStatus, RunConclusion, RunStatus, StatusReason, WaitingReason,
};
use proptest::bool::weighted;
use proptest::collection::vec;
use proptest::prelude::{Strategy, any};
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::Deserialize;
use serde_json::{Value, json};

use super::journal::{Action, consistent, next_report, perform};
use super::{Projection, World, finished, job_cancel};
use crate::agents::progress::JobEnd;
use crate::commands::Applied;

fn parse(source: &str) -> ir::Workflow {
    bosun_workflow::parse("ci", source)
        .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
        .workflow
}

async fn jobs(world: &World, run: RunId) -> Vec<job::Model> {
    let attempts: Vec<_> = bosun_db::entity::attempt::Entity::find()
        .filter(bosun_db::entity::attempt::Column::RunId.eq(run.as_uuid()))
        .all(world.db())
        .await
        .expect("the attempts")
        .into_iter()
        .map(|attempt| attempt.id)
        .collect();
    job::Entity::find()
        .filter(job::Column::AttemptId.is_in(attempts))
        .order_by_asc(job::Column::Id)
        .all(world.db())
        .await
        .expect("the jobs")
}

fn group<'a>(jobs: &'a [job::Model], key: &str) -> &'a job::Model {
    jobs.iter()
        .find(|job| job.key == key && job.group_id.is_none())
        .unwrap_or_else(|| panic!("no job `{key}` in {jobs:#?}"))
}

fn members<'a>(jobs: &'a [job::Model], group: &job::Model) -> Vec<&'a job::Model> {
    jobs.iter()
        .filter(|job| job.group_id == Some(group.id))
        .collect()
}

fn id(job: &job::Model) -> JobId {
    JobId::try_from(job.id).expect("a version 7 id")
}

fn status(job: &job::Model) -> (JobStatus, Option<Conclusion>) {
    (
        super::name(&job.status),
        job.conclusion.as_deref().map(super::name),
    )
}

async fn reason(world: &World, run: RunId, job: JobId) -> Option<StatusReason> {
    world
        .journal(run)
        .await
        .into_iter()
        .filter(|event| event.job_id == Some(job))
        .find_map(|event| match event.event {
            Event::JobStatusChanged(changed) if changed.to == JobStatus::Completed => {
                changed.reason
            }
            _ => None,
        })
}

async fn concluded(world: &World, run: RunId) -> (RunStatus, Option<RunConclusion>) {
    let shown = world.run(run).await;
    (shown.status, shown.conclusion)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    description: String,
    name: Option<String>,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
    strategy: bosun_expr::Value,
    jobs: Option<Vec<Member>>,
    error: Option<Refusal>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    name: String,
    matrix: Option<Value>,
    strategy: Value,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    code: String,
    message: String,
    path: Vec<String>,
}

fn cases() -> Vec<(PathBuf, Case)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/matrix");
    let mut files: Vec<PathBuf> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            let case = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            (file, case)
        })
        .collect()
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("a JSON string")
}

fn flow(value: &bosun_expr::Value) -> String {
    match value {
        bosun_expr::Value::Null => "null".to_owned(),
        bosun_expr::Value::Boolean(flag) => flag.to_string(),
        bosun_expr::Value::Number(number) => number.to_string(),
        bosun_expr::Value::String(text) => quoted(text),
        bosun_expr::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(flow).collect();
            format!("[{}]", items.join(", "))
        }
        bosun_expr::Value::Object(fields) => {
            let fields: Vec<String> = fields
                .iter()
                .map(|(name, value)| format!("{}: {}", quoted(name), flow(value)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
    }
}

fn source(case: &Case) -> String {
    let mut source = "on: workflow_dispatch\njobs:\n".to_owned();
    if !case.outputs.is_empty() {
        source.push_str("  setup:\n    runs-on: self-hosted\n    outputs:\n");
        for name in case.outputs.keys() {
            source.push_str(&format!(
                "      {name}: ${{{{ steps.list.outputs.{name} }}}}\n"
            ));
        }
        source.push_str("    steps:\n      - id: list\n        run: make\n");
    }
    source.push_str("  test:\n");
    if !case.outputs.is_empty() {
        source.push_str("    needs: setup\n");
    }
    if let Some(name) = &case.name {
        source.push_str(&format!("    name: {}\n", quoted(name)));
    }
    source.push_str(&format!(
        "    runs-on: self-hosted\n    strategy: {}\n    steps:\n      - run: make\n",
        flow(&case.strategy)
    ));
    source
}

#[tokio::test]
async fn the_expansion_cases_pass() {
    let world = World::new().await;
    let cases = cases();
    assert!(!cases.is_empty(), "contracts/fixtures/matrix holds no case");
    for (file, case) in cases {
        let file = file.display();
        let run = world.dispatch(&parse(&source(&case))).await;
        if !case.outputs.is_empty() {
            world
                .finish(
                    run,
                    "setup",
                    JobEnd {
                        conclusion: Conclusion::Success,
                        outputs: case.outputs.clone(),
                    },
                )
                .await;
        }
        let stored = jobs(&world, run).await;
        let template = group(&stored, "test");
        let created = members(&stored, template);
        match (&case.jobs, &case.error) {
            (Some(expected), None) => {
                let rows = if created.is_empty() {
                    vec![template]
                } else {
                    created
                };
                let shown: Vec<Member> = rows
                    .into_iter()
                    .map(|row| Member {
                        name: row.name.clone(),
                        matrix: row.matrix.clone(),
                        strategy: row.strategy.clone().unwrap_or(Value::Null),
                    })
                    .collect();
                assert_eq!(&shown, expected, "{file}: {}", case.description);
            }
            (None, Some(refusal)) => {
                assert!(created.is_empty(), "{file}: {created:#?}");
                assert_eq!(
                    status(template),
                    (JobStatus::Completed, Some(Conclusion::Failure)),
                    "{file}"
                );
                assert_eq!(
                    reason(&world, run, id(template)).await,
                    Some(StatusReason::EvaluationFailed),
                    "{file}"
                );
                let shown = world.run(run).await;
                assert_eq!(
                    (shown.status, shown.conclusion),
                    (RunStatus::Completed, Some(RunConclusion::Failure)),
                    "{file}"
                );
                let [notice] = shown.notices.as_slice() else {
                    panic!("{file}: the notices are {:#?}", shown.notices);
                };
                assert_eq!(notice.job_id, Some(id(template)), "{file}");
                assert_eq!(notice.diagnostic.severity, Severity::Error, "{file}");
                let path: Vec<String> = notice
                    .diagnostic
                    .path
                    .iter()
                    .map(|segment| match segment {
                        PathSegment::Key(key) => key.clone(),
                        PathSegment::Index(index) => index.to_string(),
                    })
                    .collect();
                assert_eq!(
                    Refusal {
                        code: notice.diagnostic.code.clone(),
                        message: notice.diagnostic.message.clone(),
                        path,
                    },
                    *refusal,
                    "{file}: {}",
                    case.description
                );
            }
            _ => panic!("{file}: a case holds either `jobs` or an `error`"),
        }
    }
}

fn shards(fail_fast: bool, tolerated: &str) -> String {
    format!(
        "on: workflow_dispatch\njobs:\n  test:\n    runs-on: self-hosted\n    \
         continue-on-error: ${{{{ contains(fromJSON('{tolerated}'), matrix.shard) }}}}\n    \
         strategy:\n      fail-fast: {fail_fast}\n      matrix:\n        shard: [0, 1, 2, 3]\n    \
         steps:\n      - run: make\n  after:\n    needs: test\n    \
         if: always() && needs.test.result == 'failure'\n    runs-on: self-hosted\n    \
         steps:\n      - run: ./after\n"
    )
}

#[tokio::test]
async fn under_fail_fast_a_failing_member_cancels_the_queued_and_running_members() {
    let world = World::new().await;
    let run = world.dispatch(&parse(&shards(true, "[]"))).await;
    let stored = jobs(&world, run).await;
    let shards: Vec<JobId> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(id)
        .collect();
    assert_eq!(shards.len(), 4);
    let failing = world.start(run, shards[0]).await;
    let running = world.start(run, shards[1]).await;
    let assigned = world.assign(run, shards[2]).await;
    world.end(failing, finished(Conclusion::Failure)).await;

    let stored = jobs(&world, run).await;
    let rows: Vec<_> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(status)
        .collect();
    let cancelled = (JobStatus::Completed, Some(Conclusion::Cancelled));
    assert_eq!(
        rows,
        [
            (JobStatus::Completed, Some(Conclusion::Failure)),
            (JobStatus::InProgress, None),
            cancelled,
            cancelled,
        ]
    );
    for shard in &shards[2..] {
        assert_eq!(
            reason(&world, run, *shard).await,
            Some(StatusReason::FailFast)
        );
    }
    assert_eq!(world.controls().await, BTreeSet::from([running, assigned]));
    assert_eq!(status(group(&stored, "test")), (JobStatus::Pending, None));

    world.end(running, finished(Conclusion::Cancelled)).await;
    let stored = jobs(&world, run).await;
    assert_eq!(
        status(group(&stored, "test")),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    assert_eq!(status(group(&stored, "after")), (JobStatus::Queued, None));
    world
        .finish(run, "after", finished(Conclusion::Success))
        .await;
    assert_eq!(
        concluded(&world, run).await,
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}

#[tokio::test]
async fn without_fail_fast_every_member_finishes() {
    let world = World::new().await;
    let run = world.dispatch(&parse(&shards(false, "[]"))).await;
    let stored = jobs(&world, run).await;
    let shards: Vec<JobId> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(id)
        .collect();
    let executions: Vec<ExecutionId> = {
        let mut started = Vec::new();
        for shard in &shards {
            started.push(world.start(run, *shard).await);
        }
        started
    };
    world
        .end(executions[1], finished(Conclusion::Failure))
        .await;
    assert!(world.controls().await.is_empty());
    for execution in [executions[0], executions[2], executions[3]] {
        world.end(execution, finished(Conclusion::Success)).await;
    }
    let stored = jobs(&world, run).await;
    let rows: Vec<_> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(status)
        .collect();
    let success = (JobStatus::Completed, Some(Conclusion::Success));
    assert_eq!(
        rows,
        [
            success,
            (JobStatus::Completed, Some(Conclusion::Failure)),
            success,
            success
        ]
    );
    assert_eq!(
        status(group(&stored, "test")),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
}

#[tokio::test]
async fn a_member_failing_under_continue_on_error_leaves_the_group_successful() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  test:
    runs-on: self-hosted
    continue-on-error: ${{ matrix.experimental }}
    strategy:
      matrix:
        node: [20, 22]
        experimental: [false]
        include:
          - node: 23
            experimental: true
    steps:
      - run: make
  after:
    needs: test
    if: needs.test.result == 'success'
    runs-on: self-hosted
    steps:
      - run: ./after
";
    let run = world.dispatch(&parse(source)).await;
    let stored = jobs(&world, run).await;
    let created = members(&stored, group(&stored, "test"));
    let names: Vec<&str> = created.iter().map(|member| member.name.as_str()).collect();
    assert_eq!(
        names,
        ["test (20, false)", "test (22, false)", "test (23, true)"]
    );
    let shards: Vec<JobId> = created.into_iter().map(id).collect();
    world
        .finish_job(run, shards[2], finished(Conclusion::Failure))
        .await;
    let stored = jobs(&world, run).await;
    let rows: Vec<_> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(status)
        .collect();
    assert_eq!(
        rows,
        [
            (JobStatus::Queued, None),
            (JobStatus::Queued, None),
            (JobStatus::Completed, Some(Conclusion::Failure))
        ]
    );
    for shard in &shards[..2] {
        world
            .finish_job(run, *shard, finished(Conclusion::Success))
            .await;
    }
    let stored = jobs(&world, run).await;
    assert_eq!(
        status(group(&stored, "test")),
        (JobStatus::Completed, Some(Conclusion::Success))
    );
    assert_eq!(status(group(&stored, "after")), (JobStatus::Queued, None));
    world
        .finish(run, "after", finished(Conclusion::Success))
        .await;
    assert_eq!(
        concluded(&world, run).await,
        (RunStatus::Completed, Some(RunConclusion::Success))
    );
}

#[tokio::test]
async fn the_group_combines_the_outputs_of_its_members_for_its_dependents() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  test:
    runs-on: self-hosted
    outputs:
      first: ${{ steps.report.outputs.first }}
      second: ${{ steps.report.outputs.second }}
    strategy:
      matrix:
        shard: [1, 2]
    steps:
      - id: report
        run: make
  after:
    needs: test
    runs-on: ${{ needs.test.outputs.first }}-${{ needs.test.outputs.second }}
    steps:
      - run: ./after
";
    let run = world.dispatch(&parse(source)).await;
    let stored = jobs(&world, run).await;
    let shards: Vec<JobId> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(id)
        .collect();
    let reported = |first: &str, second: &str| JobEnd {
        conclusion: Conclusion::Success,
        outputs: BTreeMap::from([
            ("first".to_owned(), first.to_owned()),
            ("second".to_owned(), second.to_owned()),
        ]),
    };
    world.finish_job(run, shards[1], reported("", "b")).await;
    world.finish_job(run, shards[0], reported("a", "")).await;
    let stored = jobs(&world, run).await;
    assert_eq!(
        group(&stored, "test").outputs,
        json!({ "first": "a", "second": "b" })
    );
    let after = group(&stored, "after");
    assert_eq!(
        (status(after), after.labels.as_slice()),
        ((JobStatus::Queued, None), ["a-b".to_owned()].as_slice())
    );
}

#[tokio::test]
async fn a_member_whose_values_fail_to_evaluate_fails_and_fail_fast_cancels_the_others() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  test:
    runs-on: ${{ matrix.label }}
    strategy:
      matrix:
        label: [linux, '', macos]
    steps:
      - run: make
";
    let run = world.dispatch(&parse(source)).await;
    let stored = jobs(&world, run).await;
    let created = members(&stored, group(&stored, "test"));
    let rows: Vec<_> = created.iter().map(|member| status(member)).collect();
    let cancelled = (JobStatus::Completed, Some(Conclusion::Cancelled));
    assert_eq!(
        rows,
        [
            cancelled,
            (JobStatus::Completed, Some(Conclusion::Failure)),
            cancelled
        ]
    );
    assert_eq!(
        reason(&world, run, id(created[1])).await,
        Some(StatusReason::EvaluationFailed)
    );
    assert_eq!(
        reason(&world, run, id(created[2])).await,
        Some(StatusReason::FailFast)
    );
    let notices = world.run(run).await.notices;
    let [notice] = notices.as_slice() else {
        panic!("the notices are {notices:#?}");
    };
    assert_eq!(
        (notice.job_id, notice.diagnostic.code.as_str()),
        (Some(id(created[1])), "invalid_value")
    );
    assert_eq!(
        status(group(&stored, "test")),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    assert_eq!(
        concluded(&world, run).await,
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}

#[tokio::test]
async fn cancelling_a_group_cancels_its_members_and_the_group_follows_them() {
    let world = World::new().await;
    let run = world.dispatch(&parse(&shards(true, "[]"))).await;
    let stored = jobs(&world, run).await;
    let template = id(group(&stored, "test"));
    let shards: Vec<JobId> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(id)
        .collect();
    let running = world.start(run, shards[0]).await;
    let Applied::Accepted(_) = world.command(run, &job_cancel(template)).await else {
        panic!("job.cancel of the group was refused");
    };
    let stored = jobs(&world, run).await;
    let rows: Vec<_> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(status)
        .collect();
    let cancelled = (JobStatus::Completed, Some(Conclusion::Cancelled));
    assert_eq!(
        rows,
        [
            (JobStatus::InProgress, None),
            cancelled,
            cancelled,
            cancelled
        ]
    );
    assert_eq!(world.controls().await, BTreeSet::from([running]));
    assert_eq!(status(group(&stored, "test")), (JobStatus::Pending, None));
    world.end(running, finished(Conclusion::Cancelled)).await;
    let stored = jobs(&world, run).await;
    assert_eq!(status(group(&stored, "test")), cancelled);
    assert_eq!(
        status(group(&stored, "after")),
        (JobStatus::Completed, Some(Conclusion::Skipped))
    );
    assert_eq!(
        concluded(&world, run).await,
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
}

const MOST_MEMBERS: usize = 6;

const MOST_ACTIONS: usize = 300;

const MAX_PARALLEL: usize = 2;

#[derive(Debug, Clone)]
struct Drive {
    results: Vec<bool>,
    tolerated: Vec<bool>,
    fail_fast: bool,
    commands: bool,
    choices: Vec<u32>,
}

fn drives() -> impl Strategy<Value = Drive> {
    (1..=MOST_MEMBERS)
        .prop_flat_map(|count| {
            (
                vec(weighted(0.7), count),
                vec(weighted(0.25), count),
                any::<bool>(),
                any::<bool>(),
                vec(any::<u32>(), 0..96),
            )
        })
        .prop_map(|(results, tolerated, fail_fast, commands, choices)| Drive {
            results,
            tolerated,
            fail_fast,
            commands,
            choices,
        })
}

fn limited(drive: &Drive) -> String {
    let shards: Vec<String> = (0..drive.results.len()).map(|n| n.to_string()).collect();
    let tolerated: Vec<String> = drive
        .tolerated
        .iter()
        .enumerate()
        .filter(|(_, tolerated)| **tolerated)
        .map(|(n, _)| n.to_string())
        .collect();
    format!(
        "on: workflow_dispatch\njobs:\n  test:\n    runs-on: self-hosted\n    \
         continue-on-error: ${{{{ contains(fromJSON('[{}]'), matrix.shard) }}}}\n    \
         strategy:\n      fail-fast: {}\n      max-parallel: {MAX_PARALLEL}\n      matrix:\n        \
         shard: [{}]\n    steps:\n      - run: make\n  after:\n    needs: test\n    \
         if: always()\n    runs-on: self-hosted\n    steps:\n      - run: ./after\n",
        tolerated.join(", "),
        drive.fail_fast,
        shards.join(", ")
    )
}

#[test]
fn with_max_parallel_no_more_members_than_the_limit_are_outside_waiting_and_completed() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let world = runtime.block_on(World::new());
    let mut runner = TestRunner::new(Config {
        cases: 96,
        failure_persistence: None,
        ..Config::default()
    });
    let result = runner.run(&drives(), |drive| {
        runtime.block_on(drive_group(&world, &drive))
    });
    runtime.block_on(async move { drop(world) });
    if let Err(failure) = result {
        panic!("{failure}");
    }
}

async fn drive_group(world: &World, drive: &Drive) -> Result<(), TestCaseError> {
    let run = world.dispatch(&parse(&limited(drive))).await;
    let stored = jobs(world, run).await;
    let template = id(group(&stored, "test"));
    let shard_of: HashMap<JobId, usize> = members(&stored, group(&stored, "test"))
        .into_iter()
        .map(|member| {
            let shard = member
                .matrix
                .as_ref()
                .and_then(|matrix| matrix["shard"].as_u64())
                .expect("a shard");
            (id(member), usize::try_from(shard).expect("a shard"))
        })
        .collect();
    if shard_of.len() != drive.results.len() {
        return Err(TestCaseError::fail(format!(
            "the matrix expanded to {} members",
            shard_of.len()
        )));
    }
    let mut choices = drive.choices.iter().copied();
    let mut settled = false;
    for _ in 0..MOST_ACTIONS {
        let projection = consistent(world, run).await?;
        bounded(&projection, template)?;
        let controls = world.controls().await;
        let (progress, extra) = actions(drive, &projection, &controls, &shard_of);
        let Some(&first) = progress.first() else {
            settled = true;
            break;
        };
        let action = match choices.next() {
            Some(choice) if drive.commands && !extra.is_empty() && choice % 6 == 0 => {
                extra[(choice as usize / 6) % extra.len()]
            }
            Some(choice) => progress[(choice as usize / 6) % progress.len()],
            None => first,
        };
        perform(world, run, action).await?;
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
    let group_row = &projection.jobs[&template];
    let mut expected = Conclusion::Success;
    for (job, row) in &projection.jobs {
        let Some(shard) = shard_of.get(job) else {
            continue;
        };
        if matches!(
            row.conclusion,
            Some(Conclusion::Failure | Conclusion::TimedOut)
        ) && !drive.tolerated[*shard]
        {
            expected = Conclusion::Failure;
        } else if row.conclusion == Some(Conclusion::Cancelled) && expected != Conclusion::Failure {
            expected = Conclusion::Cancelled;
        }
    }
    if group_row.conclusion != Some(expected) {
        return Err(TestCaseError::fail(format!(
            "the group concluded {:?}, and its members say {expected:?}: {projection:#?}",
            group_row.conclusion
        )));
    }
    if drive.fail_fast {
        nothing_started_after_a_failure(world, drive, run, &shard_of).await?;
    }
    Ok(())
}

fn bounded(projection: &Projection, group: JobId) -> Result<(), TestCaseError> {
    let outside: Vec<&super::JobRow> = projection
        .jobs
        .values()
        .filter(|row| row.group == Some(group))
        .filter(|row| !matches!(row.status, JobStatus::Waiting | JobStatus::Completed))
        .collect();
    if outside.len() > MAX_PARALLEL {
        return Err(TestCaseError::fail(format!(
            "{} members are outside waiting and completed with max-parallel {MAX_PARALLEL}: \
             {outside:#?}",
            outside.len()
        )));
    }
    if projection.jobs.values().any(|row| {
        row.group == Some(group)
            && row.status == JobStatus::Waiting
            && row.waiting_reason != Some(WaitingReason::MaxParallel)
    }) {
        return Err(TestCaseError::fail(
            "a member waits for another reason than max_parallel".to_owned(),
        ));
    }
    Ok(())
}

fn actions(
    drive: &Drive,
    projection: &Projection,
    controls: &BTreeSet<ExecutionId>,
    shard_of: &HashMap<JobId, usize>,
) -> (Vec<Action>, Vec<Action>) {
    let mut progress = Vec::new();
    let mut extra = vec![Action::CancelRun];
    for (&job, row) in &projection.jobs {
        if row.status != JobStatus::Completed {
            extra.push(Action::CancelJob(job));
        }
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
                let result = shard_of
                    .get(&row.job)
                    .is_none_or(|shard| drive.results[*shard]);
                progress.push(next_report(projection, execution, &[result]));
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

async fn nothing_started_after_a_failure(
    world: &World,
    drive: &Drive,
    run: RunId,
    shard_of: &HashMap<JobId, usize>,
) -> Result<(), TestCaseError> {
    let mut failed_at = None;
    for event in world.journal(run).await {
        let Event::JobStatusChanged(changed) = &event.event else {
            continue;
        };
        let Some(shard) = event.job_id.and_then(|job| shard_of.get(&job)) else {
            continue;
        };
        match failed_at {
            None if changed.to == JobStatus::Completed
                && matches!(
                    changed.conclusion,
                    Some(Conclusion::Failure | Conclusion::TimedOut)
                )
                && !drive.tolerated[*shard] =>
            {
                failed_at = Some(event.seq);
            }
            Some(at)
                if matches!(
                    changed.to,
                    JobStatus::Queued | JobStatus::Assigned | JobStatus::InProgress
                ) =>
            {
                return Err(TestCaseError::fail(format!(
                    "a member went to {:?} at {} after a member failed at {at} under fail-fast",
                    changed.to, event.seq
                )));
            }
            _ => {}
        }
    }
    Ok(())
}
