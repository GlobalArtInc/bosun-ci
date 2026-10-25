use std::collections::BTreeSet;
use std::time::Duration;

use bosun_db::agents::{Connection, NewAgent, NewRegistrationToken, Registration};
use bosun_db::notify::{Listener, Notification};
use bosun_db::pools::{Access, NewPool};
use bosun_db::queue::{Claim, Claimed, ClaimedJob};
use bosun_db::runs::Evaluated as Stored;
use bosun_proto::agent::v1::Assignment;
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::assignment::{Documents, Evaluated};
use bosun_types::actor::Actor;
use bosun_types::api::{Capability, TrustLevel};
use bosun_types::diagnostic::{PathSegment, Severity};
use bosun_types::event::{Event, JobStatusChanged, RunEvent};
use bosun_types::id::{AgentId, JobId, PoolId};
use bosun_types::ir::{self, Evaluable, JobTemplate, RunsOn, Template};
use bosun_types::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, RunConclusion, RunStatus,
    StatusReason,
};
use chrono::{TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use tokio::time::timeout;

use super::{World, finished, keys, replay, site, template, workflow};
use crate::agents::progress::{self, Progress, Reporter};
use crate::queue;
use crate::site::Site;

pub(super) const LEASE: Duration = Duration::from_secs(30);

pub(super) const LABELS: [&str; 4] = ["self-hosted", "linux", "x64", "shell"];

pub(super) fn runs_on(mut job: JobTemplate, group: Option<&str>, labels: &[&str]) -> JobTemplate {
    job.runs_on = Evaluable::Literal(RunsOn {
        group: group.map(Template::text),
        labels: labels.iter().copied().map(Template::text).collect(),
    });
    job
}

pub(super) async fn connected(
    world: &World,
    name: &str,
    labels: &[&str],
    replica: Uuid,
) -> Reporter {
    let pool = bosun_db::pools::default_pool(world.db())
        .await
        .expect("the default pool");
    connected_in(world, pool.id, name, labels, &[Capability::Shell], replica).await
}

async fn connected_in(
    world: &World,
    pool: PoolId,
    name: &str,
    labels: &[&str],
    capabilities: &[Capability],
    replica: Uuid,
) -> Reporter {
    let db = world.db();
    let sha256 = Uuid::now_v7().as_bytes().repeat(2);
    bosun_db::agents::create_registration_token(
        db,
        NewRegistrationToken {
            pool_id: pool,
            sha256: sha256.clone(),
            reusable: false,
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
    )
    .await
    .expect("a registration token");
    let registered = bosun_db::agents::register(
        db,
        &sha256,
        Utc::now(),
        NewAgent {
            name: name.to_owned(),
            labels: Vec::new(),
            credential_sha256: Uuid::now_v7().as_bytes().repeat(2),
        },
    )
    .await
    .expect("a registration");
    let Registration::Registered(registered) = registered else {
        panic!("the agent was not registered");
    };
    bosun_db::agents::connect(
        db,
        registered.id,
        Connection {
            replica_id: replica,
            version: "0.1.0".to_owned(),
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            capabilities: capabilities.to_vec(),
        },
    )
    .await
    .expect("the agent connected");
    Reporter {
        id: registered.id,
        name: registered.name,
    }
}

pub(super) async fn claim<C: TransactionTrait>(
    db: &C,
    agent: AgentId,
    replica: Uuid,
    lease: Duration,
    passed_over: &[JobId],
) -> Option<ClaimedJob> {
    loop {
        let claimed = bosun_db::queue::claim(
            db,
            Claim {
                agent,
                replica_id: replica,
                lease,
                passed_over,
            },
        )
        .await
        .expect("a claim");
        match claimed {
            Claimed::Job(claimed) => return Some(*claimed),
            Claimed::Nothing => return None,
            Claimed::Contended => tokio::task::yield_now().await,
        }
    }
}

pub(super) async fn values(db: &DatabaseConnection, sql: &str) -> Vec<String> {
    db.query_all(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_iter()
        .map(|row| row.try_get::<String>("", "value").expect("a text value"))
        .collect()
}

async fn queued_at(db: &DatabaseConnection, job: JobId) -> Vec<String> {
    values(
        db,
        &format!("SELECT queued_at::text AS value FROM bosun.job WHERE id = '{job}'"),
    )
    .await
}

fn reasons(events: &[RunEvent]) -> Vec<(JobStatus, JobStatus, Option<StatusReason>)> {
    events
        .iter()
        .filter_map(|event| match &event.event {
            Event::JobStatusChanged(changed) => Some((changed.from, changed.to, changed.reason)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_claim_takes_the_first_queued_job_by_priority_then_age_that_the_agent_satisfies() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let older = world
        .dispatch(&workflow(vec![
            runs_on(template("gpu", &[], 1), None, &["self-hosted", "gpu"]),
            runs_on(
                template("elsewhere", &[], 1),
                Some("nowhere"),
                &["self-hosted"],
            ),
            runs_on(template("old", &[], 1), None, &["Self-Hosted", "Linux"]),
        ]))
        .await;
    let newer = world
        .dispatch(&workflow(vec![
            runs_on(template("new", &[], 1), Some("default"), &["linux"]),
            runs_on(template("urgent", &[], 1), None, &["self-hosted"]),
        ]))
        .await;
    let (urgent, _) = world.project(newer).await.job("urgent");
    db.execute_unprepared(&format!(
        "UPDATE bosun.job SET priority = 5 WHERE id = '{urgent}'"
    ))
    .await
    .expect("a higher priority");

    let mut taken = Vec::new();
    for number in 1..=4 {
        let agent = connected(&world, &format!("runner1-{number}"), &LABELS, replica).await;
        match claim(db, agent.id, replica, LEASE, &[]).await {
            Some(claimed) => taken.push(claimed.key.to_string()),
            None => taken.push("nothing".to_owned()),
        }
    }
    assert_eq!(taken, ["urgent", "old", "new", "nothing"]);

    let gpu = connected(&world, "runner2-1", &["self-hosted", "gpu"], replica).await;
    let claimed = claim(db, gpu.id, replica, LEASE, &[])
        .await
        .expect("the gpu job");
    assert_eq!(claimed.key.to_string(), "gpu");
    assert_eq!(claimed.run_id, older);
    let projection = world.project(older).await;
    assert_eq!(projection.job("elsewhere").1.status, JobStatus::Queued);
    for run in [older, newer] {
        assert_eq!(
            replay(&world.journal(run).await),
            Ok(world.project(run).await)
        );
    }
}

#[tokio::test]
async fn a_claim_assigns_the_job_with_a_lease_and_hands_over_what_its_assignment_needs() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let mut ir = workflow(vec![template("build", &[], 2)]);
    ir.env.insert("A".to_owned(), Template::text("workflow"));
    ir.env.insert("B".to_owned(), Template::text("workflow"));
    ir.jobs[0].env.insert("B".to_owned(), Template::text("job"));
    let run = world.dispatch(&ir).await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;

    assert!(
        claim(db, agent.id, Uuid::now_v7(), LEASE, &[])
            .await
            .is_none(),
        "a replica that does not hold the agent's stream claimed for it"
    );
    let (build, _) = world.project(run).await.job("build");
    assert!(
        claim(db, agent.id, replica, LEASE, &[build])
            .await
            .is_none(),
        "a job passed over was claimed"
    );
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    assert_eq!(claimed.run_id, run);
    assert_eq!(claimed.job_id, build);
    assert_eq!(
        (claimed.organisation.as_str(), claimed.project.as_str()),
        ("globalart", "keel")
    );
    assert_eq!(claimed.name, "build");
    assert_eq!(claimed.job, ir.jobs[0]);
    assert_eq!(claimed.env, ir.env);
    assert_eq!(
        (&claimed.evaluated, claimed.continue_on_error),
        (
            &Stored {
                timeout_minutes: 360,
                env: [("B".to_owned(), "job".to_owned())].into(),
                shell: None,
                working_directory: None,
                deferred: false,
            },
            false
        )
    );
    let (assignment, documents) = assigned(&world, claimed.clone()).await;
    assert_eq!(assignment.execution_id, claimed.execution_id.to_string());
    assert_eq!(documents.job, ir.jobs[0]);
    assert_eq!(
        documents.evaluated.env,
        [
            ("A".to_owned(), "workflow".to_owned()),
            ("B".to_owned(), "job".to_owned())
        ]
        .into()
    );
    let lease = values(
        db,
        &format!(
            "SELECT round(extract(epoch FROM lease_until - now()))::text AS value \
             FROM bosun.execution WHERE id = '{}'",
            claimed.execution_id
        ),
    )
    .await;
    let seconds: f64 = lease[0].parse().expect("seconds");
    assert!(
        (25.0..=30.0).contains(&seconds),
        "a lease of {seconds} seconds"
    );

    let projection = world.project(run).await;
    assert_eq!(projection.job("build").1.status, JobStatus::Assigned);
    let executions = projection.executions_of(build);
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].0, claimed.execution_id);
    assert_eq!(
        (executions[0].1.agent, executions[0].1.status),
        (agent.id, ExecutionStatus::Assigned)
    );
    assert_eq!(replay(&world.journal(run).await), Ok(projection));

    let second = world
        .dispatch(&workflow(vec![template("lint", &[], 1)]))
        .await;
    assert!(
        claim(db, agent.id, replica, LEASE, &[]).await.is_none(),
        "an agent with an assignment claimed a second job"
    );
    progress::accepted(db, &agent, claimed.execution_id, &Site::default())
        .await
        .expect("an acceptance");
    assert!(claim(db, agent.id, replica, LEASE, &[]).await.is_none());
    progress::job_finished(
        db,
        &agent,
        claimed.execution_id,
        finished(Conclusion::Success),
        &Site::default(),
    )
    .await
    .expect("a finished job");
    let next = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("the next job once the agent is free");
    assert_eq!(next.run_id, second);
}

pub(super) async fn assigned(world: &World, claimed: ClaimedJob) -> (Assignment, Documents) {
    let Some(Message::Assignment(assignment)) =
        queue::assignment(world.db(), claimed, &site(), &keys(), world.actions())
            .await
            .expect("an assignment")
            .expect("an offer")
            .message
            .message
    else {
        panic!("the offer of a claim is not an assignment");
    };
    let documents = Documents::read(&assignment).expect("readable documents");
    (assignment, documents)
}

pub(super) fn parse(source: &str) -> ir::Workflow {
    bosun_workflow::parse("ci", source)
        .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
        .workflow
}

#[tokio::test]
async fn the_workflow_env_is_evaluated_at_assignment_and_a_name_in_both_takes_the_job_value() {
    let world = World::new().await;
    let replica = Uuid::now_v7();
    let run = world
        .dispatch(&parse(
            "name: Release
on: workflow_dispatch
env:
  EVENT: ${{ github.event_name }} by ${{ github.actor }}
  SHARED: workflow ${{ github.job }}
  PROJECT: ${{ bosun.project }} ${{ toJSON(inputs) }} ${{ toJSON(vars) }}
jobs:
  build:
    runs-on: self-hosted
    timeout-minutes: 20
    continue-on-error: true
    env:
      SHARED: job ${{ github.job }}
    defaults:
      run:
        shell: sh
        working-directory: crates
    steps:
      - run: make
",
        ))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let claimed = claim(world.db(), agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    let (_, documents) = assigned(&world, claimed).await;
    assert_eq!(
        documents.evaluated,
        Evaluated {
            env: [
                ("EVENT".to_owned(), "workflow_dispatch by roman".to_owned()),
                ("PROJECT".to_owned(), "globalart/keel {} {}".to_owned()),
                ("SHARED".to_owned(), "job build".to_owned()),
            ]
            .into(),
            shell: Some(ir::Shell::Sh),
            working_directory: Some("crates".to_owned()),
            timeout_minutes: 20,
            continue_on_error: true,
        }
    );
    assert_eq!(documents.contexts.github["job"], "build");
    assert_eq!(documents.contexts.github["workflow"], "Release");
    assert_eq!(documents.contexts.bosun["run_id"], run.to_string());
    assert_eq!(
        documents.contexts.bosun["run_url"],
        format!("https://ci.example.org/runs/{run}")
    );
    assert_eq!(
        replay(&world.journal(run).await),
        Ok(world.project(run).await)
    );
}

#[tokio::test]
async fn an_evaluation_error_in_the_workflow_env_fails_the_job_at_its_assignment() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let run = world
        .dispatch(&parse(
            "on: workflow_dispatch
env:
  BROKEN: ${{ fromJSON('x') }}
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
",
        ))
        .await;
    world
        .dispatch(&workflow(vec![template("lint", &[], 1)]))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    assert_eq!(claimed.run_id, run);
    let execution = claimed.execution_id;
    assert!(
        queue::assignment(db, claimed, &site(), &keys(), world.actions())
            .await
            .expect("a failed assignment")
            .is_none(),
        "a job whose workflow env fails to evaluate was offered"
    );

    let projection = world.project(run).await;
    let (build, row) = projection.job("build");
    assert_eq!(
        (row.status, row.conclusion),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    let executions = projection.executions_of(build);
    assert_eq!(
        (executions[0].0, executions[0].1.status),
        (execution, ExecutionStatus::Completed)
    );
    assert_eq!(
        projection.job("test").1.conclusion,
        Some(Conclusion::Skipped)
    );
    let journal = world.journal(run).await;
    assert_eq!(
        reasons(&journal)
            .into_iter()
            .filter(|(_, to, _)| *to == JobStatus::Completed)
            .collect::<Vec<_>>(),
        [
            (
                JobStatus::Assigned,
                JobStatus::Completed,
                Some(StatusReason::EvaluationFailed)
            ),
            (
                JobStatus::Pending,
                JobStatus::Completed,
                Some(StatusReason::IfFalse)
            ),
        ]
    );
    let shown = world.run(run).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
    let [notice] = shown.notices.as_slice() else {
        panic!("the run has the notices {:?}", shown.notices);
    };
    assert_eq!(notice.job_id, Some(build));
    let diagnostic = &notice.diagnostic;
    assert_eq!(
        (
            diagnostic.severity,
            diagnostic.code.as_str(),
            &diagnostic.path,
            diagnostic.span.line
        ),
        (
            Severity::Error,
            "function_failed",
            &vec![
                PathSegment::Key("env".to_owned()),
                PathSegment::Key("BROKEN".to_owned())
            ],
            3
        )
    );
    assert!(
        diagnostic
            .message
            .starts_with("`env.BROKEN` of the workflow: "),
        "{}",
        diagnostic.message
    );
    assert_eq!(replay(&journal), Ok(projection));

    let next = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("the agent is free for the next job");
    assert_ne!(next.run_id, run);
}

#[tokio::test]
async fn a_claim_that_waited_for_another_claim_of_its_agent_sees_the_execution_it_created() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 1),
            template("lint", &[], 1),
        ]))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;

    let first = db.begin().await.expect("a transaction");
    let held = claim(&first, agent.id, replica, LEASE, &[])
        .await
        .expect("the first claim");
    let waiting = {
        let db = db.clone();
        let agent = agent.id;
        tokio::spawn(async move {
            bosun_db::queue::claim(
                &db,
                Claim {
                    agent,
                    replica_id: replica,
                    lease: LEASE,
                    passed_over: &[],
                },
            )
            .await
        })
    };
    timeout(Duration::from_secs(30), async {
        while values(
            db,
            "SELECT count(*)::text AS value FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .await
            != ["1"]
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second claim waits for the agent's row");
    first.commit().await.expect("the first claim commits");

    assert_eq!(
        waiting
            .await
            .expect("the second claim task")
            .expect("the second claim"),
        Claimed::Nothing
    );
    assert_eq!(
        values(
            db,
            &format!(
                "SELECT id::text AS value FROM bosun.execution WHERE agent_id = '{}'",
                agent.id
            ),
        )
        .await,
        [held.execution_id.to_string()]
    );
    let projection = world.project(run).await;
    let other = if held.key.to_string() == "build" {
        "lint"
    } else {
        "build"
    };
    assert_eq!(projection.job(other).1.status, JobStatus::Queued);
    assert_eq!(replay(&world.journal(run).await), Ok(projection));
}

#[tokio::test]
async fn a_claim_that_finds_only_jobs_of_locked_runs_says_so_instead_of_waiting() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let claim = || {
        bosun_db::queue::claim(
            db,
            Claim {
                agent: agent.id,
                replica_id: replica,
                lease: LEASE,
                passed_over: &[],
            },
        )
    };

    let holder = db.begin().await.expect("a transaction");
    bosun_db::runs::lock(&holder, run)
        .await
        .expect("a lock")
        .expect("the run");
    assert_eq!(claim().await.expect("a claim"), Claimed::Contended);
    holder.rollback().await.expect("the lock is released");
    assert!(matches!(claim().await.expect("a claim"), Claimed::Job(_)));
    assert_eq!(claim().await.expect("a claim"), Claimed::Nothing);
}

#[tokio::test]
async fn fifty_agents_claiming_at_once_never_take_the_same_job() {
    let world = World::new().await;
    let replica = Uuid::now_v7();
    let jobs: Vec<String> = (0..30).map(|index| format!("j{index}")).collect();
    let run = world
        .dispatch(&workflow(
            jobs.iter().map(|key| template(key, &[], 1)).collect(),
        ))
        .await;
    let mut agents = Vec::new();
    for number in 0..50 {
        agents.push(connected(&world, &format!("runner{number}"), &LABELS, replica).await);
    }
    let wide = bosun_db::connect(&world.database.url(), 16)
        .await
        .expect("a wider pool");
    let claims: Vec<_> = agents
        .iter()
        .map(|agent| {
            let db = wide.clone();
            let agent = agent.id;
            tokio::spawn(async move { claim(&db, agent, replica, LEASE, &[]).await })
        })
        .collect();
    let mut claimed = Vec::new();
    for handle in claims {
        if let Some(found) = handle.await.expect("a claim task") {
            claimed.push(found.job_id);
        }
    }
    let distinct: BTreeSet<JobId> = claimed.iter().copied().collect();
    assert_eq!(claimed.len(), 30, "{claimed:?}");
    assert_eq!(distinct.len(), 30, "a job was claimed twice: {claimed:?}");
    let projection = world.project(run).await;
    assert!(
        projection
            .jobs
            .values()
            .all(|job| job.status == JobStatus::Assigned),
        "{projection:#?}"
    );
    assert_eq!(projection.executions.len(), 30);
    assert_eq!(replay(&world.journal(run).await), Ok(projection));
    wide.close().await.expect("the wider pool closes");
}

#[tokio::test]
async fn a_lapsed_assignment_returns_its_job_to_the_queue_and_tells_its_agent_to_cancel() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let leased = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    assert_eq!(
        queue::expire(db, leased.execution_id)
            .await
            .expect("an expiry"),
        None,
        "an assignment within its lease expired"
    );
    let first_queued = queued_at(db, leased.job_id).await;
    db.execute_unprepared(&format!(
        "UPDATE bosun.execution SET lease_until = now() - interval '1 second' WHERE id = '{}'",
        leased.execution_id
    ))
    .await
    .expect("a lapsed lease");
    let lapsed = bosun_db::queue::expired_leases(db)
        .await
        .expect("the expired leases")
        .expect("the lock of the lease expiry");
    assert_eq!(lapsed, [leased.execution_id]);

    assert_eq!(
        queue::expire(db, leased.execution_id)
            .await
            .expect("an expiry"),
        Some(leased.job_id)
    );
    assert_eq!(
        queue::expire(db, leased.execution_id)
            .await
            .expect("an expiry"),
        None
    );
    let projection = world.project(run).await;
    let (_, build) = projection.job("build");
    assert_eq!(build.status, JobStatus::Queued);
    let execution = &projection.executions[&leased.execution_id];
    assert_eq!(
        (execution.status, execution.conclusion, execution.reason),
        (
            ExecutionStatus::Completed,
            Some(Conclusion::Cancelled),
            Some(StatusReason::LeaseExpired)
        )
    );
    assert_eq!(queued_at(db, leased.job_id).await, first_queued);
    assert_eq!(
        world.controls().await.into_iter().collect::<Vec<_>>(),
        [leased.execution_id]
    );
    let journal = world.journal(run).await;
    assert_eq!(
        reasons(&journal).last(),
        Some(&(
            JobStatus::Assigned,
            JobStatus::Queued,
            Some(StatusReason::LeaseExpired)
        ))
    );
    assert_eq!(replay(&journal), Ok(projection));
    assert_eq!(
        progress::accepted(db, &agent, leased.execution_id, &Site::default())
            .await
            .expect("a late acceptance"),
        Progress::Ignored(format!(
            "execution {} is not waiting for its agent to accept it",
            leased.execution_id
        ))
    );

    let again = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("the job claimed again");
    assert_eq!(again.job_id, leased.job_id);
    assert_ne!(again.execution_id, leased.execution_id);
}

