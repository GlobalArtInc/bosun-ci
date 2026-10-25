use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use bosun_db::entity::job;
use bosun_db::runs::Evaluated;
use bosun_types::api::Notice;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::event::Event;
use bosun_types::ir::{self, Shell, Span};
use bosun_types::status::{Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason};
use sea_orm::EntityTrait;

use super::{World, finished, run_cancel};
use crate::agents::progress::JobEnd;
use crate::commands::Applied;

fn parse(source: &str) -> ir::Workflow {
    bosun_workflow::parse("ci", source)
        .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
        .workflow
}

fn path(segments: &[&str]) -> Vec<PathSegment> {
    segments
        .iter()
        .map(|segment| PathSegment::Key((*segment).to_owned()))
        .collect()
}

async fn states(
    world: &World,
    run: bosun_types::id::RunId,
    keys: &[&str],
) -> Vec<(JobStatus, Option<Conclusion>)> {
    let projection = world.project(run).await;
    keys.iter()
        .map(|key| {
            let job = projection.job(key).1;
            (job.status, job.conclusion)
        })
        .collect()
}

async fn reason(world: &World, run: bosun_types::id::RunId, key: &str) -> Option<StatusReason> {
    let (job, _) = world.project(run).await.job(key);
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

async fn stored(world: &World, id: bosun_types::id::JobId) -> job::Model {
    job::Entity::find_by_id(id.as_uuid())
        .one(world.db())
        .await
        .expect("a job")
        .expect("the job")
}

const STATUSES: &str = "\
on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
  test:
    needs: build
    runs-on: self-hosted
    steps:
      - run: make test
  report:
    needs: test
    if: failure()
    runs-on: self-hosted
    steps:
      - run: ./report
  notify:
    needs: build
    if: ${{ failure() }}
    runs-on: self-hosted
    steps:
      - run: ./notify
  cleanup:
    needs: test
    if: always()
    runs-on: self-hosted
    steps:
      - run: ./cleanup
  publish:
    needs: build
    if: github.ref == 'refs/heads/main'
    runs-on: self-hosted
    steps:
      - run: ./publish
";

const KEYS: [&str; 6] = ["build", "test", "report", "notify", "cleanup", "publish"];

#[tokio::test]
async fn failure_runs_after_any_failed_ancestor_and_always_after_anything() {
    let world = World::new().await;
    let failing = world.dispatch(&parse(STATUSES)).await;
    world
        .finish(failing, "build", finished(Conclusion::Failure))
        .await;
    let skipped = (JobStatus::Completed, Some(Conclusion::Skipped));
    let queued = (JobStatus::Queued, None);
    assert_eq!(
        states(&world, failing, &KEYS).await,
        [
            (JobStatus::Completed, Some(Conclusion::Failure)),
            skipped,
            queued,
            queued,
            queued,
            skipped,
        ]
    );
    assert_eq!(
        reason(&world, failing, "publish").await,
        Some(StatusReason::IfFalse)
    );
    assert_eq!(
        reason(&world, failing, "test").await,
        Some(StatusReason::IfFalse)
    );

    let passing = world.dispatch(&parse(STATUSES)).await;
    world
        .finish(passing, "build", finished(Conclusion::Success))
        .await;
    world
        .finish(passing, "test", finished(Conclusion::Success))
        .await;
    assert_eq!(
        states(&world, passing, &KEYS).await,
        [
            (JobStatus::Completed, Some(Conclusion::Success)),
            (JobStatus::Completed, Some(Conclusion::Success)),
            skipped,
            skipped,
            queued,
            skipped,
        ]
    );
}

#[tokio::test]
async fn a_job_that_fails_under_continue_on_error_counts_as_succeeded() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  flaky:
    runs-on: self-hosted
    continue-on-error: ${{ github.event_name == 'workflow_dispatch' }}
    steps:
      - run: make flaky
  after:
    needs: flaky
    if: needs.flaky.result == 'success'
    runs-on: self-hosted
    steps:
      - run: make
";
    let run = world.dispatch(&parse(source)).await;
    world
        .finish(run, "flaky", finished(Conclusion::Failure))
        .await;
    assert_eq!(
        states(&world, run, &["flaky", "after"]).await,
        [
            (JobStatus::Completed, Some(Conclusion::Failure)),
            (JobStatus::Queued, None)
        ]
    );
    world
        .finish(run, "after", finished(Conclusion::Success))
        .await;
    let run_row = world.project(run).await.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Success))
    );
}

