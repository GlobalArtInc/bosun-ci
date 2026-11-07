use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use bosun_forge::GitHubApp;
use bosun_testkit::forge::{APP_ID, APP_KEY};
use bosun_types::status::{JobStatus, RunConclusion, RunStatus, StatusReason, WaitingReason};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use url::Url;

use super::{CHECKOUT, Harness, LEASE, at, quick, resolving, site};
use crate::config::Resolution;
use crate::forge::Forge;
use crate::resolver::Resolver;

const ONE_STEP: &str = "      - uses: actions/checkout@v4\n";

fn requested(harness: &Harness) -> usize {
    harness
        .forge
        .requests()
        .iter()
        .filter(|request| request.path.contains("/commits/"))
        .count()
}

async fn hanging_on(tag: &str) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port for the forge");
    let address = listener.local_addr().expect("an address");
    let hang = format!("/commits/{tag}");
    let router = Router::new().fallback(move |uri: Uri| {
        let hang = hang.clone();
        async move {
            if uri.path().ends_with(&hang) {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            (StatusCode::OK, "0123456789abcdef0123456789abcdef01234567").into_response()
        }
    });
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (
        Url::parse(&format!("http://{address}/")).expect("an address"),
        server,
    )
}

#[tokio::test]
async fn a_slow_forge_that_answers_every_request_in_time_fails_no_pass_and_the_lease_follows_it() {
    let harness = Harness::configured(Resolution {
        request_timeout: Duration::from_secs(1),
        ..quick()
    })
    .await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    let steps: String = (1..=8)
        .map(|number| {
            harness.tag("actions/checkout", &format!("v{number}"), &commit);
            format!("      - uses: actions/checkout@v{number}\n")
        })
        .collect();
    harness.forge.delay(Duration::from_millis(300));
    let run = harness.dispatch_unresolved(&steps).await;

    let resolver = Arc::clone(&harness.resolver);
    let pass = tokio::spawn(async move { resolver.once().await });
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let rival = bosun_db::resolutions::claim(harness.db(), 10, LEASE)
        .await
        .expect("a claim");
    assert!(
        rival.claimed.is_empty(),
        "the lease of a pass that was making progress ran out"
    );
    assert_eq!(pass.await.expect("a pass"), 1);
    assert_eq!(harness.status(run, "build").await, JobStatus::Queued);
    assert_eq!(requested(&harness), 8);
}

#[tokio::test]
async fn a_forge_that_answers_slowly_but_in_time_cannot_hold_a_job_beyond_the_give_up_limit() {
    let harness = Harness::configured(Resolution {
        give_up: Duration::from_millis(700),
        ..quick()
    })
    .await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    let steps: String = (1..=8)
        .map(|number| {
            harness.tag("actions/checkout", &format!("v{number}"), &commit);
            format!("      - uses: actions/checkout@v{number}\n")
        })
        .collect();
    harness.forge.delay(Duration::from_millis(300));
    let run = harness.dispatch_unresolved(&steps).await;

    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(
        harness.transitions(run, "build").await.last(),
        Some(&(
            JobStatus::Waiting,
            JobStatus::Completed,
            None,
            Some(StatusReason::ActionUnresolved)
        ))
    );
    assert!(requested(&harness) < 8, "{}", requested(&harness));
    let notices = harness.notices(run).await;
    assert_eq!(notices.len(), 1, "{notices:#?}");
    let message = &notices[0].diagnostic.message;
    assert!(
        message.starts_with("the action `actions/checkout@v")
            && message.contains("the forge did not finish within the limit"),
        "{message}"
    );
}

#[tokio::test]
async fn a_request_that_hangs_fails_with_the_reference_it_asked_about() {
    let (api, server) = hanging_on("v2").await;
    let harness = Harness::without_installation().await;
    let app = GitHubApp::new(APP_ID, APP_KEY.as_bytes(), &api).expect("the App's client");
    let resolver = Resolver::new(
        harness.db().clone(),
        Some(Arc::new(Forge::of(app))),
        site(),
        Resolution {
            request_timeout: Duration::from_secs(4),
            give_up: Duration::from_secs(3),
            ..quick()
        },
    );
    let run = harness
        .dispatch_unresolved(
            "      - uses: actions/checkout@v1\n      - run: make\n      - uses: \
             actions/checkout@v2\n      - uses: actions/checkout@v3\n",
        )
        .await;

    assert_eq!(resolver.once().await, 1);
    server.abort();
    assert_eq!(
        harness.transitions(run, "build").await.last(),
        Some(&(
            JobStatus::Waiting,
            JobStatus::Completed,
            None,
            Some(StatusReason::ActionUnresolved)
        ))
    );
    let notices = harness.notices(run).await;
    assert_eq!(notices.len(), 1, "{notices:#?}");
    assert_eq!(notices[0].diagnostic.path, at(2));
    assert!(
        notices[0].diagnostic.message.starts_with(
            "the action `actions/checkout@v2` cannot be resolved: the forge did not answer within \
             4s"
        ),
        "{}",
        notices[0].diagnostic.message
    );
}

