use std::fs;

use axum::http::{Method, StatusCode};
use bosun_blob::Store;
use serde_json::{Value, json};

use crate::{Harness, JobIdentity, block, identity, job, repository, run};

fn ids(who: JobIdentity) -> Value {
    json!({
        "workflow_run_backend_id": who.attempt.to_string(),
        "workflow_job_run_backend_id": who.execution.to_string(),
    })
}

fn with(mut base: Value, name: &str, value: Value) -> Value {
    base[name] = value;
    base
}

#[tokio::test]
async fn the_twirp_fixtures_round_trip_through_the_service() {
    let directory = repository().join("contracts/fixtures/twirp/artifact-service");
    let mut files: Vec<_> = fs::read_dir(&directory)
        .expect("the fixture directory")
        .map(|entry| entry.expect("an entry").path())
        .collect();
    files.sort();
    assert!(files.len() >= 8, "{files:?}");

    let harness = Harness::new().await;
    let token = harness.token(identity(1));
    for file in files {
        let case: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("a fixture")).expect("JSON");
        let method = case["method"].as_str().expect("a method");
        let reply = harness.twirp(Some(&token), method, &case["request"]).await;
        assert_eq!(
            u64::from(reply.status.as_u16()),
            case["status"].as_u64().expect("a status"),
            "{}",
            file.display()
        );
        assert_eq!(reply.json(), case["response"], "{}", file.display());
        if let Some(parts) = case["then_upload"].as_array() {
            let address = reply.json()["signed_upload_url"]
                .as_str()
                .expect("an address")
                .to_owned();
            let mut blocks = Vec::new();
            for (number, content) in (0u8..).zip(parts) {
                let content = content.as_str().expect("text");
                let staged = harness
                    .stage(&address, &block(number), content.as_bytes())
                    .await;
                assert_eq!(staged.status, StatusCode::CREATED, "{}", file.display());
                blocks.push(block(number));
            }
            let committed = harness.commit(&address, &blocks).await;
            assert_eq!(committed.status, StatusCode::CREATED, "{}", file.display());
        }
        if let Some(content) = case["then_download"].as_str() {
            let address = reply.json()["signed_url"]
                .as_str()
                .expect("an address")
                .to_owned();
            let read = harness.blob(Method::GET, &address, "", &[], b"").await;
            assert_eq!(read.status, StatusCode::OK, "{}", file.display());
            assert_eq!(read.body, content.as_bytes(), "{}", file.display());
        }
    }
}

