use std::fs;

use bosun_testkit::TestDatabase;
use serde_json::Value;

use crate::{Launch, client, get, h2_client};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/install-agent.sh");

fn served(server: &str) -> String {
    fs::read_to_string(SCRIPT)
        .expect("deploy/install-agent.sh")
        .replace("__BOSUN_SERVER__", server)
        .replace("__BOSUN_VERSION__", env!("CARGO_PKG_VERSION"))
}

#[tokio::test]
async fn the_install_script_names_the_address_it_was_fetched_from_and_the_servers_version() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;

    let response = get(address, "/install-agent.sh").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/x-shellscript; charset=utf-8"
    );
    let text = response.text().await.expect("the script");
    assert_eq!(text, served(&format!("http://{address}")));
    assert!(
        text.contains(&format!("version='{}'\n", env!("CARGO_PKG_VERSION"))),
        "{text}"
    );

    let proxied = client()
        .get(format!("http://{address}/install-agent.sh"))
        .header("host", "ci.example.org")
        .header("x-forwarded-proto", "https")
        .send()
        .await
        .expect("GET /install-agent.sh through a proxy")
        .text()
        .await
        .expect("the script");
    assert_eq!(proxied, served("https://ci.example.org"));

    let over_h2 = h2_client()
        .get(format!("http://{address}/install-agent.sh"))
        .send()
        .await
        .expect("GET /install-agent.sh over HTTP/2")
        .text()
        .await
        .expect("the script");
    assert_eq!(over_h2, served(&format!("http://{address}")));

    let refused = client()
        .get(format!("http://{address}/install-agent.sh"))
        .header("host", "ci.example.org'$(id)")
        .send()
        .await
        .expect("GET /install-agent.sh with a host the script cannot hold");
    assert_eq!(refused.status(), 422);
    let body: Value = refused.json().await.expect("an error envelope");
    assert_eq!(body["error"]["code"], "invalid_input");
}

#[tokio::test]
async fn the_server_hands_out_the_agent_binaries_of_its_directory_without_a_token() {
    let database = TestDatabase::migrated().await;
    let binaries = tempfile::tempdir().expect("a directory of agent binaries");
    let agent: Vec<u8> = (0..300_000u32).map(|index| (index % 251) as u8).collect();
    fs::write(binaries.path().join("linux-x86_64"), &agent).expect("an agent binary");
    let mut server = Launch::serve(&database.url())
        .env(
            "BOSUN_AGENTS_BINARY_DIR",
            &binaries.path().to_string_lossy(),
        )
        .spawn();
    let address = server.listening().await;

    let response = get(address, "/_runtime/agent/linux-x86_64").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(
        response.headers()["content-length"],
        agent.len().to_string().as_str()
    );
    assert_eq!(response.bytes().await.expect("the binary"), agent);

    for (platform, message) in [
        (
            "linux-aarch64",
            format!(
                "the server has no agent for linux-aarch64: there is no {}",
                binaries.path().join("linux-aarch64").display()
            ),
        ),
        (
            "windows-x86_64",
            "there is no agent for windows-x86_64; the server has agents for linux-x86_64 and \
             linux-aarch64"
                .to_owned(),
        ),
    ] {
        let response = get(address, &format!("/_runtime/agent/{platform}")).await;
        assert_eq!(response.status(), 404, "{platform}");
        let body: Value = response.json().await.expect("an error envelope");
        assert_eq!(body["error"]["code"], "not_found", "{body}");
        assert_eq!(body["error"]["message"], message.as_str(), "{body}");
    }
}