#[tokio::test]
async fn a_rejected_assignment_returns_its_job_to_the_queue() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let other = connected(&world, "runner2-1", &LABELS, replica).await;
    let offered = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");

    assert_eq!(
        progress::rejected(db, &other, offered.execution_id)
            .await
            .expect("a rejection"),
        Progress::Ignored(format!(
            "execution {} belongs to another agent",
            offered.execution_id
        ))
    );
    assert_eq!(
        progress::rejected(db, &agent, offered.execution_id)
            .await
            .expect("a rejection"),
        Progress::Applied
    );
    assert!(matches!(
        progress::rejected(db, &agent, offered.execution_id)
            .await
            .expect("a second rejection"),
        Progress::Ignored(_)
    ));
    let projection = world.project(run).await;
    assert_eq!(projection.job("build").1.status, JobStatus::Queued);
    let execution = &projection.executions[&offered.execution_id];
    assert_eq!(
        (execution.conclusion, execution.reason),
        (
            Some(Conclusion::Cancelled),
            Some(StatusReason::RejectedByAgent)
        )
    );
    assert!(world.controls().await.is_empty());
    let journal = world.journal(run).await;
    let rejected = journal.last().expect("an event");
    assert_eq!(
        rejected.actor,
        Actor::Agent {
            id: agent.id,
            name: agent.name.clone()
        }
    );
    assert_eq!(replay(&journal), Ok(projection));
}

