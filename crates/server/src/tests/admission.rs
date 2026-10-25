use axum::http::{Method, StatusCode};
use bosun_db::pools::Access;
use bosun_db::roles::Subject;
use bosun_db::secrets::Scope;
use bosun_types::actor::Actor;
use bosun_types::api::{Capability, TrustLevel};
use bosun_types::command::{Command, CommandRequest, Target};
use bosun_types::event::Event;
use bosun_types::id::RunId;
use bosun_types::status::{Conclusion, JobStatus, RunStatus, WaitingReason};
use sea_orm::prelude::Uuid;
use serde_json::json;

use super::access::{Site, forbidden};
use super::queue::{LEASE, assigned, claim, connected_in, parse, pool, values};
use super::{World, finished, keys, replay, template, workflow};
use crate::commands::Applied;
use crate::secrets;

const REFUSED: &str = "the run does not wait for admission; `run.admit` needs a run from a fork \
                       that waits for it";

fn admit() -> CommandRequest {
    CommandRequest {
        command: Command::RunAdmit,
        target: Target::default(),
        reason: None,
    }
}

async fn status(world: &World, run: RunId) -> Option<RunStatus> {
    world.project(run).await.run.map(|row| row.status)
}

async fn members(world: &World, run: RunId) -> Vec<String> {
    values(
        world.db(),
        &format!(
            "SELECT concat_ws(' ', j.matrix ->> 'shard', j.status, j.waiting_reason) AS value \
             FROM bosun.job j JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.group_id IS NOT NULL ORDER BY j.matrix ->> 'shard'"
        ),
    )
    .await
}

#[tokio::test]
async fn the_jobs_of_a_run_from_a_fork_wait_untrusted_until_a_command_admits_it() {
    let world = World::new().await;
    let run = world
        .dispatch_from_fork(
            &workflow(vec![
                template("build", &[], 1),
                template("test", &["build"], 1),
            ]),
            false,
        )
        .await;
    let projection = world.project(run).await;
    let (build, waiting) = projection.job("build");
    assert_eq!(
        (waiting.status, waiting.waiting_reason),
        (JobStatus::Waiting, Some(WaitingReason::Untrusted))
    );
    assert_eq!(projection.job("test").1.status, JobStatus::Pending);
    assert_eq!(status(&world, run).await, Some(RunStatus::Waiting));

    let Applied::Accepted(accepted) = world.command(run, &admit()).await else {
        panic!("the admission was refused");
    };
    let projection = world.project(run).await;
    let released = projection.job("build").1;
    assert_eq!(
        (released.status, released.waiting_reason),
        (JobStatus::Queued, None)
    );
    assert_eq!(status(&world, run).await, Some(RunStatus::Queued));
    let journal = world.journal(run).await;
    let moved = journal
        .iter()
        .find(|event| {
            event.job_id == Some(build)
                && matches!(
                    &event.event,
                    Event::JobStatusChanged(changed) if changed.to == JobStatus::Queued
                )
        })
        .expect("the release of the job");
    assert!(moved.seq > accepted.seq);
    assert_eq!(moved.actor, accepted.actor);
    assert!(
        matches!(&accepted.actor, Actor::Token { login, .. } if login == "roman"),
        "{accepted:?}"
    );

    world
        .finish(run, "build", finished(Conclusion::Success))
        .await;
    let test = world.project(run).await;
    let test = test.job("test").1;
    assert_eq!(
        (test.status, test.waiting_reason),
        (JobStatus::Queued, None)
    );
    match world.command(run, &admit()).await {
        Applied::Refused(message) => assert_eq!(message, REFUSED),
        Applied::Accepted(event) => panic!("a second admission was accepted: {event:?}"),
    }
    assert_eq!(
        replay(&world.journal(run).await),
        Ok(world.project(run).await)
    );
}

#[tokio::test]
async fn a_trusted_run_and_a_run_admitted_at_once_are_not_admitted_and_a_completed_one_neither() {
    let world = World::new().await;
    let jobs = workflow(vec![template("build", &[], 1)]);
    let trusted = world.dispatch(&jobs).await;
    let automatic = world.dispatch_from_fork(&jobs, true).await;
    let queued = world.project(automatic).await;
    let queued = queued.job("build").1;
    assert_eq!(
        (queued.status, queued.waiting_reason),
        (JobStatus::Queued, None)
    );
    for run in [trusted, automatic] {
        match world.command(run, &admit()).await {
            Applied::Refused(message) => assert_eq!(message, REFUSED),
            Applied::Accepted(event) => panic!("run {run} was admitted: {event:?}"),
        }
    }

    let cancelled = world.dispatch_from_fork(&jobs, false).await;
    let cancel = CommandRequest {
        command: Command::RunCancel,
        target: Target::default(),
        reason: None,
    };
    assert!(matches!(
        world.command(cancelled, &cancel).await,
        Applied::Accepted(_)
    ));
    assert_eq!(status(&world, cancelled).await, Some(RunStatus::Completed));
    match world.command(cancelled, &admit()).await {
        Applied::Refused(message) => assert_eq!(
            message,
            "the run is completed; `run.admit` needs a run from a fork that waits for it"
        ),
        Applied::Accepted(event) => panic!("a completed run was admitted: {event:?}"),
    }
}

