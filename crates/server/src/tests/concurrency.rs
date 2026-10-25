use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};

use bosun_db::entity::{attempt, control_message, job};
use bosun_db::runs::{NewRun, Trust};
use bosun_db::secrets::Scope;
use bosun_db::workflows::NewWorkflow;
use bosun_types::api::Workflow;
use bosun_types::command::{Command, CommandRequest, Target};
use bosun_types::event::{Arrival, ConcurrencyCancelled, Event};
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::ir;
use bosun_types::status::{
    Conclusion, ExecutionStatus, JobStatus, RunConclusion, RunStatus, StatusReason, WaitingReason,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use proptest::collection::vec;
use proptest::prelude::{Strategy, any};
use proptest::test_runner::{Config, TestCaseError, TestRunner};

use super::access::{Site, owner_credentials};
use super::journal::{Action, consistent, perform};
use super::matrix::flow;
use super::planning::strings;
use super::{World, finished, operator, run_cancel, site};
use crate::commands::Applied;
use crate::planner;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    description: String,
    workflows: BTreeMap<String, bosun_expr::Value>,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Dispatch {
        workflow: String,
        run: String,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        times: Option<usize>,
    },
    Start {
        run: String,
        job: String,
    },
    Finish {
        run: String,
        job: String,
        conclusion: Conclusion,
    },
    Cancel {
        run: String,
    },
    Expect(BTreeMap<String, BTreeMap<String, Value>>),
}

