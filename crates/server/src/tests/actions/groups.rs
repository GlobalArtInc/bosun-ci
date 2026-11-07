use std::time::Duration;

use bosun_db::resolutions::Claimed;
use bosun_types::command::{Command, CommandRequest, Target};
use bosun_types::event::{Arrival, ConcurrencyCancelled, Event};
use bosun_types::id::{JobId, RunId};
use bosun_types::status::{Conclusion, JobStatus, StatusReason, WaitingReason};

use super::{CHECKOUT, Harness, LEASE, Transition, quick, resolving};
use crate::config::Resolution;
use crate::tests::{finished, replay};

const QUEUED: Transition = (JobStatus::Waiting, JobStatus::Queued, None, None);

const CANCELLED: Transition = (
    JobStatus::Waiting,
    JobStatus::Completed,
    None,
    Some(StatusReason::Concurrency),
);

const SINGLE: &str = "      group: deploy\n";

const MAX: &str = "      group: deploy\n      queue: max\n";

const CANCELLING: &str = "      group: deploy\n      cancel-in-progress: true\n";

const MAKE: &str = "      - run: make\n";

fn entered(reason: WaitingReason) -> Transition {
    (JobStatus::Pending, JobStatus::Waiting, Some(reason), None)
}

fn moved(reason: WaitingReason) -> Transition {
    (JobStatus::Waiting, JobStatus::Waiting, Some(reason), None)
}

fn grouped(concurrency: &str, steps: &str) -> String {
    format!(
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    concurrency:\n\
         {concurrency}    steps:\n{steps}"
    )
}

fn using(tag: &str) -> String {
    format!("      - uses: actions/checkout@{tag}\n")
}

fn stocked(harness: &Harness) {
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    for tag in ["v1", "v2", "v3"] {
        harness.tag("actions/checkout", tag, &commit);
    }
}

fn asked(harness: &Harness, tag: &str) -> usize {
    let suffix = format!("/commits/{tag}");
    harness
        .forge
        .requests()
        .iter()
        .filter(|request| request.path.ends_with(&suffix))
        .count()
}

