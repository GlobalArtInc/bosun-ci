use bosun_types::event::Event;
use bosun_types::ir::{Evaluable, RunsOn};
use bosun_types::status::{
    Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason, StepStatus,
};
use sea_orm::{ConnectionTrait, Statement};

use super::{World, replay, template, workflow};
use crate::agents::progress::{self, Progress, StepEnd};

async fn strings(world: &World, sql: &str) -> Vec<String> {
    world
        .db()
        .query_all(Statement::from_string(
            world.db().get_database_backend(),
            sql,
        ))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_iter()
        .map(|row| row.try_get::<String>("", "value").expect("a text column"))
        .collect()
}

#[tokio::test]
async fn jobs_without_needs_are_queued_at_dispatch_with_their_labels_and_the_pools_they_may_use() {
    let world = World::new().await;
    let mut grouped = template("grouped", &[], 1);
    grouped.runs_on = Evaluable::Literal(RunsOn {
        group: Some("default".to_owned()),
        labels: vec!["Shell".to_owned()],
    });
    let mut nowhere = template("nowhere", &[], 1);
    nowhere.runs_on = Evaluable::Literal(RunsOn {
        group: Some("nowhere".to_owned()),
        labels: Vec::new(),
    });
    let run = world
        .dispatch(&workflow(vec![
            template("test", &["build"], 1),
            template("build", &[], 2),
            grouped,
            nowhere,
        ]))
        .await;

    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    let run_row = projection.run.as_ref().expect("the run");
    assert_eq!(
        (run_row.status, run_row.started),
        (RunStatus::Queued, false)
    );
    assert_eq!(projection.job("build").1.status, JobStatus::Queued);
    assert_eq!(projection.job("test").1.status, JobStatus::Pending);
    assert_eq!(
        strings(
            &world,
            "SELECT concat_ws(' ', j.key, j.status, array_to_string(j.labels, ','), \
             (SELECT string_agg(p.name, ',' ORDER BY p.name) FROM bosun.pool p \
              WHERE p.id = ANY(j.pool_ids)), j.queued_at IS NOT NULL) AS value \
             FROM bosun.job j ORDER BY j.key"
        )
        .await,
        [
            "build queued self-hosted,linux default t",
            "grouped queued shell default t",
            "nowhere queued  t",
            "test pending  f",
        ]
    );
}

#[tokio::test]
async fn a_job_waits_for_every_job_it_needs_and_the_run_succeeds_when_all_do() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![
            template("a", &[], 1),
            template("b", &["a"], 1),
            template("c", &["a"], 1),
            template("d", &["b", "c"], 1),
        ]))
        .await;
    let finish = |key: &'static str| {
        let world = &world;
        async move {
            let (job, _) = world.project(run).await.job(key);
            let execution = world.assign(run, job).await;
            let agent = world.agent();
            assert_eq!(
                progress::accepted(world.db(), agent, execution)
                    .await
                    .expect("an acceptance"),
                Progress::Applied
            );
            assert_eq!(
                world.project(run).await.run.expect("the run").status,
                RunStatus::InProgress
            );
            for (outcome, applied) in [
                (
                    progress::step_started(world.db(), agent, execution, 0, None).await,
                    "a started step",
                ),
                (
                    progress::step_finished(
                        world.db(),
                        agent,
                        execution,
                        StepEnd {
                            index: 0,
                            at: None,
                            outcome: Conclusion::Success,
                            conclusion: Conclusion::Success,
                        },
                    )
                    .await,
                    "a finished step",
                ),
                (
                    progress::job_finished(world.db(), agent, execution, Conclusion::Success).await,
                    "a finished job",
                ),
            ] {
                assert_eq!(outcome.expect(applied), Progress::Applied, "{applied}");
            }
        }
    };
    let status = |key: &'static str| {
        let world = &world;
        async move { world.project(run).await.job(key).1.status }
    };

    finish("a").await;
    assert_eq!(
        (status("b").await, status("c").await, status("d").await),
        (JobStatus::Queued, JobStatus::Queued, JobStatus::Pending)
    );
    finish("b").await;
    assert_eq!(status("d").await, JobStatus::Pending);
    finish("c").await;
    assert_eq!(status("d").await, JobStatus::Queued);
    finish("d").await;

    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    let run_row = projection.run.expect("the run");
    assert_eq!(
        (
            run_row.status,
            run_row.conclusion,
            run_row.started,
            run_row.completed
        ),
        (
            RunStatus::Completed,
            Some(RunConclusion::Success),
            true,
            true
        )
    );
}

#[tokio::test]
async fn a_failing_step_fails_its_job_skips_its_dependents_and_fails_the_run() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 2),
            template("test", &["build"], 1),
            template("deploy", &["test"], 1),
        ]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    let agent = world.agent();
    progress::accepted(world.db(), agent, execution)
        .await
        .expect("an acceptance");
    progress::step_started(world.db(), agent, execution, 0, None)
        .await
        .expect("a started step");
    progress::step_finished(
        world.db(),
        agent,
        execution,
        StepEnd {
            index: 0,
            at: None,
            outcome: Conclusion::Failure,
            conclusion: Conclusion::Failure,
        },
    )
    .await
    .expect("a finished step");
    progress::job_finished(world.db(), agent, execution, Conclusion::Failure)
        .await
        .expect("a finished job");

    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    assert_eq!(
        ["build", "test", "deploy"].map(|key| {
            let job = projection.job(key).1;
            (job.status, job.conclusion)
        }),
        [
            (JobStatus::Completed, Some(Conclusion::Failure)),
            (JobStatus::Completed, Some(Conclusion::Skipped)),
            (JobStatus::Completed, Some(Conclusion::Skipped)),
        ]
    );
    let steps: Vec<_> = projection
        .steps_of(execution)
        .into_iter()
        .map(|step| (step.name.as_str(), step.status, step.conclusion))
        .collect();
    assert_eq!(
        steps,
        [
            (
                "Run echo build 0",
                StepStatus::Completed,
                Some(Conclusion::Failure)
            ),
            (
                "Run echo build 1",
                StepStatus::Completed,
                Some(Conclusion::Skipped)
            ),
        ]
    );
    let skips: Vec<_> = world
        .journal(run)
        .await
        .into_iter()
        .filter_map(|event| match event.event {
            Event::JobStatusChanged(changed) if changed.conclusion == Some(Conclusion::Skipped) => {
                Some((changed.from, changed.reason))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        skips,
        [
            (JobStatus::Pending, Some(StatusReason::IfFalse)),
            (JobStatus::Pending, Some(StatusReason::IfFalse)),
        ]
    );
    let run_row = projection.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}