#[tokio::test]
async fn after_run_cancel_only_the_pending_jobs_whose_if_admits_it_run() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
  always:
    needs: build
    if: always()
    runs-on: self-hosted
    steps:
      - run: ./always
  cancelled:
    needs: build
    if: cancelled()
    runs-on: self-hosted
    steps:
      - run: ./cancelled
  test:
    needs: build
    runs-on: self-hosted
    steps:
      - run: make test
  lint:
    runs-on: self-hosted
    steps:
      - run: make lint
  report:
    needs: [build, lint]
    if: failure()
    runs-on: self-hosted
    steps:
      - run: ./report
";
    let keys = ["build", "always", "cancelled", "test", "report"];
    let run = world.dispatch(&parse(source)).await;
    world
        .finish(run, "lint", finished(Conclusion::Failure))
        .await;
    let (build, _) = world.project(run).await.job("build");
    let execution = world.assign(run, build).await;
    crate::agents::progress::accepted(world.db(), world.agent(), execution, &super::site())
        .await
        .expect("an acceptance");
    let Applied::Accepted(_) = world.command(run, &run_cancel()).await else {
        panic!("run.cancel was refused");
    };
    let cancelled = (JobStatus::Completed, Some(Conclusion::Cancelled));
    let pending = (JobStatus::Pending, None);
    assert_eq!(
        states(&world, run, &keys).await,
        [
            (JobStatus::InProgress, None),
            pending,
            pending,
            cancelled,
            pending
        ]
    );
    crate::agents::progress::job_finished(
        world.db(),
        world.agent(),
        execution,
        finished(Conclusion::Cancelled),
        &super::site(),
    )
    .await
    .expect("a finished job");
    let queued = (JobStatus::Queued, None);
    assert_eq!(
        states(&world, run, &keys).await,
        [cancelled, queued, queued, cancelled, cancelled]
    );
    assert_eq!(
        reason(&world, run, "report").await,
        Some(StatusReason::Command)
    );
    world
        .finish(run, "always", finished(Conclusion::Success))
        .await;
    world
        .finish(run, "cancelled", finished(Conclusion::Success))
        .await;
    let run_row = world.project(run).await.run.expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}

#[tokio::test]
async fn an_evaluation_error_fails_the_job_and_becomes_an_error_notice_at_the_expression() {
    let world = World::new().await;
    let source = "\
on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    continue-on-error: true
    timeout-minutes: ${{ fromJSON('x') }}
    steps:
      - run: make
  test:
    needs: build
    runs-on: self-hosted
    steps:
      - run: make test
";
    let run = world.dispatch(&parse(source)).await;
    assert_eq!(
        states(&world, run, &["build", "test"]).await,
        [
            (JobStatus::Completed, Some(Conclusion::Failure)),
            (JobStatus::Completed, Some(Conclusion::Skipped))
        ]
    );
    assert_eq!(
        reason(&world, run, "build").await,
        Some(StatusReason::EvaluationFailed)
    );
    let (build, _) = world.project(run).await.job("build");
    let shown = world.run(run).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
    let [notice] = shown.notices.as_slice() else {
        panic!("the notices are {:#?}", shown.notices);
    };
    assert_eq!(notice.job_id, Some(build));
    let diagnostic = &notice.diagnostic;
    assert_eq!(
        (
            diagnostic.severity,
            diagnostic.code.as_str(),
            diagnostic.file.as_str(),
            diagnostic.span,
            &diagnostic.path
        ),
        (
            Severity::Error,
            "function_failed",
            "ci",
            Span {
                line: 6,
                column: 26,
                end_line: 6,
                end_column: 39
            },
            &path(&["jobs", "build", "timeout-minutes"])
        )
    );
    assert!(
        diagnostic.message.starts_with(
            "`timeout-minutes` of job `build`: the argument of `fromJSON` is not JSON"
        ),
        "{}",
        diagnostic.message
    );
    let order: Vec<&str> = world
        .journal(run)
        .await
        .iter()
        .filter(|event| event.job_id == Some(build))
        .map(|event| event.event.kind())
        .collect();
    assert_eq!(order, ["job.created", "run.notice", "job.status"]);
}