async fn due(harness: &Harness) -> Claimed {
    for _ in 0..200 {
        let mut taken = bosun_db::resolutions::claim(harness.db(), 10, LEASE)
            .await
            .expect("a claim")
            .claimed;
        if let Some(claimed) = taken.pop() {
            return claimed;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no job became due");
}

async fn consistent(harness: &Harness, run: RunId) {
    assert_eq!(
        replay(&harness.world.journal(run).await),
        Ok(harness.world.project(run).await)
    );
}

async fn cancellations(harness: &Harness, run: RunId) -> Vec<(Option<JobId>, Event)> {
    harness
        .world
        .journal(run)
        .await
        .into_iter()
        .filter(|event| matches!(event.event, Event::ConcurrencyCancelled(_)))
        .map(|event| (event.job_id, event.event))
        .collect()
}

#[tokio::test]
async fn an_arrival_with_cancel_in_progress_cancels_a_member_still_resolving_and_its_late_answer_is_dropped()
 {
    let harness = Harness::new().await;
    stocked(&harness);
    harness.forge.fail(true);
    let first = harness
        .dispatch_source(&grouped(CANCELLING, &using("v1")))
        .await;
    harness.resolver.once().await;
    assert_eq!(
        harness
            .forge
            .requests()
            .iter()
            .filter(|request| request.status == 502)
            .count(),
        1
    );
    harness.forge.fail(false);
    let late = due(&harness).await;

    let second = harness
        .dispatch_source(&grouped(CANCELLING, &using("v2")))
        .await;
    assert_eq!(
        harness.transitions(first, "build").await,
        [entered(WaitingReason::Resolving), CANCELLED]
    );
    let (arriving, _) = harness.world.project(second).await.job("build");
    let (resolving, _) = harness.world.project(first).await.job("build");
    assert_eq!(
        cancellations(&harness, first).await,
        [(
            Some(resolving),
            Event::ConcurrencyCancelled(ConcurrencyCancelled {
                group: "deploy".to_owned(),
                arrival: Arrival {
                    run_id: second,
                    job_id: Some(arriving),
                },
            })
        )]
    );
    assert_eq!(
        harness.transitions(second, "build").await,
        [
            entered(WaitingReason::Concurrency),
            moved(WaitingReason::Resolving)
        ]
    );

    harness.resolver.resolve(late).await;
    assert_eq!(harness.transitions(first, "build").await.len(), 2);
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(second, "build").await.last(),
        Some(&QUEUED)
    );
    assert_eq!((asked(&harness, "v1"), asked(&harness, "v2")), (1, 1));
    consistent(&harness, first).await;
    consistent(&harness, second).await;
}

#[tokio::test]
async fn under_queue_max_the_member_behind_is_not_asked_until_the_one_ahead_has_completed() {
    let harness = Harness::new().await;
    stocked(&harness);
    let first = harness.dispatch_source(&grouped(MAX, &using("v1"))).await;
    let second = harness.dispatch_source(&grouped(MAX, &using("v2"))).await;
    assert_eq!(
        harness.waiting(first, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert_eq!(
        harness.waiting(second, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );

    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(harness.status(first, "build").await, JobStatus::Queued);
    assert_eq!(harness.resolver.once().await, 0);
    assert_eq!((asked(&harness, "v1"), asked(&harness, "v2")), (1, 0));
    assert_eq!(
        harness.waiting(second, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );

    harness
        .world
        .finish(first, "build", finished(Conclusion::Success))
        .await;
    assert_eq!(
        harness.waiting(second, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert_eq!(asked(&harness, "v2"), 0);
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(asked(&harness, "v2"), 1);
    assert_eq!(harness.status(second, "build").await, JobStatus::Queued);
}

#[tokio::test]
async fn under_queue_single_a_displaced_member_is_cancelled_without_a_request_to_the_forge() {
    let harness = Harness::new().await;
    stocked(&harness);
    harness
        .dispatch_source(&grouped(SINGLE, &using("v1")))
        .await;
    assert_eq!(harness.resolver.once().await, 1);
    let second = harness
        .dispatch_source(&grouped(SINGLE, &using("v2")))
        .await;
    let third = harness
        .dispatch_source(&grouped(SINGLE, &using("v3")))
        .await;

    assert_eq!(
        harness.transitions(second, "build").await,
        [entered(WaitingReason::Concurrency), CANCELLED]
    );
    let (arriving, _) = harness.world.project(third).await.job("build");
    let (displaced, _) = harness.world.project(second).await.job("build");
    assert_eq!(
        cancellations(&harness, second).await,
        [(
            Some(displaced),
            Event::ConcurrencyCancelled(ConcurrencyCancelled {
                group: "deploy".to_owned(),
                arrival: Arrival {
                    run_id: third,
                    job_id: Some(arriving),
                },
            })
        )]
    );
    assert_eq!(
        harness.waiting(third, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );
    assert_eq!(harness.resolver.once().await, 0);
    assert_eq!(
        (
            asked(&harness, "v1"),
            asked(&harness, "v2"),
            asked(&harness, "v3")
        ),
        (1, 0, 0)
    );
}

#[tokio::test]
async fn the_next_member_goes_from_concurrency_to_resolving_to_queued_when_the_holder_completes() {
    let harness = Harness::new().await;
    stocked(&harness);
    let holder = harness.dispatch_source(&grouped(MAX, MAKE)).await;
    let next = harness.dispatch_source(&grouped(MAX, &using("v1"))).await;
    assert_eq!(harness.status(holder, "build").await, JobStatus::Queued);
    assert_eq!(
        harness.waiting(next, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );

    harness
        .world
        .finish(holder, "build", finished(Conclusion::Success))
        .await;
    assert_eq!(
        harness.waiting(next, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert!(harness.forge.requests().is_empty());
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(next, "build").await,
        [
            entered(WaitingReason::Concurrency),
            moved(WaitingReason::Resolving),
            QUEUED
        ]
    );
    consistent(&harness, next).await;
}

#[tokio::test]
async fn a_holder_that_cannot_resolve_gives_its_group_to_the_next_member() {
    let harness = Harness::new().await;
    stocked(&harness);
    let holder = harness.dispatch_source(&grouped(MAX, &using("v9"))).await;
    let next = harness.dispatch_source(&grouped(MAX, &using("v1"))).await;

    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(holder, "build").await,
        [
            entered(WaitingReason::Resolving),
            (
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::ActionUnresolved)
            )
        ]
    );
    assert_eq!(
        harness.transitions(next, "build").await,
        [
            entered(WaitingReason::Concurrency),
            moved(WaitingReason::Resolving)
        ]
    );
    assert_eq!(asked(&harness, "v1"), 0);
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(harness.status(next, "build").await, JobStatus::Queued);
    assert_eq!(asked(&harness, "v1"), 1);
}

#[tokio::test]
async fn a_job_without_remote_actions_is_queued_at_once_with_a_group_and_without() {
    let harness = Harness::new().await;
    let alone = harness
        .dispatch_source(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      - run: make\n",
        )
        .await;
    let member = harness.dispatch_source(&grouped(SINGLE, MAKE)).await;
    for run in [alone, member] {
        assert_eq!(
            harness.transitions(run, "build").await,
            [(JobStatus::Pending, JobStatus::Queued, None, None)]
        );
    }
    assert_eq!(harness.resolver.once().await, 0);
    assert!(harness.forge.requests().is_empty());
}

#[tokio::test]
async fn resolving_counts_against_max_parallel_and_a_member_goes_from_max_parallel_through_its_group_to_resolving()
 {
    let harness = Harness::new().await;
    stocked(&harness);
    let holder = harness.dispatch_source(&grouped(MAX, MAKE)).await;
    let matrix = harness
        .dispatch_source(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    strategy:\n      \
             max-parallel: 1\n      matrix:\n        shard: [1, 2]\n    concurrency:\n      \
             group: deploy\n      queue: max\n    steps:\n      - uses: actions/checkout@v1\n",
        )
        .await;
    let behind = harness.dispatch_source(&grouped(MAX, MAKE)).await;
    let members: Vec<JobId> = harness
        .world
        .project(matrix)
        .await
        .jobs
        .iter()
        .filter(|(_, job)| job.group.is_some())
        .map(|(id, _)| *id)
        .collect();
    let [first, second] = members[..] else {
        panic!("a matrix of two members expanded to {members:?}");
    };
    let state = |job: JobId| {
        let harness = &harness;
        async move {
            let projection = harness.world.project(matrix).await;
            let row = &projection.jobs[&job];
            (row.status, row.waiting_reason)
        }
    };
    assert_eq!(
        state(first).await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );
    assert_eq!(
        state(second).await,
        (JobStatus::Waiting, Some(WaitingReason::MaxParallel))
    );

    harness
        .world
        .finish(holder, "build", finished(Conclusion::Success))
        .await;
    assert_eq!(
        state(first).await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert_eq!(
        state(second).await,
        (JobStatus::Waiting, Some(WaitingReason::MaxParallel))
    );
    assert!(harness.forge.requests().is_empty());

    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(state(first).await, (JobStatus::Queued, None));
    assert_eq!(
        state(second).await,
        (JobStatus::Waiting, Some(WaitingReason::MaxParallel))
    );

    harness
        .world
        .finish_job(matrix, first, finished(Conclusion::Success))
        .await;
    assert_eq!(harness.status(behind, "build").await, JobStatus::Queued);
    assert_eq!(
        state(second).await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );
    harness
        .world
        .finish(behind, "build", finished(Conclusion::Success))
        .await;
    assert_eq!(
        state(second).await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(state(second).await, (JobStatus::Queued, None));
    assert_eq!(
        harness.moves(matrix, second).await,
        [
            entered(WaitingReason::MaxParallel),
            moved(WaitingReason::Concurrency),
            moved(WaitingReason::Resolving),
            QUEUED
        ]
    );
}

#[tokio::test]
async fn a_member_of_a_fork_run_goes_from_untrusted_through_its_group_to_resolving_to_queued() {
    let harness = Harness::new().await;
    stocked(&harness);
    let holder = harness.dispatch_source(&grouped(MAX, MAKE)).await;
    let run = harness
        .world
        .dispatch_from_fork(&super::workflow(&grouped(MAX, &using("v1"))), false)
        .await;
    assert_eq!(
        harness.waiting(run, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Untrusted))
    );

    let admit = CommandRequest {
        command: Command::RunAdmit,
        target: Target::default(),
        reason: None,
    };
    harness.world.command(run, &admit).await;
    assert_eq!(
        harness.waiting(run, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Concurrency))
    );

    harness
        .world
        .finish(holder, "build", finished(Conclusion::Success))
        .await;
    assert_eq!(
        harness.waiting(run, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert!(harness.forge.requests().is_empty());
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(run, "build").await,
        [
            entered(WaitingReason::Untrusted),
            moved(WaitingReason::Concurrency),
            moved(WaitingReason::Resolving),
            QUEUED
        ]
    );
    consistent(&harness, run).await;
}

#[tokio::test]
async fn a_holder_that_gives_up_on_a_forge_that_does_not_answer_gives_its_group_to_the_next_member()
{
    let harness = Harness::new().await;
    stocked(&harness);
    harness.forge.fail(true);
    let holder = harness.dispatch_source(&grouped(MAX, &using("v1"))).await;
    let next = harness.dispatch_source(&grouped(MAX, &using("v2"))).await;
    let impatient = resolving(
        &harness.world,
        &harness.forge,
        Resolution {
            give_up: Duration::from_millis(1),
            ..quick()
        },
    );

    assert_eq!(impatient.once().await, 1);
    assert_eq!(
        harness.transitions(holder, "build").await,
        [
            entered(WaitingReason::Resolving),
            (
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::ActionUnresolved)
            )
        ]
    );
    assert_eq!(
        harness.transitions(next, "build").await,
        [
            entered(WaitingReason::Concurrency),
            moved(WaitingReason::Resolving)
        ]
    );

    harness.forge.fail(false);
    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(harness.status(next, "build").await, JobStatus::Queued);
    consistent(&harness, holder).await;
    consistent(&harness, next).await;
}