#[tokio::test]
async fn a_job_whose_row_does_not_read_is_postponed_and_failed_at_the_give_up_limit() {
    let harness = Harness::configured(Resolution {
        backoff_min: Duration::from_secs(60),
        backoff_max: Duration::from_secs(120),
        ..quick()
    })
    .await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let healthy = harness.dispatch_unresolved(ONE_STEP).await;
    let poisoned = harness.dispatch_unresolved(ONE_STEP).await;
    let chained = harness
        .dispatch_source(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      \
             - uses: actions/checkout@v4\n  after:\n    runs-on: self-hosted\n    needs: build\n    \
             steps:\n      - run: make\n",
        )
        .await;
    harness
        .db()
        .execute_unprepared(&format!(
            "UPDATE bosun.run SET ir = '{{\"jobs\": 7}}'::jsonb WHERE id IN ('{poisoned}', '{chained}')"
        ))
        .await
        .expect("a corrupted IR");

    assert_eq!(harness.resolver.once().await, 1);
    assert_eq!(harness.status(healthy, "build").await, JobStatus::Queued);
    assert_eq!(
        harness.waiting(poisoned, "build").await,
        (JobStatus::Waiting, Some(WaitingReason::Resolving))
    );
    assert_eq!(harness.attempts(poisoned, "build").await, 1);
    let next = bosun_db::resolutions::claim(harness.db(), 10, LEASE)
        .await
        .expect("a claim");
    assert!(
        next.claimed.is_empty() && next.unreadable.is_empty(),
        "the postponed job came back at once: {next:#?}"
    );
    assert_eq!(requested(&harness), 1);

    harness
        .db()
        .execute_unprepared("UPDATE bosun.job SET resolve_after = NULL")
        .await
        .expect("jobs due at once");
    let impatient = resolving(
        &harness.world,
        &harness.forge,
        Resolution {
            give_up: Duration::from_millis(1),
            ..quick()
        },
    );
    assert_eq!(impatient.once().await, 0);
    for run in [poisoned, chained] {
        assert_eq!(
            harness.transitions(run, "build").await.last(),
            Some(&(
                JobStatus::Waiting,
                JobStatus::Completed,
                None,
                Some(StatusReason::ActionUnresolved)
            ))
        );
        let notices = harness.notices(run).await;
        assert_eq!(notices.len(), 1, "{notices:#?}");
        let notice = &notices[0].diagnostic;
        assert_eq!(notice.code, "action_unresolved");
        assert!(
            notice.message.contains("cannot read the record of the job")
                && !notice.message.contains("jobs"),
            "{}",
            notice.message
        );
    }
    let planned = harness.world.project(poisoned).await;
    let run_row = planned.run.as_ref().expect("the run");
    assert_eq!(
        (run_row.status, run_row.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
    assert_eq!(harness.status(chained, "after").await, JobStatus::Pending);
    assert_eq!(requested(&harness), 1);
}

#[tokio::test]
async fn the_backoff_starts_at_its_minimum_doubles_and_stops_at_its_ceiling() {
    let harness = Harness::new().await;
    let run = harness.dispatch_unresolved(ONE_STEP).await;
    let (job, _) = harness.world.project(run).await.job("build");
    let token = Uuid::now_v7();
    let transaction = harness.db().begin().await.expect("a transaction");
    let mut delays = Vec::new();
    for attempts in [0, 1, 2, 3, 4, 5, 6, 7, 100_000] {
        transaction
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE bosun.job SET resolve_claim = $2, resolve_attempts = $3 WHERE id = $1",
                [Uuid::from(job).into(), token.into(), attempts.into()],
            ))
            .await
            .expect("a claim");
        bosun_db::resolutions::retry(
            &transaction,
            job,
            token,
            Duration::from_secs(1),
            Duration::from_secs(60),
        )
        .await
        .expect("a retry");
        let found = transaction
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT extract(epoch FROM resolve_after - now())::float8 AS delay \
                 FROM bosun.job WHERE id = $1",
                [Uuid::from(job).into()],
            ))
            .await
            .expect("a delay")
            .expect("the job");
        delays.push(found.try_get::<f64>("", "delay").expect("a delay"));
    }
    transaction.rollback().await.expect("a rollback");
    assert_eq!(delays, [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 60.0, 60.0, 60.0]);
}
