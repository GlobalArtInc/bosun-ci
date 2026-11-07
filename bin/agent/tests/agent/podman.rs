use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use bosun_proto::agent::v1::Capability;
use serde_json::Value;

use crate::Home;
use crate::server::FakeServer;

fn podman_printing(directory: &Path, info: &str) {
    let script = directory.join("podman");
    fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{info}'\n")).expect("a fake podman");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("an executable");
}

async fn capabilities_and_logs(path: &Path) -> (Vec<i32>, Vec<Value>) {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start_with_path(path);
    let mut stream = server.accept().await;
    let capabilities = stream.hello.capabilities.clone();
    stream.welcome(server.agent_id());
    stream.idle().await;
    let logs = running.stop().await;
    stream.closed().await;
    (capabilities, logs)
}

fn logged<'a>(logs: &'a [Value], start: &str) -> Option<&'a str> {
    logs.iter()
        .filter_map(|line| line["message"].as_str())
        .find(|message| message.starts_with(start))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_on_a_host_without_podman_starts_without_the_capability_and_logs_why() {
    let empty = tempfile::tempdir().expect("a PATH without podman");
    let (capabilities, logs) = capabilities_and_logs(empty.path()).await;
    assert_eq!(capabilities, vec![i32::from(Capability::Shell)]);
    assert_eq!(
        logged(&logs, "the agent runs without the podman capability"),
        Some(
            "the agent runs without the podman capability, which needs podman 4.4 or newer, \
             and podman is not on the PATH"
        )
    );

    let old = tempfile::tempdir().expect("a PATH with an old podman");
    podman_printing(
        old.path(),
        r#"{"host": {"cgroupVersion": "v1", "networkBackend": "cni"}, "version": {"Version": "3.4.4"}}"#,
    );
    let (capabilities, logs) = capabilities_and_logs(old.path()).await;
    assert_eq!(capabilities, vec![i32::from(Capability::Shell)]);
    assert_eq!(
        logged(&logs, "the agent runs without the podman capability"),
        Some(
            "the agent runs without the podman capability, which needs podman 4.4 or newer, \
             where it finds podman 3.4.4; cgroups v2, where podman reports v1; netavark, \
             where podman uses cni"
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_whose_podman_meets_the_prerequisites_advertises_it_next_to_shell() {
    let host = tempfile::tempdir().expect("a PATH with podman");
    podman_printing(
        host.path(),
        r#"{"host": {"cgroupVersion": "v2", "networkBackend": "netavark"}, "version": {"Version": "4.9.3"}}"#,
    );
    let (capabilities, logs) = capabilities_and_logs(host.path()).await;
    assert_eq!(
        capabilities,
        vec![i32::from(Capability::Shell), i32::from(Capability::Podman)]
    );
    assert_eq!(
        logged(&logs, "podman 4.9.3"),
        Some("podman 4.9.3 with cgroups v2 and netavark, so the agent holds podman")
    );
}
