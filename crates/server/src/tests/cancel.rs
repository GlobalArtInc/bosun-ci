use bosun_types::command::Target;
use bosun_types::error::ErrorCode;
use bosun_types::event::Event;
use bosun_types::id::{AgentId, JobId};
use bosun_types::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, RunConclusion, RunStatus,
    StatusReason, StepStatus,
};
use sea_orm::TransactionTrait;
use sea_orm::prelude::Uuid;

use super::{World, finished, job_cancel, operator, replay, run_cancel, template, workflow};
use crate::agents::progress::{self, Progress, Reporter};
use crate::commands::{self, Applied};
use crate::site::Site;

#[tokio::test]
async fn a_cancelled_run_completes_every_job_and_tells_the_agent_to_stop_the_running_one() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 2),
            template("test", &["build"], 1),
            template("lint", &[], 1),
        ]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    let agent = world.agent();
    progress::accepted(world.db(), agent, execution, &Site::default())
        .await
        .expect("an acceptance");
    progress::step_started(world.db(), agent, execution, 0, None)
        .await
        .expect("a started step");

    let Applied::Accepted(accepted) = world.command(run, &run_cancel()).await else {
        panic!("run.cancel was refused");
    };
    assert_eq!(accepted.event.kind(), "command.accepted");
    assert_eq!((accepted.attempt_id, accepted.job_id), (None, None));
    let projection = world.project(run).await;
    for key in ["test", "lint"] {
        let job = projection.job(key).1;
        assert_eq!(
            (job.status, job.conclusion, job.source),
            (
                JobStatus::Completed,
                Some(Conclusion::Cancelled),
                Some(ConclusionSource::Natural)
            ),
            "{key}"
        );
    }
    assert_eq!(projection.job("build").1.status, JobStatus::InProgress);
    assert_eq!(
        world.controls().await.into_iter().collect::<Vec<_>>(),
        [execution]
    );
    let run_row = projection.run.as_ref().expect("the run");
    assert_eq!(
        (run_row.status, run_row.cancel_requested),
        (RunStatus::InProgress, true)
    );
    let reasons: Vec<_> = world
        .journal(run)
        .await
        .into_iter()
        .filter(|event| event.seq > accepted.seq)
        .filter_map(|event| match event.event {
            Event::JobStatusChanged(changed) => Some((changed.from, changed.reason)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        [
            (JobStatus::Pending, Some(StatusReason::Command)),
            (JobStatus::Queued, Some(StatusReason::Command)),
        ]
    );

    assert_eq!(
        progress::job_finished(
            world.db(),
            agent,
            execution,
            finished(Conclusion::Cancelled),
            &Site::default()
        )
        .await
        .expect("a finished job"),
        Progress::Applied
    );
    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    assert!(
        projection
            .jobs
            .values()
            .all(|job| job.status == JobStatus::Completed),
        "{projection:#?}"
    );
    assert_eq!(
        projection
            .steps_of(execution)
            .into_iter()
            .map(|step| (step.status, step.conclusion))
            .collect::<Vec<_>>(),
        [
            (StepStatus::Completed, Some(Conclusion::Cancelled)),
            (StepStatus::Completed, Some(Conclusion::Skipped)),
        ]
    );
    let run_row = projection.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );

    let refused = world.command(run, &run_cancel()).await;
    let Applied::Refused(message) = refused else {
        panic!("run.cancel of a completed run was accepted: {refused:?}");
    };
    assert_eq!(
        message,
        "the run is completed; `run.cancel` needs a run that is not completed"
    );
    let last = world.journal(run).await.pop().expect("an event");
    assert_eq!(last.event.kind(), "command.refused");
}

#[tokio::test]
async fn a_run_cancelled_while_its_last_job_finishes_well_still_ends_cancelled() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    progress::accepted(world.db(), world.agent(), execution, &Site::default())
        .await
        .expect("an acceptance");
    world.command(run, &run_cancel()).await;
    progress::job_finished(
        world.db(),
        world.agent(),
        execution,
        finished(Conclusion::Success),
        &Site::default(),
    )
    .await
    .expect("a finished job");
    let run_row = world.project(run).await.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
}

