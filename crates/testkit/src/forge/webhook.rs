use ring::hmac;
use uuid::Uuid;

use super::git::hex;

pub struct Webhook {
    http: reqwest::Client,
    delivery: String,
    event: String,
    body: Vec<u8>,
    signature: String,
}

impl Webhook {
    pub(super) fn sign(
        http: reqwest::Client,
        secret: &str,
        event: &str,
        payload: &serde_json::Value,
    ) -> Webhook {
        let body = serde_json::to_vec(payload)
            .unwrap_or_else(|error| panic!("the webhook payload is not JSON: {error}"));
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let signature = format!("sha256={}", hex(hmac::sign(&key, &body).as_ref()));
        Webhook {
            http,
            delivery: Uuid::now_v7().to_string(),
            event: event.to_owned(),
            body,
            signature,
        }
    }

    pub fn delivery(&self) -> &str {
        &self.delivery
    }

    pub fn event(&self) -> &str {
        &self.event
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub async fn send(&self, url: &str) -> u16 {
        self.http
            .post(url)
            .header("content-type", "application/json")
            .header("user-agent", "GitHub-Hookshot/bosun-fake")
            .header("x-github-event", &self.event)
            .header("x-github-delivery", &self.delivery)
            .header("x-hub-signature-256", &self.signature)
            .body(self.body.clone())
            .send()
            .await
            .unwrap_or_else(|error| panic!("the fake forge could not deliver a webhook: {error}"))
            .status()
            .as_u16()
    }
}
