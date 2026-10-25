use bosun_db::runs::NewRun;
use bosun_types::event::Event;
use bosun_types::ir::{Action, ActionReference, Evaluable, JobKind, RunsOn, Template};
use bosun_types::status::{
    Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason, StepStatus,
};
use sea_orm::{ConnectionTrait, Statement, TransactionTrait};

use super::{World, operator, replay, template, workflow};
use crate::agents::progress::{self, Progress, StepEnd};
use crate::planner;

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
        group: Some(Template::text("default")),
        labels: vec![Template::text("Shell")],
    });
    let mut nowhere = template("nowhere", &[], 1);
    nowhere.runs_on = Evaluable::Literal(RunsOn {
        group: Some(Template::text("nowhere")),
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
async fn a_step_that_uses_an_action_is_named_after_the_action() {
    let world = World::new().await;
    let mut build = template("build", &[], 2);
    let JobKind::Steps(steps) = &mut build.kind;
    steps[1].action = Action::Uses(ActionReference {
        owner: "actions".to_owned(),
        repository: "cache".to_owned(),
        path: Some("restore".to_owned()),
        r#ref: "v4".to_owned(),
    });
    let run = world.dispatch(&workflow(vec![build])).await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    progress::accepted(world.db(), world.agent(), execution)
        .await
        .expect("an acceptance");
    let projection = world.project(run).await;
    let names: Vec<&str> = projection
        .steps_of(execution)
        .into_iter()
        .map(|step| step.name.as_str())
        .collect();
    assert_eq!(names, ["Run echo build 0", "Run actions/cache/restore@v4"]);
}

#[tokio::test]
async fn steps_are_named_from_the_text_around_their_expressions() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - name: Build ${{ github.ref_name }}
        run: make
      - run: echo ${{ github.sha }}
      - name: ${{ github.ref_name }}
        run: |
          ${{ github.workspace }}
          make check
";
    let ir = bosun_workflow::parse("ci", source)
        .expect("a valid workflow")
        .workflow;
    let run = world.dispatch(&ir).await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    progress::accepted(world.db(), world.agent(), execution)
        .await
        .expect("an acceptance");
    let projection = world.project(run).await;
    let names: Vec<&str> = projection
        .steps_of(execution)
        .into_iter()
        .map(|step| step.name.as_str())
        .collect();
    assert_eq!(names, ["Build", "Run echo", "Run make check"]);
}

#[tokio::test]
async fn a_job_placed_by_an_expression_is_never_queued_without_it() {
    let world = World::new().await;
    for runs_on in [
        "[self-hosted, '${{ vars.ARCH }}']",
        "{ group: '${{ vars.POOL }}', labels: [linux] }",
        "${{ vars.LABEL }}",
    ] {
        let source = format!(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: {runs_on}\n    steps:\n      \
             - run: make\n"
        );
        let ir = bosun_workflow::parse("ci", &source)
            .expect("a valid workflow")
            .workflow;
        assert_eq!(
            planner::unapplied(&ir).as_deref(),
            Some(
                "an expression in `runs-on` of job `build`, which the planner applies from M1 \
                 step 15"
            ),
            "{runs_on}"
        );
        let transaction = world.db().begin().await.expect("a transaction");
        let refused = planner::dispatch(
            &transaction,
            NewRun {
                workflow: &world.workflow,
                revision_id: world.workflow.revision_id.expect("a current revision"),
                ir: &ir,
                actor: &operator(),
            },
        )
        .await
        .map(|run| run.id);
        assert_eq!(
            refused.map_err(|error| error.to_string()),
            Err(
                "a row of run holds a value bosun-types does not accept: the IR of the run \
                 places job `build` by an expression in `runs-on`, which the planner evaluates \
                 from M1 step 15"
                    .to_owned()
            ),
            "{runs_on}"
        );
    }
    assert_eq!(
        strings(&world, "SELECT count(*)::text AS value FROM bosun.job").await,
        ["0"]
    );
}

#[test]
fn keys_the_planner_does_not_apply_yet_are_named_with_the_step_that_brings_them() {
    let job = |keys: &str| {
        let source = format!(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n{keys}    \
             steps:\n      - run: make\n"
        );
        let ir = bosun_workflow::parse("ci", &source)
            .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
            .workflow;
        planner::unapplied(&ir)
    };
    let cases = [
        ("    if: false\n", "`if` of job `build`", 15),
        (
            "    continue-on-error: true\n",
            "`continue-on-error` of job `build`",
            15,
        ),
        (
            "    strategy:\n      matrix:\n        os: [linux]\n",
            "`strategy` of job `build`",
            16,
        ),
        (
            "    concurrency: deploy\n",
            "`concurrency` of job `build`",
            17,
        ),
    ];
    for (keys, what, step) in cases {
        assert_eq!(
            job(keys),
            Some(format!(
                "{what}, which the planner applies from M1 step {step}"
            )),
            "{keys}"
        );
    }
    assert_eq!(
        job(
            "    continue-on-error: false\n    timeout-minutes: 10\n    env:\n      A: ${{ github.sha }}\n"
        ),
        None
    );
    let uses = bosun_workflow::parse(
        "ci",
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      \
         - run: make\n      - uses: actions/checkout@v4\n",
    )
    .expect("a valid workflow")
    .workflow;
    assert_eq!(
        planner::unapplied(&uses).as_deref(),
        Some("a `uses` step of job `build`, which the planner applies from M1 step 33")
    );
    let workflow = bosun_workflow::parse(
        "ci",
        "on: workflow_dispatch\nconcurrency: ci\njobs:\n  build:\n    runs-on: self-hosted\n    \
         steps:\n      - run: make\n",
    )
    .expect("a valid workflow")
    .workflow;
    assert_eq!(
        planner::unapplied(&workflow).as_deref(),
        Some("`concurrency` of the workflow, which the planner applies from M1 step 17")
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
