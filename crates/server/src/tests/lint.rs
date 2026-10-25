use std::fs;
use std::path::Path;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::access::{Answer, Credentials, Site, document, owner_credentials, request};

const LINT: &str = "/api/v1/lint";

fn fixture(path: &str) -> Value {
    serde_json::from_str(&document(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

async fn lint(site: &Site, credentials: &Credentials, body: Value) -> Answer {
    site.send(Method::POST, LINT, credentials, Some(body)).await
}

fn diagnostics(answer: &Answer) -> &Vec<Value> {
    assert_eq!(answer.status, StatusCode::OK, "{answer:?}");
    answer.body["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("no diagnostics in {answer:?}"))
}

#[tokio::test]
async fn a_source_with_an_unknown_key_answers_the_diagnostic_of_the_fixture() {
    let site = Site::new().await;
    let (_, stranger) = site.user("stranger").await;
    let asked = fixture("contracts/fixtures/api/lint-request.json");
    let answered = fixture("contracts/fixtures/api/lint-response.json");
    assert_eq!(
        asked["source"],
        document("contracts/fixtures/workflows/unknown-key.yml")
    );
    assert_eq!(
        answered["diagnostics"],
        json!([fixture("contracts/fixtures/diagnostics/unknown-key.json")])
    );
    let answer = lint(&site, &stranger, asked).await;
    assert_eq!(answer.status, StatusCode::OK, "{answer:?}");
    assert_eq!(answer.body, answered);

    let session = owner_credentials(&site).await;
    let signed = lint(
        &site,
        &session,
        fixture("contracts/fixtures/api/lint-request.json"),
    )
    .await;
    assert_eq!(signed.body, answered);
}

#[tokio::test]
async fn the_bosun_context_warns_under_github_workflows_and_nowhere_else() {
    let site = Site::new().await;
    let (_, caller) = site.user("dana").await;
    let source = document("contracts/fixtures/workflows/bosun-context-in-github-workflows.yml");
    let warned = lint(
        &site,
        &caller,
        json!({ "source": source, "path": ".github/workflows/x.yml" }),
    )
    .await;
    let found = diagnostics(&warned);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0]["code"], "extension");
    assert_eq!(found[0]["severity"], "warning");
    assert_eq!(found[0]["file"], ".github/workflows/x.yml");

    for body in [
        json!({ "source": source, "path": ".bosun/workflows/x.yml" }),
        json!({ "source": source, "path": null }),
        json!({ "source": source }),
    ] {
        let answer = lint(&site, &caller, body.clone()).await;
        assert!(diagnostics(&answer).is_empty(), "{body}: {answer:?}");
    }
}

#[tokio::test]
async fn every_workflow_fixture_is_answered_with_what_the_parser_finds_at_its_path() {
    let site = Site::new().await;
    let (_, caller) = site.user("dana").await;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/workflows");
    let mut cases: Vec<String> = fs::read_dir(fixtures)
        .expect("the workflow fixtures")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".yml"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let mut broken = 0;
    for case in &cases {
        let source = document(&format!("contracts/fixtures/workflows/{case}"));
        for place in [".bosun/workflows", ".github/workflows"] {
            let path = format!("{place}/{case}");
            let answer = lint(&site, &caller, json!({ "source": source, "path": path })).await;
            let expected =
                serde_json::to_value(bosun_workflow::lint(&path, &source)).expect("serialisable");
            assert_eq!(
                &Value::Array(diagnostics(&answer).clone()),
                &expected,
                "{path}"
            );
            broken += usize::from(has_error(&expected));
        }
    }
    assert!(
        broken > 0,
        "no fixture has an error, so none shows the 200 of a broken source"
    );
}

fn has_error(diagnostics: &Value) -> bool {
    diagnostics.as_array().is_some_and(|found| {
        found
            .iter()
            .any(|diagnostic| diagnostic["severity"] == "error")
    })
}

#[tokio::test]
async fn a_request_without_credentials_is_unauthenticated() {
    let site = Site::new().await;
    let response = site
        .router
        .clone()
        .oneshot(
            request(Method::POST, LINT)
                .header("content-type", "application/json")
                .body(Body::from(
                    fixture("contracts/fixtures/api/lint-request.json").to_string(),
                ))
                .expect("a request"),
        )
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("a body");
    let body: Value = serde_json::from_slice(&body).expect("the error envelope");
    assert_eq!(body["error"]["code"], "unauthenticated");
}

#[tokio::test]
async fn a_body_of_another_shape_is_invalid_input() {
    let site = Site::new().await;
    let (_, caller) = site.user("dana").await;
    for body in [
        json!({ "path": ".bosun/workflows/ci.yml" }),
        json!({ "source": "on: push\n", "file": ".bosun/workflows/ci.yml" }),
        json!({ "source": 7 }),
    ] {
        let answer = lint(&site, &caller, body.clone()).await;
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(answer.body["error"]["code"], "invalid_input", "{body}");
    }
}