fn cases() -> Vec<(PathBuf, Case)> {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/concurrency");
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

fn parse(source: &str) -> ir::Workflow {
    bosun_workflow::parse("ci", source)
        .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
        .workflow
}

async fn stored(world: &World, name: &str, number: u8) -> Workflow {
    bosun_db::workflows::create(
        world.db(),
        NewWorkflow {
            project_id: world.workflow.project_id,
            name,
            source: "name: ci\n",
            sha256: vec![number; 32],
        },
    )
    .await
    .expect("a workflow")
}

async fn dispatch(
    world: &World,
    stored: &Workflow,
    workflow: &ir::Workflow,
    git_ref: Option<&str>,
) -> RunId {
    let transaction = world.db().begin().await.expect("a transaction");
    let payload = json!({ "inputs": null, "ref": git_ref, "workflow": stored.name });
    let run = planner::dispatch(
        &transaction,
        NewRun {
            workflow: stored,
            revision_id: stored.revision_id.expect("a current revision"),
            ir: workflow,
            event: "workflow_dispatch",
            git_ref: None,
            sha: None,
            payload: &payload,
            inputs: &Map::new(),
            actor: &operator(),
            trust: Trust::Trusted,
        },
        &[],
        &site(),
    )
    .await
    .expect("a dispatch");
    transaction.commit().await.expect("a commit");
    run.id
}

async fn jobs(world: &World, run: RunId) -> Vec<job::Model> {
    let attempts: Vec<_> = attempt::Entity::find()
        .filter(attempt::Column::RunId.eq(run.as_uuid()))
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

async fn named(world: &World, run: RunId, name: &str) -> (JobId, JobStatus) {
    let found = jobs(world, run).await;
    let job = found
        .iter()
        .find(|job| job.name == name)
        .unwrap_or_else(|| panic!("no job `{name}` in {found:#?}"));
    (super::id(job.id), super::name(&job.status))
}

async fn running(world: &World, run: RunId, job: JobId) -> ExecutionId {
    let projection = world.project(run).await;
    projection
        .executions_of(job)
        .into_iter()
        .find(|(_, execution)| execution.status == ExecutionStatus::InProgress)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("job {job} runs on no agent: {projection:#?}"))
}

async fn completion(world: &World, run: RunId, job: JobId) -> Option<StatusReason> {
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

fn text<T: serde::Serialize>(value: T) -> String {
    match serde_json::to_value(value).expect("a JSON value") {
        Value::String(text) => text,
        other => panic!("{other} is not a name"),
    }
}

async fn shown(world: &World, run: RunId) -> BTreeMap<String, Value> {
    let view = world.run(run).await;
    let mut state = BTreeMap::new();
    let mut parts = vec![text(view.status)];
    parts.extend(view.conclusion.map(text));
    state.insert("run".to_owned(), Value::String(parts.join(" ")));
    for job in jobs(world, run).await {
        let mut parts = vec![job.status.clone()];
        parts.extend(job.waiting_reason.clone());
        parts.extend(job.conclusion.clone());
        if job.status == "completed" {
            parts.extend(completion(world, run, super::id(job.id)).await.map(text));
        }
        state.insert(job.name, Value::String(parts.join(" ")));
    }
    if !view.notices.is_empty() {
        let codes: Vec<Value> = view
            .notices
            .into_iter()
            .map(|notice| Value::String(notice.diagnostic.code))
            .collect();
        state.insert("notices".to_owned(), Value::Array(codes));
    }
    state
}

fn lookup(runs: &BTreeMap<String, RunId>, name: &str, at: &str) -> RunId {
    *runs
        .get(name)
        .unwrap_or_else(|| panic!("{at}: no run `{name}`"))
}

async fn play(world: &World, file: &str, case: &Case) {
    let mut workflows = BTreeMap::new();
    for (number, (name, workflow)) in case.workflows.iter().enumerate() {
        let number = u8::try_from(number + 10).expect("a few workflows");
        let stored = stored(world, &format!("{file}-{name}"), number).await;
        workflows.insert(name.clone(), (stored, parse(&flow(workflow))));
    }
    let mut runs: BTreeMap<String, RunId> = BTreeMap::new();
    for (index, step) in case.steps.iter().enumerate() {
        let at = format!("{file}, step {index}: {}", case.description);
        let run = |name: &str| lookup(&runs, name, &at);
        match step {
            Step::Dispatch {
                workflow,
                run: name,
                git_ref,
                times,
            } => {
                let (stored, workflow) = &workflows[workflow];
                let names = match times {
                    None => vec![name.clone()],
                    Some(times) => (1..=*times)
                        .map(|number| format!("{name}-{number}"))
                        .collect(),
                };
                for name in names {
                    let id = dispatch(world, stored, workflow, git_ref.as_deref()).await;
                    runs.insert(name, id);
                }
            }
            Step::Start { run: name, job } => {
                let run = run(name);
                let (job, status) = named(world, run, job).await;
                assert_eq!(status, JobStatus::Queued, "{at}");
                world.start(run, job).await;
            }
            Step::Finish {
                run: name,
                job,
                conclusion,
            } => {
                let run = run(name);
                let (job, status) = named(world, run, job).await;
                let execution = match status {
                    JobStatus::Queued => world.start(run, job).await,
                    JobStatus::InProgress => running(world, run, job).await,
                    other => panic!("{at}: job {job} is {other:?} and cannot finish"),
                };
                world.end(execution, finished(*conclusion)).await;
            }
            Step::Cancel { run: name } => {
                world.command(run(name), &run_cancel()).await;
            }
            Step::Expect(expected) => {
                for (name, state) in expected {
                    assert_eq!(&shown(world, run(name)).await, state, "{at}: run `{name}`");
                }
            }
        }
        for (name, run) in &runs {
            if let Err(failure) = consistent(world, *run).await {
                panic!("{at}: run `{name}`: {failure}");
            }
        }
    }
}

#[tokio::test]
async fn the_concurrency_cases_pass() {
    let cases = cases();
    assert!(
        !cases.is_empty(),
        "contracts/fixtures/concurrency holds no case"
    );
    for (file, case) in cases {
        let world = World::new().await;
        let name = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("a file name");
        play(&world, name, &case).await;
    }
}

const GROUPED: &str = "on: workflow_dispatch
concurrency:
  group: deploy
  cancel-in-progress: true
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
  test:
    runs-on: self-hosted
    steps:
      - run: make
  report:
    needs: build
    if: always()
    runs-on: self-hosted
    steps:
      - run: make
";

#[tokio::test]
async fn an_arrival_with_cancel_in_progress_cancels_the_running_run_as_a_cancel_does() {
    let world = World::new().await;
    let workflow = parse(GROUPED);
    let first = world.dispatch(&workflow).await;
    let (build, _) = named(&world, first, "build").await;
    let execution = world.start(first, build).await;
    let seen = world.journal(first).await.len();
    let second = world.dispatch(&workflow).await;

    let events = world.journal(first).await;
    let cancelled: Vec<&Event> = events[seen..].iter().map(|event| &event.event).collect();
    let (test, _) = named(&world, first, "test").await;
    assert_eq!(
        cancelled.first(),
        Some(&&Event::ConcurrencyCancelled(ConcurrencyCancelled {
            group: "deploy".to_owned(),
            arrival: Arrival {
                run_id: second,
                job_id: None,
            },
        })),
        "{cancelled:#?}"
    );
    assert_eq!(events[seen].job_id, None);
    assert_eq!(
        completion(&world, first, test).await,
        Some(StatusReason::Concurrency)
    );
    let controls: Vec<ExecutionId> = control_message::Entity::find()
        .all(world.db())
        .await
        .expect("the control messages")
        .into_iter()
        .map(|control| super::id(control.execution_id))
        .collect();
    assert_eq!(controls, [execution]);
    let projection = consistent(&world, first)
        .await
        .expect("a consistent journal");
    assert!(
        projection
            .run
            .as_ref()
            .is_some_and(|run| run.cancel_requested)
    );

    world.end(execution, finished(Conclusion::Cancelled)).await;
    let (report, status) = named(&world, first, "report").await;
    assert_eq!(
        status,
        JobStatus::Queued,
        "`if: always()` runs after the cancel"
    );
    world
        .finish_job(first, report, finished(Conclusion::Success))
        .await;
    let shown = world.run(first).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
    let shown = world.run(second).await;
    assert_eq!(shown.status, RunStatus::Queued);
    consistent(&world, second)
        .await
        .expect("a consistent journal");
}

const MOST_WAITING: usize = 100;

async fn statuses(world: &World, runs: &[RunId]) -> Vec<(RunStatus, Option<RunConclusion>)> {
    let mut found = Vec::new();
    for run in runs {
        let shown = world.run(*run).await;
        found.push((shown.status, shown.conclusion));
    }
    found
}

async fn crowded(world: &World) -> (Vec<RunId>, RunId) {
    let workflow = parse(
        "on: workflow_dispatch\nconcurrency:\n  group: deploy\n  queue: max\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      - run: make\n",
    );
    let mut members = Vec::new();
    for _ in 0..=MOST_WAITING {
        members.push(world.dispatch(&workflow).await);
    }
    let refused = world.dispatch(&workflow).await;
    (members, refused)
}

pub(super) async fn full_queue_notices() -> Vec<String> {
    let world = World::new().await;
    let (_, refused) = crowded(&world).await;
    world
        .run(refused)
        .await
        .notices
        .into_iter()
        .map(|notice| notice.diagnostic.code)
        .collect()
}

#[tokio::test]
async fn under_queue_max_a_hundred_members_wait_and_start_in_their_order_of_arrival() {
    let world = World::new().await;
    let (members, refused) = crowded(&world).await;
    let waiting = (RunStatus::Waiting, None);
    let mut expected = vec![(RunStatus::Queued, None)];
    expected.extend(std::iter::repeat_n(waiting, MOST_WAITING));
    assert_eq!(statuses(&world, &members).await, expected);
    let shown = world.run(refused).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
    let (build, _) = named(&world, refused, "build").await;
    assert_eq!(
        completion(&world, refused, build).await,
        Some(StatusReason::Concurrency)
    );
    let codes: Vec<&str> = shown
        .notices
        .iter()
        .map(|notice| notice.diagnostic.code.as_str())
        .collect();
    assert_eq!(codes, ["concurrency_queue_full"]);

    for (index, member) in members.iter().enumerate() {
        let statuses = statuses(&world, &members).await;
        assert_eq!(
            statuses[index],
            (RunStatus::Queued, None),
            "member {index} is next"
        );
        assert!(
            statuses[index + 1..]
                .iter()
                .all(|status| *status == waiting),
            "member {index} runs alone: {statuses:?}"
        );
        world
            .finish(*member, "build", finished(Conclusion::Success))
            .await;
    }
    let done = (RunStatus::Completed, Some(RunConclusion::Success));
    assert!(
        statuses(&world, &members)
            .await
            .iter()
            .all(|status| *status == done)
    );
}

#[tokio::test]
async fn a_job_with_cancel_in_progress_cancels_a_queued_member_and_takes_its_place_at_once() {
    let world = World::new().await;
    let workflow = parse(
        "on: workflow_dispatch\njobs:\n  ship:\n    runs-on: self-hosted\n    concurrency:\n      \
         group: ship\n      cancel-in-progress: true\n    steps:\n      - run: make\n",
    );
    let first = world.dispatch(&workflow).await;
    let second = world.dispatch(&workflow).await;
    let (ship, status) = named(&world, first, "ship").await;
    assert_eq!(status, JobStatus::Completed);
    assert_eq!(
        completion(&world, first, ship).await,
        Some(StatusReason::Concurrency)
    );
    let (arriving, status) = named(&world, second, "ship").await;
    assert_eq!(status, JobStatus::Queued);
    let cancelled: Vec<(Option<JobId>, Event)> = world
        .journal(first)
        .await
        .into_iter()
        .filter(|event| matches!(event.event, Event::ConcurrencyCancelled(_)))
        .map(|event| (event.job_id, event.event))
        .collect();
    assert_eq!(
        cancelled,
        [(
            Some(ship),
            Event::ConcurrencyCancelled(ConcurrencyCancelled {
                group: "ship".to_owned(),
                arrival: Arrival {
                    run_id: second,
                    job_id: Some(arriving),
                },
            })
        )]
    );
    for run in [first, second] {
        consistent(&world, run).await.expect("a consistent journal");
    }
}

fn admit() -> CommandRequest {
    CommandRequest {
        command: Command::RunAdmit,
        target: Target::default(),
        reason: None,
    }
}

async fn members(world: &World, run: RunId) -> Vec<String> {
    strings(
        world,
        &format!(
            "SELECT concat_ws(' ', key, running::text) AS value FROM bosun.concurrency_member \
             WHERE run_id = '{run}' ORDER BY id"
        ),
    )
    .await
}

#[tokio::test]
async fn a_run_from_a_fork_enters_its_workflow_group_when_a_command_admits_it() {
    let world = World::new().await;
    let workflow = parse(
        "on: workflow_dispatch\nconcurrency:\n  group: deploy\n  cancel-in-progress: true\njobs:\n  \
         build:\n    runs-on: self-hosted\n    steps:\n      - run: make\n",
    );
    let first = world.dispatch(&workflow).await;
    let forked = world.dispatch_from_fork(&workflow, false).await;

    assert_eq!(named(&world, forked, "build").await.1, JobStatus::Waiting);
    assert_eq!(
        jobs(&world, forked).await[0].waiting_reason.as_deref(),
        Some("untrusted")
    );
    assert_eq!(members(&world, forked).await, Vec::<String>::new());
    assert_eq!(members(&world, first).await, ["deploy true"]);
    assert_eq!(named(&world, first, "build").await.1, JobStatus::Queued);
    assert!(
        world
            .journal(first)
            .await
            .iter()
            .all(|event| !matches!(event.event, Event::ConcurrencyCancelled(_)))
    );

    assert!(matches!(
        world.command(forked, &admit()).await,
        Applied::Accepted(_)
    ));
    assert_eq!(
        completion(&world, first, named(&world, first, "build").await.0).await,
        Some(StatusReason::Concurrency)
    );
    assert!(world.journal(first).await.iter().any(|event| event.event
        == Event::ConcurrencyCancelled(ConcurrencyCancelled {
            group: "deploy".to_owned(),
            arrival: Arrival {
                run_id: forked,
                job_id: None,
            },
        })));
    assert_eq!(named(&world, forked, "build").await.1, JobStatus::Queued);
    assert_eq!(members(&world, forked).await, ["deploy true"]);
    for run in [first, forked] {
        consistent(&world, run).await.expect("a consistent journal");
    }
}

#[tokio::test]
async fn a_run_from_a_fork_that_finds_room_in_its_groups_is_queued_with_the_actor_of_the_command() {
    let world = World::new().await;
    let workflow = parse(
        "on: workflow_dispatch\nconcurrency: deploy\njobs:\n  build:\n    runs-on: self-hosted\n    \
         concurrency: lane\n    steps:\n      - run: make\n",
    );
    let forked = world.dispatch_from_fork(&workflow, false).await;
    let (build, status) = named(&world, forked, "build").await;
    assert_eq!(status, JobStatus::Waiting);

    let Applied::Accepted(accepted) = world.command(forked, &admit()).await else {
        panic!("the admission was refused");
    };
    assert_eq!(named(&world, forked, "build").await.1, JobStatus::Queued);
    assert_eq!(members(&world, forked).await, ["deploy true", "lane true"]);
    let released = world
        .journal(forked)
        .await
        .into_iter()
        .find(|event| {
            event.job_id == Some(build)
                && matches!(
                    &event.event,
                    Event::JobStatusChanged(changed) if changed.to == JobStatus::Queued
                )
        })
        .expect("the release of the job");
    assert_eq!(released.actor, accepted.actor);
    consistent(&world, forked)
        .await
        .expect("a consistent journal");
}

#[tokio::test]
async fn the_jobs_of_a_run_from_a_fork_enter_their_groups_when_a_command_admits_the_run() {
    let world = World::new().await;
    let workflow = parse(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    concurrency: lane\n    \
         steps:\n      - run: make\n",
    );
    let first = world.dispatch(&workflow).await;
    let forked = world.dispatch_from_fork(&workflow, false).await;
    assert_eq!(members(&world, forked).await, Vec::<String>::new());
    assert_eq!(members(&world, first).await, ["lane true"]);

    let Applied::Accepted(accepted) = world.command(forked, &admit()).await else {
        panic!("the admission was refused");
    };
    let (build, status) = named(&world, forked, "build").await;
    assert_eq!(status, JobStatus::Waiting);
    assert_eq!(
        jobs(&world, forked).await[0].waiting_reason.as_deref(),
        Some("concurrency")
    );
    assert_eq!(members(&world, forked).await, ["lane false"]);
    assert_eq!(world.run(forked).await.status, RunStatus::Waiting);
    let held: Vec<_> = world
        .journal(forked)
        .await
        .into_iter()
        .filter(|event| {
            event.job_id == Some(build)
                && matches!(
                    &event.event,
                    Event::JobStatusChanged(changed)
                        if changed.waiting_reason == Some(WaitingReason::Concurrency)
                )
        })
        .map(|event| event.actor)
        .collect();
    assert_eq!(held, [accepted.actor]);

    world
        .finish(first, "build", finished(Conclusion::Success))
        .await;
    assert_eq!(
        named(&world, forked, "build").await,
        (build, JobStatus::Queued)
    );
    for run in [first, forked] {
        consistent(&world, run).await.expect("a consistent journal");
    }
}

#[tokio::test]
async fn a_workflow_group_that_fails_when_a_command_admits_the_run_cancels_its_jobs() {
    let world = World::new().await;
    let project = bosun_db::projects::find(world.db(), world.workflow.project_id)
        .await
        .expect("a project")
        .expect("the project of the world");
    let scope = Scope {
        organisation_id: project.organisation_id,
        project_id: Some(project.id),
    };
    bosun_db::variables::set(world.db(), scope, "LANE", "main")
        .await
        .expect("a variable");
    let workflow = parse(
        "on: workflow_dispatch\nconcurrency: ${{ vars.LANE }}\njobs:\n  build:\n    runs-on: \
         self-hosted\n    steps:\n      - run: make\n",
    );
    let forked = world.dispatch_from_fork(&workflow, false).await;
    assert_eq!(named(&world, forked, "build").await.1, JobStatus::Waiting);

    bosun_db::variables::set(world.db(), scope, "LANE", "")
        .await
        .expect("a variable");
    assert!(matches!(
        world.command(forked, &admit()).await,
        Applied::Accepted(_)
    ));
    let (build, status) = named(&world, forked, "build").await;
    assert_eq!(status, JobStatus::Completed);
    assert_eq!(
        completion(&world, forked, build).await,
        Some(StatusReason::EvaluationFailed)
    );
    let shown = world.run(forked).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
    let codes: Vec<String> = shown
        .notices
        .into_iter()
        .map(|notice| notice.diagnostic.code)
        .collect();
    assert_eq!(codes, ["invalid_value"]);
    assert_eq!(members(&world, forked).await, Vec::<String>::new());
    consistent(&world, forked)
        .await
        .expect("a consistent journal");
}

const ROUTINE: &str = "on: [repository_dispatch, workflow_dispatch]\njobs:\n  build:\n    \
                       runs-on: self-hosted\n    steps:\n      - run: make\n";

fn grouped(group: &str) -> String {
    ROUTINE.replacen("jobs:", &format!("concurrency: {group}\njobs:"), 1)
}

async fn waiting(world: &World, transactions: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let sql = "SELECT count(*)::text AS value FROM pg_stat_activity \
               WHERE datname = current_database() AND wait_event_type = 'Lock'";
    loop {
        let found = super::planning::strings(world, sql).await;
        if found == [transactions.to_string()] {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{found:?} transactions wait for a lock rather than {transactions}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn planning_an_event_races_a_dispatch_and_a_holder_of_the_lock_without_a_deadlock() {
    let api = Site::new().await;
    let project = api.project().await;
    let mut created = Vec::new();
    for (number, (name, source)) in [
        ("alpha", ROUTINE.to_owned()),
        ("beta", grouped("beta")),
        ("gamma", grouped("gamma")),
    ]
    .iter()
    .enumerate()
    {
        let number = u8::try_from(number + 10).expect("three workflows");
        created.push(
            bosun_db::workflows::create(
                api.db(),
                NewWorkflow {
                    project_id: project.id,
                    name,
                    source,
                    sha256: vec![number; 32],
                },
            )
            .await
            .expect("a workflow"),
        );
    }
    let [alpha, _, gamma] = created.as_slice() else {
        panic!("three workflows: {created:#?}");
    };
    let credentials = owner_credentials(&api).await;
    let holder = api.db().begin().await.expect("a transaction");
    bosun_db::runs::lock_concurrency(&holder, project.id)
        .await
        .expect("the concurrency lock");
    let path = format!("/api/v1/projects/globalart/{}/events", project.slug);
    let regrouped = parse(&grouped("alpha"));
    let payload = json!({ "inputs": null, "ref": null, "workflow": alpha.name });

    let (planned, (dispatched, held)) = tokio::join!(
        api.send(
            Method::POST,
            &path,
            &credentials,
            Some(json!({ "event_type": "deploy" })),
        ),
        async {
            waiting(&api.world, 1).await;
            tokio::join!(api.dispatch(gamma, &credentials), async {
                waiting(&api.world, 2).await;
                let held = planner::dispatch(
                    &holder,
                    NewRun {
                        workflow: alpha,
                        revision_id: alpha.revision_id.expect("a current revision"),
                        ir: &regrouped,
                        event: "workflow_dispatch",
                        git_ref: None,
                        sha: None,
                        payload: &payload,
                        inputs: &Map::new(),
                        actor: &operator(),
                        trust: Trust::Trusted,
                    },
                    &[],
                    &site(),
                )
                .await;
                holder.commit().await.expect("a commit");
                held
            })
        }
    );

    assert_eq!(
        held.expect("the holder of the lock creates a run").status,
        RunStatus::Queued
    );
    assert_eq!(planned.status, StatusCode::CREATED, "{planned:?}");
    let statuses: Vec<&Value> = planned.body["runs"]
        .as_array()
        .expect("the runs")
        .iter()
        .map(|run| &run["status"])
        .collect();
    assert_eq!(statuses, ["queued", "queued", "queued"], "{planned:?}");
    assert_eq!(dispatched.status, StatusCode::CREATED, "{dispatched:?}");
    assert_eq!(dispatched.body["status"], "waiting", "{dispatched:?}");
}

const MOST_RUNS: usize = 5;

const MOST_ACTIONS: usize = 200;

#[derive(Debug, Clone)]
struct Mix {
    queue_max: bool,
    cancel_in_progress: bool,
    ship_cancels: bool,
    arrivals: Vec<usize>,
    choices: Vec<u32>,
}

fn mixes() -> impl Strategy<Value = Mix> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        vec(0..3usize, 1..=MOST_RUNS),
        vec(any::<u32>(), 0..160),
    )
        .prop_map(
            |(queue_max, cancel_in_progress, ship_cancels, arrivals, choices)| Mix {
                queue_max,
                cancel_in_progress: cancel_in_progress && !queue_max,
                ship_cancels,
                arrivals,
                choices,
            },
        )
}

fn sources(mix: &Mix) -> [String; 3] {
    let steps = "    runs-on: self-hosted\n    steps:\n      - run: make\n";
    [
        format!(
            "on: workflow_dispatch\nconcurrency:\n  group: Deploy\n  queue: {}\n  \
             cancel-in-progress: {}\njobs:\n  build:\n{steps}  report:\n    needs: build\n    \
             if: always()\n{steps}",
            if mix.queue_max { "max" } else { "single" },
            mix.cancel_in_progress
        ),
        format!(
            "on: workflow_dispatch\njobs:\n  build:\n{steps}  ship:\n    needs: build\n    \
             concurrency:\n      group: deploy\n      cancel-in-progress: {}\n{steps}",
            mix.ship_cancels
        ),
        format!(
            "on: workflow_dispatch\njobs:\n  roll:\n    concurrency: DEPLOY\n    strategy:\n      \
             max-parallel: 1\n      matrix:\n        n: [1, 2]\n{steps}"
        ),
    ]
}

async fn holders(world: &World, workflows: &[Workflow]) -> Result<(u64, u64), TestCaseError> {
    let active = "('queued', 'assigned', 'in_progress')";
    let sql = format!(
        "SELECT concat_ws(' ', \
         (SELECT count(DISTINCT a.run_id) FROM bosun.job j \
          JOIN bosun.attempt a ON a.id = j.attempt_id JOIN bosun.run r ON r.id = a.run_id \
          WHERE r.workflow_id = '{}' AND j.status IN {active}) + \
         (SELECT count(*) FROM bosun.job j JOIN bosun.attempt a ON a.id = j.attempt_id \
          JOIN bosun.run r ON r.id = a.run_id \
          WHERE ((r.workflow_id = '{}' AND j.key = 'ship') \
             OR (r.workflow_id = '{}' AND j.group_id IS NOT NULL)) \
          AND j.status IN {active}), \
         (SELECT count(*) FROM bosun.concurrency_member WHERE running)) AS value",
        workflows[0].id, workflows[1].id, workflows[2].id
    );
    let found = super::planning::strings(world, &sql).await;
    let counts: Vec<u64> = found
        .first()
        .map(|value| {
            value
                .split(' ')
                .filter_map(|count| count.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    match counts.as_slice() {
        [active, admitted] => Ok((*active, *admitted)),
        _ => Err(TestCaseError::fail(format!(
            "no counts of members: {found:?}"
        ))),
    }
}

async fn progress_of(
    world: &World,
    runs: &[RunId],
) -> (Vec<(RunId, Action)>, Vec<(RunId, Action)>) {
    let controls = world.controls().await;
    let mut progress = Vec::new();
    let mut extra = Vec::new();
    for run in runs {
        let projection = world.project(*run).await;
        if projection
            .run
            .as_ref()
            .is_some_and(|row| row.status != RunStatus::Completed)
        {
            extra.push((*run, Action::CancelRun));
        }
        for (&job, row) in &projection.jobs {
            if row.status != JobStatus::Completed {
                extra.push((*run, Action::CancelJob(job)));
            }
            if row.status == JobStatus::Queued {
                progress.push((*run, Action::Assign(job)));
            }
        }
        for (&execution, row) in &projection.executions {
            match row.status {
                ExecutionStatus::Assigned => {
                    progress.push((*run, Action::Accept(execution)));
                    extra.push((*run, Action::Expire(execution)));
                }
                ExecutionStatus::InProgress => {
                    progress.push((*run, Action::FinishJob(execution, Conclusion::Success)));
                    extra.push((*run, Action::FinishJob(execution, Conclusion::Failure)));
                    if controls.contains(&execution) {
                        progress.push((*run, Action::FinishJob(execution, Conclusion::Cancelled)));
                    }
                }
                _ => {}
            }
        }
    }
    (progress, extra)
}

async fn mingle(world: &World, workflows: &[Workflow], mix: &Mix) -> Result<(), TestCaseError> {
    let sources = sources(mix);
    let lowered: Vec<ir::Workflow> = sources.iter().map(|source| parse(source)).collect();
    let mut arrivals = mix.arrivals.iter().copied();
    let mut choices = mix.choices.iter().copied();
    let mut runs: Vec<RunId> = Vec::new();
    let mut settled = false;
    for _ in 0..MOST_ACTIONS {
        let (active, admitted) = holders(world, workflows).await?;
        if active > 1 || admitted > 1 {
            return Err(TestCaseError::fail(format!(
                "{active} members of the group are active and {admitted} admitted"
            )));
        }
        let (progress, extra) = progress_of(world, &runs).await;
        let choice = choices.next();
        if choice.is_some_and(|choice| choice % 4 == 0)
            && let Some(arrival) = arrivals.next()
        {
            runs.push(dispatch(world, &workflows[arrival], &lowered[arrival], None).await);
            continue;
        }
        let action = match choice {
            Some(choice) if !extra.is_empty() && choice % 7 == 1 => {
                extra[(choice as usize / 7) % extra.len()]
            }
            Some(choice) if !progress.is_empty() => progress[(choice as usize) % progress.len()],
            _ => match progress.first() {
                Some(first) => *first,
                None => match arrivals.next() {
                    Some(arrival) => {
                        runs.push(
                            dispatch(world, &workflows[arrival], &lowered[arrival], None).await,
                        );
                        continue;
                    }
                    None => {
                        settled = true;
                        break;
                    }
                },
            },
        };
        let (run, action) = action;
        perform(world, run, action).await?;
    }
    if !settled {
        return Err(TestCaseError::fail(format!(
            "the runs were still going after {MOST_ACTIONS} actions"
        )));
    }
    for run in &runs {
        let projection = consistent(world, *run).await?;
        if projection
            .run
            .as_ref()
            .is_none_or(|row| row.status != RunStatus::Completed)
        {
            return Err(TestCaseError::fail(format!(
                "a run waits with nothing left to do: {projection:#?}"
            )));
        }
    }
    let left = super::planning::strings(
        world,
        "SELECT count(*)::text AS value FROM bosun.concurrency_member",
    )
    .await;
    if left != ["0"] {
        return Err(TestCaseError::fail(format!(
            "{left:?} members stayed in their groups after every run completed"
        )));
    }
    Ok(())
}

#[test]
fn runs_and_jobs_of_one_group_never_hold_it_together_and_every_run_completes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let mut runner = TestRunner::new(Config {
        cases: 32,
        failure_persistence: None,
        ..Config::default()
    });
    let result = runner.run(&mixes(), |mix| {
        runtime.block_on(async {
            let world = World::new().await;
            let mut workflows = Vec::new();
            for (number, name) in ["release", "ship", "roll"].into_iter().enumerate() {
                let number = u8::try_from(number + 10).expect("three workflows");
                workflows.push(stored(&world, name, number).await);
            }
            mingle(&world, &workflows, &mix).await
        })
    });
    if let Err(failure) = result {
        panic!("{failure}");
    }
}
