use std::fmt::Write as _;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use bosun_testkit::forge::FakeForge;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedSender};

struct Received {
    headers: HeaderMap,
    body: Bytes,
}

async fn receive(
    State(sender): State<UnboundedSender<Received>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let _ = sender.send(Received { headers, body });
    StatusCode::ACCEPTED
}

fn header<'h>(received: &'h Received, name: &str) -> &'h str {
    received
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| panic!("the webhook has no {name}"))
}

fn hmac_sha256(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|error| panic!("HMAC takes any key: {error}"));
    mac.update(body);
    let mut hex = String::from("sha256=");
    for byte in mac.finalize().into_bytes() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[tokio::test]
async fn webhooks_are_signed_with_the_webhook_secret() {
    let forge = FakeForge::start().await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|error| panic!("the receiver cannot listen: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("the receiver has no address: {error}"));
    let router = Router::new()
        .route("/hooks/github", post(receive))
        .with_state(sender);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let url = format!("http://{address}/hooks/github");
    let payload = json!({ "ref": "refs/heads/main", "after": "0123456789abcdef", "zen": "Keep it logically awesome." });
    let webhook = forge.webhook("push", &payload);
    for _ in 0..2 {
        assert_eq!(webhook.send(&url).await, 202);
        let received = receiver
            .recv()
            .await
            .unwrap_or_else(|| panic!("the receiver got nothing"));
        assert_eq!(header(&received, "x-github-event"), "push");
        assert_eq!(header(&received, "x-github-delivery"), webhook.delivery());
        assert_eq!(header(&received, "content-type"), "application/json");
        let parsed: serde_json::Value = serde_json::from_slice(&received.body)
            .unwrap_or_else(|error| panic!("the body is not JSON: {error}"));
        assert_eq!(parsed, payload);
        assert_eq!(
            header(&received, "x-hub-signature-256"),
            hmac_sha256(forge.webhook_secret(), &received.body)
        );
        assert_ne!(
            header(&received, "x-hub-signature-256"),
            hmac_sha256("another secret", &received.body)
        );
    }
    let other = forge.webhook("push", &payload);
    assert_ne!(other.delivery(), webhook.delivery());
    server.abort();
}
