use bosun_testkit::TestDatabase;
use reqwest::Version;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};

use crate::{Launch, client, get, h2_client, message};

#[tokio::test]
async fn a_route_or_method_that_does_not_exist_answers_with_the_error_envelope() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;

    let missing = get(address, "/api/v1/nothing").await;
    assert_eq!(missing.status(), 404);
    assert_eq!(
        missing.json::<Value>().await.expect("a JSON body"),
        json!({
            "error": {
                "code": "not_found",
                "message": "there is no GET /api/v1/nothing",
                "fields": [],
                "diagnostics": [],
            }
        })
    );

    let wrong_method = client()
        .post(format!("http://{address}/healthz"))
        .send()
        .await
        .expect("POST /healthz");
    assert_eq!(wrong_method.status(), 404);
    let body: Value = wrong_method.json().await.expect("a JSON body");
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(body["error"]["message"], "there is no POST /healthz");
}

#[tokio::test]
async fn every_request_carries_a_request_id_into_its_response_and_its_log_line() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;

    let given = client()
        .get(format!("http://{address}/healthz"))
        .header("x-request-id", "probe-7.a_b")
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(given.status(), 200);
    assert_eq!(given.headers()["x-request-id"], "probe-7.a_b");
    let line = server
        .expect("the request with the given id", |line| {
            message(line) == "request" && line["span"]["request_id"] == "probe-7.a_b"
        })
        .await;
    assert_eq!(line["status"], 200);
    assert_eq!(line["span"]["method"], "GET");
    assert_eq!(line["span"]["path"], "/healthz");
    assert!(line["duration_ms"].is_number(), "{line}");

    let refused = client()
        .get(format!("http://{address}/api/v1/nothing"))
        .header("x-request-id", "not an id")
        .send()
        .await
        .expect("GET /api/v1/nothing");
    let generated = refused.headers()["x-request-id"]
        .to_str()
        .expect("a textual id")
        .to_owned();
    assert_ne!(generated, "not an id");
    assert_eq!(generated.len(), 36, "{generated} is not a UUID");
    assert_eq!(&generated[14..15], "7", "{generated} is not a UUIDv7");
    let line = server
        .expect("the request with the generated id", |line| {
            message(line) == "request" && line["span"]["request_id"] == generated.as_str()
        })
        .await;
    assert_eq!(line["status"], 404);
}

#[tokio::test]
async fn one_listener_carries_http1_http2_and_grpc() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;

    let http1 = get(address, "/healthz").await;
    assert_eq!(http1.version(), Version::HTTP_11);
    assert_eq!(http1.status(), 200);
    assert_eq!(
        http1.json::<Value>().await.expect("a JSON body"),
        json!({ "status": "alive" })
    );

    let http2 = h2_client()
        .get(format!("http://{address}/healthz"))
        .send()
        .await
        .expect("GET /healthz over HTTP/2");
    assert_eq!(http2.version(), Version::HTTP_2);
    assert_eq!(http2.status(), 200);

    for (path, status) in [
        ("/bosun.agent.v1.Agent/Connect", "16"),
        ("/bosun.plugin.v1.Plugin/Connect", "12"),
    ] {
        let grpc = h2_client()
            .post(format!("http://{address}{path}"))
            .header(CONTENT_TYPE, "application/grpc")
            .header("te", "trailers")
            .send()
            .await
            .expect("a gRPC call");
        assert_eq!(grpc.version(), Version::HTTP_2);
        assert_eq!(grpc.status(), 200);
        assert_eq!(grpc.headers()["content-type"], "application/grpc");
        assert_eq!(grpc.headers()["grpc-status"], status, "{path}");
    }
}