#[tokio::test]
async fn a_cancelled_job_skips_its_dependents_and_a_completed_job_refuses_the_command() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 1),
            template("test", &["build"], 1),
        ]))
        .await;
    let (build, _) = world.project(run).await.job("build");

    let Applied::Accepted(accepted) = world.command(run, &job_cancel(build)).await else {
        panic!("job.cancel was refused");
    };
    assert_eq!(accepted.job_id, Some(build));
    assert!(accepted.attempt_id.is_some());
    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    let (build_row, test_row) = (projection.job("build").1, projection.job("test").1);
    assert_eq!(
        (build_row.conclusion, test_row.conclusion),
        (Some(Conclusion::Cancelled), Some(Conclusion::Skipped))
    );
    let run_row = projection.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion, run_row.cancel_requested),
        (RunStatus::Completed, Some(RunConclusion::Cancelled), false)
    );

    let refused = world.command(run, &job_cancel(build)).await;
    let Applied::Refused(message) = refused else {
        panic!("job.cancel of a completed job was accepted: {refused:?}");
    };
    assert_eq!(
        message,
        "job `build` is completed; `job.cancel` needs a job that is not completed"
    );
    let last = world.journal(run).await.pop().expect("an event");
    assert_eq!(
        (last.event.kind(), last.job_id),
        ("command.refused", Some(build))
    );
}

#[tokio::test]
async fn an_assigned_job_is_cancelled_at_once_and_its_late_acceptance_ignored() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;

    world.command(run, &job_cancel(build)).await;
    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    let execution_row = &projection.executions[&execution];
    assert_eq!(
        (
            execution_row.status,
            execution_row.conclusion,
            execution_row.reason
        ),
        (
            ExecutionStatus::Completed,
            Some(Conclusion::Cancelled),
            Some(StatusReason::Command)
        )
    );
    assert_eq!(
        world.controls().await.into_iter().collect::<Vec<_>>(),
        [execution]
    );
    assert!(matches!(
        progress::accepted(world.db(), world.agent(), execution, &Site::default())
            .await
            .expect("an answer"),
        Progress::Ignored(_)
    ));
    assert_eq!(world.project(run).await, projection);
}

#[tokio::test]
async fn an_execution_whose_agent_is_lost_fails_its_job_and_the_planner_takes_the_run_on() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 1),
            template("test", &["build"], 1),
        ]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    progress::accepted(world.db(), world.agent(), execution, &Site::default())
        .await
        .expect("an acceptance");

    world.set_online(true).await;
    assert!(
        !progress::lose_after_grace(world.db(), execution, &Site::default())
            .await
            .expect("a loss"),
        "the execution of an online agent was lost"
    );
    world.set_online(false).await;
    assert!(
        progress::lose_after_grace(world.db(), execution, &Site::default())
            .await
            .expect("a loss")
    );
    assert!(
        !progress::lose_after_grace(world.db(), execution, &Site::default())
            .await
            .expect("a loss"),
        "a completed execution was lost twice"
    );

    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    let build_row = projection.job("build").1;
    assert_eq!(build_row.conclusion, Some(Conclusion::Failure));
    assert_eq!(
        projection.executions[&execution].reason,
        Some(StatusReason::AgentLost)
    );
    assert_eq!(
        projection.job("test").1.conclusion,
        Some(Conclusion::Skipped)
    );
    let run_row = projection.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}