#[tokio::test]
async fn the_job_takes_the_values_the_planner_evaluated_from_its_needs_and_the_run() {
    let world = World::new().await;
    let source = "\
name: Release
on: workflow_dispatch
jobs:
  setup:
    runs-on: self-hosted
    outputs:
      label: ${{ steps.pick.outputs.label }}
    steps:
      - id: pick
        run: echo label=gpu >> \"$GITHUB_OUTPUT\"
  build:
    needs: setup
    name: Build on ${{ needs.setup.outputs.label }}
    runs-on: ${{ needs.setup.outputs.label }}
    timeout-minutes: ${{ fromJSON('15') }}
    continue-on-error: ${{ needs.setup.result == 'success' }}
    env:
      PROJECT: ${{ bosun.project }}
      RUN: ${{ bosun.run_url }}
      WORKFLOW: ${{ github.workflow }} ${{ github.run_number }} ${{ github.job }} ${{ toJSON(vars) }}
    defaults:
      run:
        shell: ${{ 'bash' }}
        working-directory: ${{ env.PROJECT }}/${{ inputs.missing }}
    steps:
      - run: make
";
    let run = world.dispatch(&parse(source)).await;
    world
        .finish(
            run,
            "setup",
            JobEnd {
                conclusion: Conclusion::Success,
                outputs: BTreeMap::from([
                    ("label".to_owned(), "GPU".to_owned()),
                    ("stray".to_owned(), "x".to_owned()),
                ]),
            },
        )
        .await;
    let projection = world.project(run).await;
    let (setup, _) = projection.job("setup");
    let (build, row) = projection.job("build");
    assert_eq!(row.status, JobStatus::Queued);
    assert_eq!(
        stored(&world, setup).await.outputs,
        serde_json::json!({ "label": "GPU" })
    );
    let build = stored(&world, build).await;
    assert_eq!(
        (
            build.name.as_str(),
            build.labels.as_slice(),
            build.continue_on_error
        ),
        ("Build on GPU", ["gpu".to_owned()].as_slice(), true)
    );
    let evaluated: Evaluated =
        serde_json::from_value(build.evaluated.expect("the evaluated values")).expect("values");
    assert_eq!(
        evaluated,
        Evaluated {
            timeout_minutes: 15,
            env: BTreeMap::from([
                ("PROJECT".to_owned(), "globalart/keel".to_owned()),
                (
                    "RUN".to_owned(),
                    format!("https://ci.example.org/runs/{run}")
                ),
                ("WORKFLOW".to_owned(), "Release 1 build {}".to_owned()),
            ]),
            shell: Some(Shell::Bash),
            working_directory: Some("globalart/keel/".to_owned()),
            deferred: false,
        }
    );
}

#[tokio::test]
async fn run_name_names_the_run_and_its_failure_keeps_the_default_name() {
    let world = World::new().await;
    let named = world
        .dispatch(&parse(
            "run-name: ${{ github.event_name }} by ${{ github.actor }}\non: workflow_dispatch\n\
             jobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      - run: make\n",
        ))
        .await;
    let shown = world.run(named).await;
    assert_eq!(shown.display_name, "workflow_dispatch by roman");
    assert_eq!(shown.notices, []);

    let broken = world
        .dispatch(&parse(
            "name: Nightly\nrun-name: Nightly ${{ fromJSON('x') }}\non: workflow_dispatch\n\
             jobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      - run: make\n",
        ))
        .await;
    let shown = world.run(broken).await;
    assert_eq!(shown.display_name, "Nightly");
    assert_eq!(shown.status, RunStatus::Queued);
    let [notice] = shown.notices.as_slice() else {
        panic!("the notices are {:#?}", shown.notices);
    };
    assert_eq!(
        (
            notice.job_id,
            notice.diagnostic.code.as_str(),
            notice.diagnostic.span,
            &notice.diagnostic.path
        ),
        (
            None,
            "function_failed",
            Span {
                line: 2,
                column: 23,
                end_line: 2,
                end_column: 36
            },
            &path(&["run-name"])
        )
    );
    assert_eq!(
        world.project(broken).await.job("build").1.status,
        JobStatus::Queued
    );
}

