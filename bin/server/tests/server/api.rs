use std::fs;
use std::net::SocketAddr;
use std::path::Path;

use bosun_testkit::TestDatabase;
use reqwest::{Method, RequestBuilder};
use serde_json::{Value, json};

use crate::{Launch, Server, client, owner};

pub struct Api {
    pub address: SocketAddr,
    token: String,
    _server: Server,
}

impl Api {
    pub async fn start(database: &TestDatabase) -> Api {
        let token = owner(&database.url(), "roman").await;
        let mut server = Launch::serve(&database.url()).spawn();
        let address = server.listening().await;
        Api {
            address,
            token,
            _server: server,
        }
    }

    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        client()
            .request(method, format!("http://{}{path}", self.address))
            .bearer_auth(&self.token)
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        send(self.request(Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        send(self.request(Method::POST, path).json(body)).await
    }

    pub async fn put(&self, path: &str, body: &Value) -> (u16, Value) {
        send(self.request(Method::PUT, path).json(body)).await
    }

    pub async fn project(&self) -> Value {
        let (status, organisation) = self
            .post(
                "/api/v1/organisations",
                &json!({ "slug": "globalart", "name": "GlobalArt" }),
            )
            .await;
        assert_eq!(status, 201, "{organisation}");
        let (status, project) = self
            .post(
                "/api/v1/projects",
                &json!({ "organisation": "globalart", "slug": "ops", "name": "Operations" }),
            )
            .await;
        assert_eq!(status, 201, "{project}");
        project
    }

    pub async fn workflow(&self, project: &Value, name: &str, source: &str) -> Value {
        let (status, workflow) = self
            .post(
                &format!("/api/v1/projects/{}/workflows", text(&project["id"])),
                &json!({ "name": name, "source": source }),
            )
            .await;
        assert_eq!(status, 201, "{workflow}");
        workflow
    }
}

pub async fn send(request: RequestBuilder) -> (u16, Value) {
    let response = request.send().await.expect("a response");
    let status = response.status().as_u16();
    let body = response.text().await.expect("a body");
    let value = serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("a body that is not JSON ({error}): {body}"));
    (status, value)
}

pub fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{value} is not a string"))
}

pub fn fixture(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures")
        .join(path);
    fs::read_to_string(&full).unwrap_or_else(|error| panic!("{}: {error}", full.display()))
}