#[tokio::test]
async fn the_members_of_a_matrix_wait_for_admission_and_then_for_room_under_max_parallel() {
    let world = World::new().await;
    let run = world
        .dispatch_from_fork(
            &parse(
                "on: workflow_dispatch
jobs:
  test:
    runs-on: self-hosted
    strategy:
      max-parallel: 2
      matrix:
        shard: [1, 2, 3]
    steps:
      - run: make
",
            ),
            false,
        )
        .await;
    assert_eq!(
        members(&world, run).await,
        [
            "1 waiting untrusted",
            "2 waiting untrusted",
            "3 waiting max_parallel"
        ]
    );
    assert_eq!(status(&world, run).await, Some(RunStatus::Waiting));

    assert!(matches!(
        world.command(run, &admit()).await,
        Applied::Accepted(_)
    ));
    assert_eq!(
        members(&world, run).await,
        ["1 queued", "2 queued", "3 waiting max_parallel"]
    );
    let first = values(
        world.db(),
        &format!(
            "SELECT j.id::text AS value FROM bosun.job j \
             JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.matrix ->> 'shard' = '1'"
        ),
    )
    .await;
    let first = first[0].parse().expect("the id of the first member");
    world
        .finish_job(run, first, finished(Conclusion::Success))
        .await;
    assert_eq!(
        members(&world, run).await,
        ["1 completed", "2 queued", "3 queued"]
    );
    assert_eq!(
        replay(&world.journal(run).await),
        Ok(world.project(run).await)
    );
}

#[tokio::test]
async fn the_assignment_of_a_run_from_a_fork_holds_no_secret_and_every_variable() {
    let world = World::new().await;
    let replica = Uuid::now_v7();
    let project = bosun_db::projects::find(world.db(), world.workflow.project_id)
        .await
        .expect("a project")
        .expect("the project of the world");
    let scope = Scope {
        organisation_id: project.organisation_id,
        project_id: Some(project.id),
    };
    let sealed = keys()
        .seal(&secrets::binding(&scope, "TOKEN"), b"the deploy token")
        .expect("a sealed value");
    bosun_db::secrets::set(world.db(), scope, "TOKEN", &sealed)
        .await
        .expect("a secret");
    bosun_db::variables::set(world.db(), scope, "REGION", "eu-central-1")
        .await
        .expect("a variable");
    let forks = pool(
        &world,
        "forks",
        TrustLevel::Untrusted,
        &[Access::AllProjects],
    )
    .await;
    let run = world
        .dispatch_from_fork(
            &parse(
                "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    env:
      TOKEN: ${{ secrets.TOKEN }}
      REGION: ${{ vars.REGION }}
    steps:
      - run: echo ${{ secrets.TOKEN }} ${{ secrets[format('{0}', 'TOKEN')] }}
",
            ),
            true,
        )
        .await;
    let outside = connected_in(
        &world,
        forks,
        "runner2-1",
        &["self-hosted", "linux", "x64", "podman"],
        &[Capability::Podman],
        replica,
    )
    .await;
    let claimed = claim(world.db(), outside.id, replica, LEASE, &[])
        .await
        .expect("the job of the run from a fork");
    let (_, documents) = assigned(&world, claimed).await;
    assert_eq!(documents.contexts.secrets, serde_json::Map::new());
    assert_eq!(
        documents.contexts.vars,
        json!({ "REGION": "eu-central-1" })
            .as_object()
            .cloned()
            .expect("an object")
    );
    assert_eq!(
        documents.evaluated.env,
        [
            ("REGION".to_owned(), "eu-central-1".to_owned()),
            ("TOKEN".to_owned(), String::new()),
        ]
        .into()
    );
    assert_eq!(
        world.project(run).await.job("build").1.status,
        JobStatus::Assigned
    );
}

#[tokio::test]
async fn a_developer_cannot_admit_a_run_from_a_fork_and_a_maintainer_can() {
    let site = Site::new().await;
    let project = site.project().await;
    let (developer, developing) = site.user("dana").await;
    site.bind(
        "developer",
        Subject::User(developer),
        project.organisation_id,
        None,
    )
    .await;
    let (maintainer, maintaining) = site.user("mia").await;
    site.bind(
        "maintainer",
        Subject::User(maintainer),
        project.organisation_id,
        Some(project.id),
    )
    .await;
    let (run, job) = site.run_from_fork().await;
    let path = format!("/api/v1/runs/{run}/commands");
    let body = json!({
        "type": "run.admit",
        "target": { "attempt": null, "job": null, "step": null },
        "reason": null
    });

    let refused = site
        .send(Method::POST, &path, &developing, Some(body.clone()))
        .await;
    forbidden(&refused, "a developer's admission");
    assert_eq!(
        refused.body["error"]["message"],
        "`dana` does not hold `run.override` on project `keel`"
    );
    let waiting = site.world.project(run).await;
    assert_eq!(
        waiting.jobs[&job].waiting_reason,
        Some(WaitingReason::Untrusted)
    );

    let accepted = site
        .send(Method::POST, &path, &maintaining, Some(body.clone()))
        .await;
    assert_eq!(accepted.status, StatusCode::CREATED, "{accepted:?}");
    assert_eq!(accepted.body["kind"], "command.accepted");
    assert_eq!(accepted.body["actor"]["login"], "mia");
    assert_eq!(
        site.world.project(run).await.jobs[&job].status,
        JobStatus::Queued
    );

    let again = site
        .send(Method::POST, &path, &maintaining, Some(body))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{again:?}");
    assert_eq!(again.body["error"]["code"], "invalid_state");
    assert_eq!(again.body["error"]["message"], REFUSED);
}