#[tokio::test]
async fn a_job_queued_longer_than_max_queue_time_is_cancelled_with_no_agent() {
    let world = World::new().await;
    let db = world.db();
    let run = world
        .dispatch(&workflow(vec![
            runs_on(template("gpu", &[], 1), None, &["self-hosted", "gpu"]),
            template("after", &["gpu"], 1),
        ]))
        .await;
    let projection = world.project(run).await;
    let (gpu, _) = projection.job("gpu");
    let (after, _) = projection.job("after");
    assert!(
        !queue::cancel_unclaimed(db, gpu, Duration::from_secs(3600), &Site::default())
            .await
            .expect("a timeout"),
        "a job queued for less than max_queue_time was cancelled"
    );
    assert!(
        !queue::cancel_unclaimed(db, after, Duration::ZERO, &Site::default())
            .await
            .expect("a timeout"),
        "a pending job was cancelled as unclaimed"
    );
    assert_eq!(
        bosun_db::queue::overdue(db, Duration::ZERO)
            .await
            .expect("the overdue jobs")
            .expect("the lock of the queue timeout"),
        [gpu]
    );
    assert!(
        queue::cancel_unclaimed(db, gpu, Duration::ZERO, &Site::default())
            .await
            .expect("a timeout")
    );

    let projection = world.project(run).await;
    let (_, timed_out) = projection.job("gpu");
    assert_eq!(
        (timed_out.status, timed_out.conclusion, timed_out.source),
        (
            JobStatus::Completed,
            Some(Conclusion::Cancelled),
            Some(ConclusionSource::Natural)
        )
    );
    let (_, skipped) = projection.job("after");
    assert_eq!(skipped.conclusion, Some(Conclusion::Skipped));
    let run_row = projection.run.clone().expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
    let journal = world.journal(run).await;
    let cancelled = journal
        .iter()
        .find(|event| {
            event.job_id == Some(gpu)
                && matches!(&event.event, Event::JobStatusChanged(changed) if changed.to == JobStatus::Completed)
        })
        .expect("the cancellation of the job");
    assert_eq!(cancelled.actor, Actor::System);
    assert_eq!(
        cancelled.event,
        Event::JobStatusChanged(JobStatusChanged {
            from: JobStatus::Queued,
            to: JobStatus::Completed,
            waiting_reason: None,
            conclusion: Some(Conclusion::Cancelled),
            conclusion_source: Some(ConclusionSource::Natural),
            reason: Some(StatusReason::NoAgent),
        })
    );
    assert_eq!(replay(&journal), Ok(projection));
}

