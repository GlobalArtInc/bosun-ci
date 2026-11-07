use std::sync::Arc;
use std::time::Duration;

use bosun_types::action_policy::ActionPolicy;
use bosun_types::command::{Command, CommandRequest, Target};
use bosun_types::diagnostic::Severity;
use bosun_types::id::RunId;
use bosun_types::status::{
    Conclusion, JobStatus, RunConclusion, RunStatus, StatusReason, WaitingReason,
};
use tokio::sync::Notify;

use super::{CHECKOUT, Harness, at, quick, resolving, workflow};
use crate::config::Resolution;
use crate::tests::{replay, run_cancel};

const ONE_STEP: &str = "      - uses: actions/checkout@v4\n";

fn commits(harness: &Harness) -> usize {
    harness
        .forge
        .requests()
        .iter()
        .filter(|request| request.path.contains("/commits/"))
        .count()
}

async fn until(harness: &Harness, run: RunId, wanted: JobStatus) {
    for _ in 0..200 {
        harness.resolver.once().await;
        if harness.status(run, "build").await == wanted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the job of run {run} did not become {wanted:?}");
}

#[tokio::test]
async fn a_job_using_a_forbidden_action_goes_from_resolving_to_failure_and_never_reaches_the_queue()
{
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let patterns = vec!["acme/*".parse().expect("a pattern")];
    harness.set_policy(&ActionPolicy::Listed { patterns }).await;
    let run = harness
        .dispatch_unresolved("      - run: make\n      - uses: actions/checkout@v4\n")
        .await;
    let projection = harness.world.project(run).await;
    let job = projection.job("build").1;
    assert_eq!(
        (job.status, job.waiting_reason),
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );

    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(run, "build").await,
        [
            (
                JobStatus::Pending,
                JobStatus::Waiting,
                Some(WaitingReason::Resolving),
                None
            ),
            (
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::ActionForbidden)
            ),
        ]
    );
    let projection = harness.world.project(run).await;
    let job = projection.job("build").1;
    assert_eq!(
        (job.status, job.conclusion, job.queued),
        (JobStatus::Completed, Some(Conclusion::Failure), false)
    );
    let run_row = projection.run.as_ref().expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
    let notices = harness.notices(run).await;
    assert_eq!(notices.len(), 1, "{notices:#?}");
    let notice = &notices[0].diagnostic;
    assert_eq!(
        (notice.severity, notice.code.as_str(), &notice.path),
        (Severity::Error, "action_forbidden", &at(1))
    );
    assert!(
        notice.message.contains("`actions/checkout@v4`") && notice.message.contains("acme/*"),
        "{}",
        notice.message
    );
    assert!(
        harness.forge.requests().is_empty(),
        "the forge was asked about a forbidden action: {:#?}",
        harness.forge.requests()
    );
    assert_eq!(
        replay(&harness.world.journal(run).await),
        Ok(harness.world.project(run).await)
    );
}

#[tokio::test]
async fn an_allowed_action_resolves_when_the_job_is_ready_and_a_tag_moved_afterwards_changes_nothing()
 {
    let harness = Harness::new().await;
    let first = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &first);
    let run = harness.dispatch(ONE_STEP).await;
    assert_eq!(
        harness.transitions(run, "build").await,
        [
            (
                JobStatus::Pending,
                JobStatus::Waiting,
                Some(WaitingReason::Resolving),
                None
            ),
            (JobStatus::Waiting, JobStatus::Queued, None, None),
        ]
    );
    assert_eq!(commits(&harness), 1);

    let moved = harness.commit(
        "actions/checkout",
        &[("action.yml", &CHECKOUT.replace("Checkout", "Moved"))],
    );
    harness.tag("actions/checkout", "v4", &moved);
    let runner = harness.agent("runner1-1").await;
    let (_, documents) = harness.assigned(&runner).await;
    assert_eq!(documents.actions[0].commit, first);
    assert_eq!(
        documents.actions[0].metadata.name.as_deref(),
        Some("Checkout")
    );
    assert_eq!(commits(&harness), 1, "the assignment asked the forge again");
}