#[tokio::test]
async fn the_warnings_of_the_revision_are_the_first_notices_of_the_run() {
    let world = World::new().await;
    let warning = Diagnostic {
        severity: Severity::Warning,
        code: "template_condition".to_owned(),
        message: "a template".to_owned(),
        file: "ci".to_owned(),
        span: Span {
            line: 8,
            column: 13,
            end_line: 8,
            end_column: 53,
        },
        path: path(&["jobs", "build", "steps"]),
    };
    let run = world
        .dispatch_with(
            &parse(
                "run-name: ${{ fromJSON('x') }}\non: workflow_dispatch\njobs:\n  build:\n    \
                 runs-on: self-hosted\n    steps:\n      - run: make\n",
            ),
            std::slice::from_ref(&warning),
        )
        .await;
    let notices = world.run(run).await.notices;
    assert_eq!(notices.len(), 2, "{notices:#?}");
    assert_eq!(
        notices[0],
        Notice {
            job_id: None,
            diagnostic: warning
        }
    );
    assert_eq!(notices[1].diagnostic.severity, Severity::Error);
}

fn runtime_codes() -> BTreeSet<String> {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/04-runs.md");
    let text = fs::read_to_string(&document)
        .unwrap_or_else(|error| panic!("{}: {error}", document.display()));
    let marker = "The codes of runtime notices:";
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("no `{marker}` in docs/design/04-runs.md"));
    lines[start..]
        .iter()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|row| {
            let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
            assert_eq!(cells.get(1), Some(&"error"), "{row}");
            cells[0].trim_matches('`').to_owned()
        })
        .collect()
}

#[tokio::test]
async fn every_runtime_notice_code_is_in_the_table_of_the_design() {
    let world = World::new().await;
    let job = |keys: &str| {
        format!("on: workflow_dispatch\njobs:\n  build:\n{keys}    steps:\n      - run: make\n")
    };
    let cases = [
        (
            job("    runs-on: self-hosted\n    timeout-minutes: ${{ fromJSON('x') }}\n"),
            "function_failed",
        ),
        (
            job("    runs-on: self-hosted\n    continue-on-error: ${{ 'yes' }}\n"),
            "invalid_type",
        ),
        (
            job("    runs-on: self-hosted\n    timeout-minutes: ${{ fromJSON('1.5') }}\n"),
            "invalid_value",
        ),
        (job("    runs-on: ${{ fromJSON('[1]') }}\n"), "invalid_type"),
        (
            job(&format!(
                "    runs-on: self-hosted\n    strategy:\n      matrix:\n        a: [{}]\n        \
                 b: [{}]\n",
                (0..16)
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                (0..17)
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            "limit_exceeded",
        ),
        (job("    runs-on: ${{ '' }}\n"), "invalid_value"),
        (
            job(
                "    runs-on: self-hosted\n    defaults:\n      run:\n        shell: ${{ 'zsh' }}\n",
            ),
            "invalid_value",
        ),
        (
            "run-name: ${{ fromJSON('x') }}\non: workflow_dispatch\njobs:\n  build:\n    \
             runs-on: self-hosted\n    steps:\n      - run: make\n"
                .to_owned(),
            "function_failed",
        ),
    ];
    let mut produced = BTreeSet::new();
    for (source, code) in cases {
        let run = world.dispatch(&parse(&source)).await;
        let codes: Vec<String> = world
            .run(run)
            .await
            .notices
            .into_iter()
            .map(|notice| notice.diagnostic.code)
            .collect();
        assert_eq!(codes, [code], "{source}");
        produced.extend(codes);
    }
    produced.extend(super::secrets::unreadable_secret_notices().await);
    assert_eq!(produced, runtime_codes());
}

#[tokio::test]
async fn a_job_takes_each_setting_of_defaults_run_it_does_not_set_from_the_workflow() {
    let world = World::new().await;
    let run = world
        .dispatch(&parse(
            "on: workflow_dispatch
defaults:
  run:
    shell: sh
    working-directory: crates
jobs:
  inherits:
    runs-on: self-hosted
    steps:
      - run: make
  overrides:
    runs-on: self-hosted
    defaults:
      run:
        shell: ${{ 'bash' }}
    steps:
      - run: make
",
        ))
        .await;
    let projection = world.project(run).await;
    let mut found = Vec::new();
    for key in ["inherits", "overrides"] {
        let (id, row) = projection.job(key);
        assert_eq!(row.status, JobStatus::Queued, "{key}");
        let evaluated: Evaluated =
            serde_json::from_value(stored(&world, id).await.evaluated.expect("values"))
                .expect("values");
        found.push((evaluated.shell, evaluated.working_directory));
    }
    assert_eq!(
        found,
        [
            (Some(Shell::Sh), Some("crates".to_owned())),
            (Some(Shell::Bash), Some("crates".to_owned())),
        ]
    );
}