#[tokio::test]
async fn removing_its_agent_fails_a_running_job_and_returns_an_assigned_one_to_the_queue() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 1),
            template("lint", &[], 1),
            template("test", &["build"], 1),
        ]))
        .await;
    let projection = world.project(run).await;
    let (build, _) = projection.job("build");
    let (lint, _) = projection.job("lint");
    let running = world.start(run, build).await;
    let assigned = world.assign(run, lint).await;
    let agent = world.agent().id;
    let stranger = AgentId::try_from(Uuid::now_v7()).expect("an agent id");
    assert!(
        !progress::remove(world.db(), stranger, running, &operator(), &Site::default())
            .await
            .expect("a removal"),
        "the execution of another agent was removed"
    );

    for execution in [running, assigned] {
        assert!(
            progress::remove(world.db(), agent, execution, &operator(), &Site::default())
                .await
                .expect("a removal")
        );
        assert!(
            !progress::remove(world.db(), agent, execution, &operator(), &Site::default())
                .await
                .expect("a removal"),
            "a completed execution was removed twice"
        );
    }

    let projection = world.project(run).await;
    assert_eq!(replay(&world.journal(run).await), Ok(projection.clone()));
    for (execution, conclusion) in [
        (running, Conclusion::Failure),
        (assigned, Conclusion::Cancelled),
    ] {
        let row = &projection.executions[&execution];
        assert_eq!(
            (row.status, row.conclusion, row.reason),
            (
                ExecutionStatus::Completed,
                Some(conclusion),
                Some(StatusReason::AgentRemoved)
            )
        );
    }
    assert_eq!(
        world.controls().await.into_iter().collect::<Vec<_>>(),
        [running, assigned]
    );
    let build_row = projection.job("build").1;
    assert_eq!(
        (build_row.status, build_row.conclusion),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    let lint_row = projection.job("lint").1;
    assert_eq!(
        (lint_row.status, lint_row.queued),
        (JobStatus::Queued, true)
    );
    assert_eq!(
        projection.job("test").1.conclusion,
        Some(Conclusion::Skipped)
    );
    let removed: Vec<(&str, String)> = world
        .journal(run)
        .await
        .iter()
        .filter_map(|event| match &event.event {
            Event::ExecutionStatusChanged(changed)
                if changed.reason == Some(StatusReason::AgentRemoved) =>
            {
                Some(("execution", format!("{:?}", changed.to)))
            }
            Event::JobStatusChanged(changed)
                if changed.reason == Some(StatusReason::AgentRemoved) =>
            {
                Some(("job", format!("{:?}", changed.to)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        removed,
        [
            ("execution", "Completed".to_owned()),
            ("job", "Completed".to_owned()),
            ("execution", "Completed".to_owned()),
            ("job", "Queued".to_owned()),
        ]
    );
}

#[tokio::test]
async fn reports_that_do_not_fit_the_execution_change_nothing() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    let stranger = Reporter {
        id: AgentId::try_from(Uuid::now_v7()).expect("an agent id"),
        name: "runner9-9".to_owned(),
    };
    let before = world.project(run).await;

    for outcome in [
        progress::accepted(world.db(), &stranger, execution, &Site::default()).await,
        progress::step_started(world.db(), world.agent(), execution, 0, None).await,
        progress::job_finished(
            world.db(),
            world.agent(),
            execution,
            finished(Conclusion::Success),
            &Site::default(),
        )
        .await,
    ] {
        assert!(
            matches!(outcome.expect("an answer"), Progress::Ignored(_)),
            "a report that does not fit was applied"
        );
    }
    progress::accepted(world.db(), world.agent(), execution, &Site::default())
        .await
        .expect("an acceptance");
    for outcome in [
        progress::step_started(world.db(), world.agent(), execution, 3, None).await,
        progress::accepted(world.db(), world.agent(), execution, &Site::default()).await,
    ] {
        assert!(
            matches!(outcome.expect("an answer"), Progress::Ignored(_)),
            "a report that does not fit was applied"
        );
    }
    assert_ne!(world.project(run).await, before);
    assert_eq!(
        replay(&world.journal(run).await),
        Ok(world.project(run).await)
    );
}

#[tokio::test]
async fn a_command_with_a_target_it_does_not_take_is_invalid_input_and_not_recorded() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let before = world.journal(run).await;
    let other = JobId::try_from(Uuid::now_v7()).expect("a job id");

    for (request, field) in [
        (job_cancel(other), "target.job"),
        (
            {
                let mut request = job_cancel(build);
                request.target.job = None;
                request
            },
            "target.job",
        ),
        (
            {
                let mut request = run_cancel();
                request.target.job = Some(build);
                request
            },
            "target.job",
        ),
        (
            {
                let mut request = run_cancel();
                request.target = Target {
                    attempt: Some(2),
                    ..Target::default()
                };
                request
            },
            "target.attempt",
        ),
    ] {
        let transaction = world.db().begin().await.expect("a transaction");
        let mut locked = bosun_db::runs::lock(&transaction, run)
            .await
            .expect("a lock")
            .expect("the run");
        let refused = commands::apply(&mut locked, &operator(), &request, &Site::default())
            .await
            .err()
            .unwrap_or_else(|| panic!("{request:?} was applied"));
        assert_eq!(refused.0.fields.len(), 1, "{:?}", refused.0);
        assert_eq!(refused.0.fields[0].field, field);
        assert_eq!(refused.0.code, ErrorCode::InvalidInput);
    }
    assert_eq!(world.journal(run).await, before);
}