#[tokio::test]
async fn a_forge_that_is_stopped_keeps_the_job_resolving_until_it_returns_and_fails_it_beyond_the_limit()
 {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);

    harness.forge.fail(true);
    let run = harness.dispatch(ONE_STEP).await;
    for _ in 0..200 {
        if harness.attempts(run, "build").await >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        harness.resolver.once().await;
    }
    let attempts = harness.attempts(run, "build").await;
    assert!(attempts >= 3, "the job was asked {attempts} times");
    assert_eq!(
        harness.transitions(run, "build").await,
        [(
            JobStatus::Pending,
            JobStatus::Waiting,
            Some(WaitingReason::Resolving),
            None
        )]
    );
    assert_eq!(harness.status(run, "build").await, JobStatus::Waiting);

    harness.forge.fail(false);
    until(&harness, run, JobStatus::Queued).await;
    assert_eq!(
        harness.transitions(run, "build").await.last(),
        Some(&(JobStatus::Waiting, JobStatus::Queued, None, None))
    );

    harness.forge.fail(true);
    let impatient = resolving(
        &harness.world,
        &harness.forge,
        Resolution {
            give_up: Duration::from_millis(1),
            ..quick()
        },
    );
    let late = harness.dispatch_unresolved(ONE_STEP).await;
    for _ in 0..200 {
        impatient.once().await;
        if harness.status(late, "build").await == JobStatus::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        harness.transitions(late, "build").await.last(),
        Some(&(
            JobStatus::Waiting,
            JobStatus::Completed,
            None,
            Some(StatusReason::ActionUnresolved)
        ))
    );
    let notices = harness.notices(late).await;
    assert_eq!(notices.len(), 1, "{notices:#?}");
    assert_eq!(notices[0].diagnostic.code, "action_unresolved");
    assert!(
        notices[0]
            .diagnostic
            .message
            .contains("the forge answered 502"),
        "{}",
        notices[0].diagnostic.message
    );
}

#[tokio::test]
async fn while_the_forge_hangs_the_jobs_of_another_run_are_planned_and_queued() {
    let harness = Harness::configured(Resolution {
        request_timeout: Duration::from_millis(300),
        ..quick()
    })
    .await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    harness.forge.delay(Duration::from_secs(3600));
    let stuck = harness.dispatch_unresolved(ONE_STEP).await;
    let working = tokio::spawn(Arc::clone(&harness.resolver).work(Arc::new(Notify::new())));

    let other = harness.dispatch_unresolved("      - run: make\n").await;
    assert_eq!(harness.status(other, "build").await, JobStatus::Queued);
    let runner = harness.agent("runner1-1").await;
    assert_eq!(harness.claim(&runner).await.run_id, other);

    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(harness.status(stuck, "build").await, JobStatus::Waiting);
    harness.world.command(stuck, &run_cancel()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    working.abort();
    assert_eq!(
        harness.transitions(stuck, "build").await,
        [
            (
                JobStatus::Pending,
                JobStatus::Waiting,
                Some(WaitingReason::Resolving),
                None
            ),
            (
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::Command)
            ),
        ]
    );
}

#[tokio::test]
async fn a_ref_the_forge_does_not_have_fails_the_job_at_once() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let run = harness
        .dispatch("      - run: make\n      - uses: actions/checkout@v9\n")
        .await;
    assert_eq!(
        harness.transitions(run, "build").await,
        [
            (
                JobStatus::Pending,
                JobStatus::Waiting,
                Some(WaitingReason::Resolving),
                None
            ),
            (
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::ActionUnresolved)
            ),
        ]
    );
    assert_eq!(
        commits(&harness),
        1,
        "the missing ref was asked about again"
    );
    assert_eq!(harness.resolver.once().await, 0);
    let notices = harness.notices(run).await;
    assert_eq!(notices.len(), 1, "{notices:#?}");
    let notice = &notices[0].diagnostic;
    assert_eq!(
        (notice.severity, notice.code.as_str(), &notice.path),
        (Severity::Error, "action_unresolved", &at(1))
    );
    assert!(
        notice
            .message
            .starts_with("the action `actions/checkout@v9` cannot be resolved"),
        "{}",
        notice.message
    );
}

#[tokio::test]
async fn two_resolvers_that_claim_at_once_resolve_every_job_exactly_once() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    let mut runs = Vec::new();
    for number in 1..=6 {
        harness.tag("actions/checkout", &format!("v{number}"), &commit);
        runs.push(
            harness
                .dispatch_unresolved(&format!("      - uses: actions/checkout@v{number}\n"))
                .await,
        );
    }
    let other = resolving(&harness.world, &harness.forge, quick());
    let (one, two) = tokio::join!(harness.resolver.once(), other.once());
    assert_eq!(one + two, runs.len());
    assert_eq!(commits(&harness), runs.len());
    for run in runs {
        assert_eq!(harness.status(run, "build").await, JobStatus::Queued);
    }
}