async fn announced(listener: &mut Listener) -> JobId {
    loop {
        let heard = timeout(Duration::from_secs(30), listener.next())
            .await
            .expect("a notification in time")
            .expect("a notification")
            .expect("a notification without a reconnect");
        if let Notification::Queued(job) = heard {
            return job;
        }
    }
}

#[tokio::test]
async fn a_job_that_becomes_queued_is_announced_on_the_queue_channel() {
    let world = World::new().await;
    let db = world.db();
    let mut listener = Listener::connect(&world.database.url(), Uuid::now_v7())
        .await
        .expect("a listener");
    let run = world
        .dispatch(&workflow(vec![
            template("build", &[], 1),
            template("test", &["build"], 1),
        ]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    assert_eq!(announced(&mut listener).await, build);

    let replica = Uuid::now_v7();
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let offered = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    progress::rejected(db, &agent, offered.execution_id)
        .await
        .expect("a rejection");
    assert_eq!(announced(&mut listener).await, build);
}

async fn pool(world: &World, name: &str, trust: TrustLevel, access: &[Access]) -> PoolId {
    bosun_db::pools::create(
        world.db(),
        NewPool {
            name,
            trust,
            access,
        },
    )
    .await
    .expect("a pool")
    .id
}

async fn execute(db: &DatabaseConnection, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

#[tokio::test]
async fn the_claim_checks_the_access_list_of_the_agents_pool_whatever_the_job_lists() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let project = bosun_db::projects::find(db, world.workflow.project_id)
        .await
        .expect("a project")
        .expect("the project of the world");
    let other = bosun_db::projects::create(db, project.organisation_id, "other", "Other")
        .await
        .expect("another project");
    let narrow = pool(
        &world,
        "narrow",
        TrustLevel::Trusted,
        &[Access::Project(other.organisation_id, other.id)],
    )
    .await;
    let agent = connected_in(
        &world,
        narrow,
        "runner9-1",
        &LABELS,
        &[Capability::Shell],
        replica,
    )
    .await;
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let (build, _) = world.project(run).await.job("build");
    execute(
        db,
        &format!(
            "UPDATE bosun.job SET pool_ids = pool_ids || '{narrow}'::uuid WHERE id = '{build}'"
        ),
    )
    .await;
    assert_eq!(
        claim(db, agent.id, replica, LEASE, &[]).await,
        None,
        "a pool closed to the project took its job"
    );
    execute(
        db,
        &format!(
            "INSERT INTO bosun.pool_access (id, pool_id, organisation_id) \
             VALUES ('{}', '{narrow}', '{}')",
            Uuid::now_v7(),
            project.organisation_id
        ),
    )
    .await;
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("the job of a project of an organisation the pool is open to");
    assert_eq!(claimed.job_id, build);
}

#[tokio::test]
async fn the_claim_offers_a_job_only_to_an_agent_that_holds_the_capabilities_it_needs() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let default = bosun_db::pools::default_pool(db)
        .await
        .expect("the default pool")
        .id;
    let run = world
        .dispatch(&workflow(vec![
            runs_on(template("container", &[], 1), None, &["self-hosted"]),
            runs_on(template("anything", &[], 1), None, &["self-hosted"]),
        ]))
        .await;
    let projection = world.project(run).await;
    let (container, _) = projection.job("container");
    let (anything, _) = projection.job("anything");
    execute(
        db,
        &format!("UPDATE bosun.job SET capabilities = '{{podman}}' WHERE id = '{container}'"),
    )
    .await;
    let bare = connected_in(&world, default, "runner0-1", &LABELS, &[], replica).await;
    assert_eq!(
        claim(db, bare.id, replica, LEASE, &[]).await,
        None,
        "an agent without a capability took a job"
    );
    let host = connected_in(
        &world,
        default,
        "runner1-1",
        &LABELS,
        &[Capability::Shell],
        replica,
    )
    .await;
    let taken = claim(db, host.id, replica, LEASE, &[])
        .await
        .expect("the job that needs either capability");
    assert_eq!(taken.job_id, anything);
    let other = connected_in(
        &world,
        default,
        "runner2-1",
        &LABELS,
        &[Capability::Shell],
        replica,
    )
    .await;
    assert_eq!(
        claim(db, other.id, replica, LEASE, &[]).await,
        None,
        "an agent without podman took the job that needs it"
    );
    let both = connected_in(
        &world,
        default,
        "runner3-1",
        &LABELS,
        &[Capability::Shell, Capability::Podman],
        replica,
    )
    .await;
    let taken = claim(db, both.id, replica, LEASE, &[])
        .await
        .expect("the job that needs podman");
    assert_eq!(taken.job_id, container);
}

#[tokio::test]
async fn the_claim_keeps_the_jobs_of_trusted_and_untrusted_runs_to_pools_of_their_trust() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let forks = pool(
        &world,
        "forks",
        TrustLevel::Untrusted,
        &[Access::AllProjects],
    )
    .await;
    let trusted_run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let untrusted_run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    execute(
        db,
        &format!("UPDATE bosun.run SET trusted = false WHERE id = '{untrusted_run}'"),
    )
    .await;
    let (trusted_job, _) = world.project(trusted_run).await.job("build");
    let (untrusted_job, _) = world.project(untrusted_run).await.job("build");
    execute(
        db,
        &format!("UPDATE bosun.job SET pool_ids = pool_ids || '{forks}'::uuid"),
    )
    .await;
    let outside = connected_in(
        &world,
        forks,
        "runner2-1",
        &LABELS,
        &[Capability::Podman],
        replica,
    )
    .await;
    let taken = claim(db, outside.id, replica, LEASE, &[])
        .await
        .expect("the job of the untrusted run");
    assert_eq!(taken.job_id, untrusted_job);
    let more = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    execute(
        db,
        &format!(
            "UPDATE bosun.run SET trusted = false WHERE id = '{more}'; \
             UPDATE bosun.job SET pool_ids = pool_ids || '{forks}'::uuid \
             WHERE NOT ('{forks}' = ANY (pool_ids))"
        ),
    )
    .await;
    let inside = connected(&world, "runner1-1", &LABELS, replica).await;
    let taken = claim(db, inside.id, replica, LEASE, &[])
        .await
        .expect("the job of the trusted run");
    assert_eq!(taken.job_id, trusted_job);
    let again = connected(&world, "runner1-2", &LABELS, replica).await;
    assert_eq!(
        claim(db, again.id, replica, LEASE, &[]).await,
        None,
        "a trusted pool took the job of an untrusted run"
    );
}