#[tokio::test]
async fn a_token_of_another_run_can_neither_list_nor_create_artifacts_of_this_one() {
    let harness = Harness::new().await;
    let (mine, theirs) = (identity(1), identity(2));
    let token = harness.token(mine);
    let intruder = harness.token(theirs);
    harness.publish("secret", b"payload").await;

    let list = harness
        .twirp(Some(&intruder), "ListArtifacts", &ids(mine))
        .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN);
    assert_eq!(list.json()["code"], "permission_denied");

    let create = harness
        .twirp(
            Some(&intruder),
            "CreateArtifact",
            &with(ids(mine), "name", json!("planted")),
        )
        .await;
    assert_eq!(create.status, StatusCode::FORBIDDEN);

    let mixed = json!({
        "workflow_run_backend_id": mine.attempt.to_string(),
        "workflow_job_run_backend_id": theirs.execution.to_string(),
    });
    let mixed = harness
        .twirp(Some(&intruder), "ListArtifacts", &mixed)
        .await;
    assert_eq!(mixed.status, StatusCode::FORBIDDEN);

    let own = harness
        .twirp(Some(&intruder), "ListArtifacts", &ids(theirs))
        .await;
    assert_eq!(own.status, StatusCode::OK);
    assert_eq!(own.json(), json!({ "artifacts": [] }));

    for method in ["GetSignedArtifactURL", "DeleteArtifact"] {
        let request = with(ids(mine), "name", json!("secret"));
        let reply = harness.twirp(Some(&intruder), method, &request).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method}");
    }
    let finalize = with(with(ids(mine), "name", json!("secret")), "size", json!("1"));
    let reply = harness
        .twirp(Some(&intruder), "FinalizeArtifact", &finalize)
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let listed = harness
        .twirp(Some(&token), "ListArtifacts", &ids(mine))
        .await;
    assert_eq!(listed.json()["artifacts"].as_array().map(Vec::len), Some(1));
    let another_job = harness
        .twirp(
            Some(&token),
            "GetSignedArtifactURL",
            &with(ids(theirs), "name", json!("secret")),
        )
        .await;
    assert_eq!(another_job.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unauthenticated_and_malformed_twirp_requests_are_refused_without_a_panic() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let (other, _) = bosun_services::SigningKey::generate().expect("a key");
    let foreign = other
        .mint(
            who,
            crate::start(),
            crate::start() + chrono::Duration::hours(1),
        )
        .expect("a token");

    let request = with(ids(who), "name", json!("anything"));
    let anonymous = harness.twirp(None, "CreateArtifact", &request).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.json()["code"], "unauthenticated");
    for bearer in [foreign.as_str(), "garbage", ""] {
        let reply = harness
            .twirp(Some(bearer), "CreateArtifact", &request)
            .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{bearer:.10}");
    }

    let bad_ids = [
        json!({}),
        with(ids(who), "workflow_run_backend_id", json!("not-a-uuid")),
        with(ids(who), "workflow_job_run_backend_id", json!("")),
        with(ids(who), "name", json!("")),
        with(ids(who), "name", json!("tab\there")),
        with(ids(who), "name", json!("n".repeat(256))),
        with(
            with(ids(who), "name", json!("x")),
            "mime_type",
            json!("text/html; charset=x"),
        ),
        with(
            with(ids(who), "name", json!("x")),
            "expires_at",
            json!("tomorrow"),
        ),
        with(with(ids(who), "name", json!("x")), "name", json!(7)),
    ];
    for body in bad_ids {
        let reply = harness.twirp(Some(&token), "CreateArtifact", &body).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
    }
    for body in [
        with(with(ids(who), "name", json!("x")), "size", json!("-1")),
        with(with(ids(who), "name", json!("x")), "size", json!("many")),
        with(with(ids(who), "name", json!("x")), "size", json!(1.5)),
        with(
            with(with(ids(who), "name", json!("x")), "size", json!("1")),
            "hash",
            json!("md5:00"),
        ),
        with(
            with(with(ids(who), "name", json!("x")), "size", json!("1")),
            "hash",
            json!("sha256:ABC"),
        ),
    ] {
        let reply = harness.twirp(Some(&token), "FinalizeArtifact", &body).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
    }

    for raw in ["", "{", "[]", "null", "\"text\"", &"[".repeat(10_000)] {
        let request = axum::http::Request::post(
            "/twirp/github.actions.results.api.v1.ArtifactService/CreateArtifact",
        )
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(raw.to_owned()))
        .expect("a request");
        let reply = harness.send(request).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{raw:.20}");
        assert_eq!(reply.json()["code"], "malformed");
    }

    let unknown = harness.twirp(Some(&token), "Nothing", &json!({})).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.json()["code"], "bad_route");
    let protobuf = axum::http::Request::post(
        "/twirp/github.actions.results.api.v1.ArtifactService/ListArtifacts",
    )
    .header("content-type", "application/protobuf")
    .header("authorization", format!("Bearer {token}"))
    .body(axum::body::Body::empty())
    .expect("a request");
    assert_eq!(harness.send(protobuf).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_retried_create_replaces_the_pending_artifact_of_the_same_job() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let first = harness.create(&token, who, "retried").await;
    harness.stage(&first, &block(1), b"lost").await;
    harness.commit(&first, &[block(1)]).await;
    harness.stage(&first, &block(2), b"orphan").await;

    let second = harness.create(&token, who, "retried").await;
    assert_ne!(first, second);
    let store = harness.store().await;
    assert!(
        store
            .list("artifacts/")
            .await
            .expect("a listing")
            .is_empty()
    );
    assert!(store.list("blocks/").await.expect("a listing").is_empty());

    let typed = [("x-ms-blob-type", "BlockBlob")];
    for stale in [
        harness.stage(&first, &block(3), b"late").await,
        harness.blob(Method::PUT, &first, "", &typed, b"late").await,
    ] {
        assert!(
            matches!(stale.status.as_u16(), 403 | 404),
            "{}",
            stale.status
        );
    }

    let created = harness
        .blob(Method::PUT, &second, "", &typed, b"kept")
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    harness.finalize(&token, who, "retried", 4).await;
    let listed = harness
        .twirp(Some(&token), "ListArtifacts", &ids(who))
        .await;
    assert_eq!(listed.json()["artifacts"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn a_create_conflicts_with_a_finalised_artifact_and_with_a_pending_one_of_another_job() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let other = JobIdentity {
        attempt: run(1),
        execution: job(2),
    };
    let other_token = harness.token(other);
    harness.publish("done", b"payload").await;
    let held = harness.create(&token, who, "held").await;

    let create = |token: &str, who: JobIdentity, name: &'static str| {
        let harness = &harness;
        let body = with(ids(who), "name", json!(name));
        let token = token.to_owned();
        async move { harness.twirp(Some(&token), "CreateArtifact", &body).await }
    };
    let finalised = create(&token, who, "done").await;
    assert_eq!(finalised.status, StatusCode::CONFLICT);
    let foreign = create(&other_token, other, "held").await;
    assert_eq!(foreign.status, StatusCode::CONFLICT);
    assert_eq!(foreign.json()["code"], "already_exists");

    let staged = harness.stage(&held, &block(1), b"still mine").await;
    assert_eq!(staged.status, StatusCode::CREATED);
}

#[tokio::test]
async fn finalize_refuses_a_size_that_differs_from_the_stored_object() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let upload = harness.create(&token, who, "sized").await;
    let stored = harness
        .blob(
            Method::PUT,
            &upload,
            "",
            &[("x-ms-blob-type", "BlockBlob")],
            b"abc",
        )
        .await;
    assert_eq!(stored.status, StatusCode::CREATED);

    for declared in ["0", "2", "999999"] {
        let request = with(
            with(ids(who), "name", json!("sized")),
            "size",
            json!(declared),
        );
        let reply = harness
            .twirp(Some(&token), "FinalizeArtifact", &request)
            .await;
        assert_eq!(reply.status, StatusCode::PRECONDITION_FAILED, "{declared}");
        assert_eq!(reply.json()["code"], "failed_precondition");
    }
    harness.finalize(&token, who, "sized", 3).await;
}