#[tokio::test]
async fn an_answer_that_arrives_after_a_cancellation_is_dropped() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let run = harness.dispatch_unresolved(ONE_STEP).await;

    let claimed = bosun_db::resolutions::claim(harness.db(), 10, Duration::from_secs(30))
        .await
        .expect("a claim")
        .claimed;
    assert_eq!(claimed.len(), 1);
    harness.world.command(run, &run_cancel()).await;
    harness.resolver.resolve(claimed[0].clone()).await;
    assert_eq!(
        harness.transitions(run, "build").await,
        [
            (
                JobStatus::Pending,
                JobStatus::Waiting,
                Some(WaitingReason::Resolving),
                None
            ),
            (
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::Command)
            ),
        ]
    );
}

#[tokio::test]
async fn a_replica_whose_lease_ran_out_cannot_decide_the_job_another_replica_took() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let run = harness.dispatch_unresolved(ONE_STEP).await;

    let lost = bosun_db::resolutions::claim(harness.db(), 10, Duration::from_millis(1))
        .await
        .expect("a claim")
        .claimed;
    assert_eq!(lost.len(), 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let taken = bosun_db::resolutions::claim(harness.db(), 10, Duration::from_secs(30))
        .await
        .expect("a claim")
        .claimed;
    assert_eq!(taken.len(), 1);
    assert_ne!(lost[0].token, taken[0].token);

    harness.resolver.resolve(lost[0].clone()).await;
    assert_eq!(harness.status(run, "build").await, JobStatus::Waiting);
    harness.resolver.resolve(taken[0].clone()).await;
    assert_eq!(harness.status(run, "build").await, JobStatus::Queued);
}

#[tokio::test]
async fn a_run_from_a_fork_is_not_resolved_before_a_maintainer_admits_it() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let source = format!(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n{ONE_STEP}"
    );
    let run = harness
        .world
        .dispatch_from_fork(&workflow(&source), false)
        .await;
    assert_eq!(
        harness.waiting(run, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Untrusted))
    );
    assert_eq!(harness.resolver.once().await, 0);
    assert!(
        harness.forge.requests().is_empty(),
        "the forge heard of a run nobody admitted: {:#?}",
        harness.forge.requests()
    );

    let admit = CommandRequest {
        command: Command::RunAdmit,
        target: Target::default(),
        reason: None,
    };
    harness.world.command(run, &admit).await;
    assert_eq!(
        harness.waiting(run, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert!(harness.forge.requests().is_empty());
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(run, "build").await,
        [
            (
                JobStatus::Pending,
                JobStatus::Waiting,
                Some(WaitingReason::Untrusted),
                None
            ),
            (
                JobStatus::Waiting,
                JobStatus::Waiting,
                Some(WaitingReason::Resolving),
                None
            ),
            (JobStatus::Waiting, JobStatus::Queued, None, None),
        ]
    );
    assert_eq!(commits(&harness), 1);
}

#[tokio::test]
async fn a_forbidden_action_fails_the_job_and_the_run_whatever_its_continue_on_error() {
    let harness = Harness::new().await;
    let patterns = vec!["acme/*".parse().expect("a pattern")];
    harness.set_policy(&ActionPolicy::Listed { patterns }).await;
    let run = harness
        .dispatch_source(&format!(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    \
             continue-on-error: true\n    steps:\n{ONE_STEP}  deploy:\n    runs-on: self-hosted\n    \
             needs: build\n    steps:\n      - run: make\n"
        ))
        .await;
    assert_eq!(harness.resolver.once().await, 1);

    let projection = harness.world.project(run).await;
    let build = projection.job("build").1;
    assert_eq!(
        (build.status, build.conclusion),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    let deploy = projection.job("deploy").1;
    assert_eq!(
        (deploy.status, deploy.conclusion),
        (JobStatus::Completed, Some(Conclusion::Skipped))
    );
    let run_row = projection.run.as_ref().expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
    assert_eq!(
        replay(&harness.world.journal(run).await),
        Ok(harness.world.project(run).await)
    );
}
