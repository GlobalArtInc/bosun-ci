use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Log, StepStarted};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::FakeAgent;
use bosun_testkit::forge::FakeForge;
use prometheus_parse::{Scrape, Value};
use reqwest::Method;
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::api::{Api, send, text};
use crate::{Launch, WAIT, client, owner, strings};

const SOURCE: &str = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    \
                      steps:\n      - run: echo one\n";

const EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8";

async fn scrape(metrics: SocketAddr) -> Scrape {
    let response = client()
        .get(format!("http://{metrics}/metrics"))
        .send()
        .await
        .expect("an answer at the metrics address");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], EXPOSITION);
    let body = response.text().await.expect("a body");
    Scrape::parse(body.lines().map(|line| Ok::<_, io::Error>(line.to_owned())))
        .expect("an exposition the parser reads")
}

fn counter(scrape: &Scrape, name: &str) -> f64 {
    scrape
        .samples
        .iter()
        .find(|sample| sample.metric == name)
        .and_then(|sample| match sample.value {
            Value::Counter(value) => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no counter `{name}` in {:#?}", scrape.samples))
}

#[tokio::test]
async fn the_metrics_answer_on_their_own_listener_and_the_public_listener_refuses_them() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;
    let metrics = server.metrics().await;
    assert_ne!(address, metrics);

    for method in [Method::GET, Method::POST] {
        let (status, body) =
            send(client().request(method.clone(), format!("http://{address}/metrics"))).await;
        assert_eq!(status, 404, "{method} {body}");
        assert_eq!(body["error"]["code"], "not_found");
        assert!(
            text(&body["error"]["message"]).contains("server.metrics_listen"),
            "{body}"
        );
    }

    let scraped = scrape(metrics).await;
    assert_eq!(
        counter(&scraped, "bosun_webhook_verification_failures_total"),
        0.0
    );
    for path in ["/", "/api/v1/me", "/healthz", "/metrics/"] {
        let (status, body) = send(client().get(format!("http://{metrics}{path}"))).await;
        assert_eq!(status, 404, "{path}: {body}");
        assert_eq!(body["error"]["code"], "not_found");
    }
    let (status, _) = send(client().post(format!("http://{metrics}/metrics"))).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_delivery_with_a_missing_or_wrong_signature_counts_as_a_verification_failure() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let mut server = Launch::serve(&database.url()).github(&forge).spawn();
    let address = server.listening().await;
    let metrics = server.metrics().await;
    let hooks = format!("http://{address}/hooks/github");
    let webhook = forge.webhook(
        "ping",
        &json!({ "zen": "Design for failure.", "hook_id": 1 }),
    );
    let wrong = format!("sha256={}", "0".repeat(64));

    for (signature, failures) in [(Some(wrong.as_str()), 1.0), (None, 2.0)] {
        let mut request = client()
            .post(&hooks)
            .header("content-type", "application/json")
            .header("x-github-event", webhook.event())
            .header("x-github-delivery", webhook.delivery())
            .body(webhook.body().to_vec());
        if let Some(signature) = signature {
            request = request.header("x-hub-signature-256", signature);
        }
        let (status, body) = send(request).await;
        assert_eq!(status, 401, "{body}");
        assert_eq!(
            counter(
                &scrape(metrics).await,
                "bosun_webhook_verification_failures_total"
            ),
            failures
        );
    }
    assert_eq!(webhook.send(&hooks).await, 202);
    let scraped = scrape(metrics).await;
    assert_eq!(
        counter(&scraped, "bosun_webhook_verification_failures_total"),
        2.0
    );
}

#[tokio::test]
async fn the_log_lines_an_agent_sends_count_once_on_the_replica_that_stores_them() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "roman").await;
    let api = Api::serve(Launch::serve(&database.url()), token).await;
    let mut other = Launch::serve(&database.url()).spawn();
    other.listening().await;
    let elsewhere = other.metrics().await;
    let metrics = api.metrics;

    let project = api.project().await;
    let workflow = api.workflow(&project, "ci", SOURCE).await;
    let (status, registration) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": false, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{registration}");
    let agent =
        FakeAgent::register(api.address, text(&registration["token"]), "runner1", &[]).await;
    let mut connection = agent.connect(api.address).await;
    let (status, dispatched) = api
        .post(
            &format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"])),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
    assert_eq!(status, 201, "{dispatched}");
    connection.idle();
    let assignment = connection.assignment().await;
    let execution = assignment.execution_id.clone();
    connection.send(Report::Accept(Accept {
        execution_id: execution.clone(),
    }));
    steps(&database, &execution).await;
    connection.send(Report::StepStarted(StepStarted {
        execution_id: execution.clone(),
        step_index: 0,
        started_at: None,
    }));
    let lines = b"one\ntwo\nthree".to_vec();
    for _ in 0..2 {
        connection.send(Report::Log(Log {
            execution_id: execution.clone(),
            step_index: 0,
            seq: 1,
            lines: lines.clone(),
        }));
        assert_eq!(connection.log_ack().await.seq, 1);
    }

    let scraped = scrape(metrics).await;
    assert_eq!(counter(&scraped, "bosun_log_lines_total"), 3.0);
    assert_eq!(
        counter(&scraped, "bosun_log_bytes_total"),
        (lines.len() + 1) as f64
    );
    let scraped = scrape(elsewhere).await;
    assert_eq!(counter(&scraped, "bosun_log_lines_total"), 0.0);
    assert_eq!(counter(&scraped, "bosun_log_bytes_total"), 0.0);
}

async fn steps(database: &TestDatabase, execution: &str) {
    let sql =
        format!("SELECT id::text AS value FROM bosun.step WHERE execution_id = '{execution}'");
    let deadline = Instant::now() + WAIT;
    while strings(database.connection(), &sql).await.is_empty() {
        assert!(Instant::now() < deadline, "the steps were not created");
        sleep(Duration::from_millis(50)).await;
    }
}
