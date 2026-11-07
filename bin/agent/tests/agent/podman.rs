use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use bosun_proto::agent::v1::Capability;
use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use serde_json::Value;

use crate::server::FakeServer;
use crate::{Home, assignment, step};

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
        r#"{"host": {"cgroupVersion": "v1", "networkBackend": "cni", "security": {"rootless": true}}, "version": {"Version": "3.4.4"}}"#,
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
        r#"{"host": {"cgroupVersion": "v2", "networkBackend": "netavark", "security": {"rootless": true}}, "version": {"Version": "4.9.3"}}"#,
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

const USABLE: &str = r#"{"host": {"cgroupVersion": "v2", "networkBackend": "netavark", "security": {"rootless": true}}, "version": {"Version": "4.9.3"}}"#;

const OLD: &str = r#"{"host": {"cgroupVersion": "v1", "networkBackend": "cni", "security": {"rootless": true}}, "version": {"Version": "3.4.4"}}"#;

fn podman_recording(directory: &Path, info: &str, failing: &str) {
    let script = directory.join("podman");
    let record = directory.join("calls");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1 $2\" in\n  \"info --format\") printf '%s\\n' '{info}' ;;\n  \
             \"ps --all\"|\"volume ls\"|\"network ls\") echo leftover ;;\nesac\n\
             if [ \"$1\" = '{failing}' ]; then echo 'Error: refused' >&2; exit 125; fi\n",
            record.display()
        ),
    )
    .expect("a fake podman");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("an executable");
}

fn calls(directory: &Path) -> Vec<String> {
    fs::read_to_string(directory.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn leftover(home: &Home) -> std::path::PathBuf {
    let auth = home.dir().join("podman/0192f3a4/auth.json");
    fs::create_dir_all(auth.parent().expect("a directory")).expect("a state directory");
    fs::write(&auth, "{}").expect("an auth file");
    auth
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_removes_what_a_killed_agent_left_before_it_takes_a_job_and_logs_a_removal_that_fails()
 {
    let host = tempfile::tempdir().expect("a PATH with podman");
    podman_recording(host.path(), USABLE, "rm");
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let auth = leftover(&home);
    let running = home.start_with_path(host.path());
    let mut stream = server.accept().await;
    assert!(!auth.exists());
    stream.welcome(server.agent_id());
    stream.barrier().await;
    assert!(
        stream
            .received
            .iter()
            .all(|message| !matches!(message, Inbound::Idle(_))),
        "{:#?}",
        stream.received
    );
    let logs = running.stop().await;
    stream.closed().await;
    assert!(
        logs.iter().any(|line| line["level"] == "ERROR"
            && line["message"]
                .as_str()
                .is_some_and(|text| text.contains("takes no job until it can"))),
        "{logs:#?}"
    );

    let made = calls(host.path());
    for expected in [
        "rm --force --volumes --time 0 leftover",
        "volume rm --force leftover",
        "network rm --force leftover",
    ] {
        assert!(
            made.iter().any(|call| call == expected),
            "{expected}: {made:?}"
        );
    }
    let mut owners = Vec::new();
    for listing in ["ps --all", "volume ls", "network ls"] {
        let call = made
            .iter()
            .find(|call| call.starts_with(listing))
            .unwrap_or_else(|| panic!("{listing}: {made:?}"));
        let owner = call
            .split_once("--quiet --filter label=bosun.execution --filter label=bosun.agent=")
            .map(|(_, owner)| owner)
            .unwrap_or_else(|| panic!("{call}"));
        assert!(
            owner.len() == 16 && owner.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{call}"
        );
        owners.push(owner.to_owned());
    }
    assert!(owners.iter().all(|owner| *owner == owners[0]), "{owners:?}");
    let warned = logs
        .iter()
        .find(|line| {
            line["message"]
                .as_str()
                .is_some_and(|text| text.starts_with("a container of an earlier job"))
        })
        .expect("the failed removal is logged");
    assert_eq!(warned["level"], "WARN");
    assert!(
        warned["message"]
            .as_str()
            .is_some_and(|text| text.contains("refused"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_whose_podman_is_unusable_does_not_sweep_and_says_nothing_of_it() {
    let host = tempfile::tempdir().expect("a PATH with an old podman");
    podman_recording(host.path(), OLD, "");
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let auth = leftover(&home);
    let running = home.start_with_path(host.path());
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;
    let logs = running.stop().await;
    stream.closed().await;

    assert!(auth.exists());
    assert_eq!(calls(host.path()).len(), 1);
    assert!(calls(host.path())[0].starts_with("info"));
    assert!(
        logs.iter().all(|line| !line["message"]
            .as_str()
            .unwrap_or_default()
            .contains("earlier job")),
        "{logs:#?}"
    );
}

fn podman_stuck(directory: &Path) {
    let script = directory.join("podman");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  \"info --format\") printf '%s\\n' '{USABLE}' ;;\n  \
             \"ps --all\"|\"volume ls\"|\"network ls\") echo leftover ;;\nesac\n\
             if [ \"$1\" = rm ] && [ -e '{}' ]; then echo 'Error: refused' >&2; exit 125; fi\n",
            directory.join("stuck").display()
        ),
    )
    .expect("a fake podman");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("an executable");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_with_a_mark_it_cannot_clear_asks_for_no_job_refuses_an_offer_and_asks_again_once_it_can()
 {
    let host = tempfile::tempdir().expect("a PATH with podman");
    podman_stuck(host.path());
    let stuck = host.path().join("stuck");
    fs::write(&stuck, "").expect("a flag");
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let mut running = home.start_with_path(host.path());
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.barrier().await;
    assert!(home.dir().join("podman/dirty").exists());

    let offered = assignment(vec![step("echo hello")]);
    let execution = offered.execution_id.clone();
    stream.send(Outgoing::Assignment(offered));
    let reject = stream
        .until("a Reject", |message| match message {
            Inbound::Reject(reject) => Some(reject.clone()),
            Inbound::Accept(accept) => {
                panic!("a job was accepted while the mark was set: {accept:?}")
            }
            _ => None,
        })
        .await;
    assert_eq!(reject.execution_id, execution);
    assert!(
        reject.reason.contains("takes no job until it can"),
        "{}",
        reject.reason
    );
    stream.barrier().await;
    assert!(
        stream
            .received
            .iter()
            .all(|message| !matches!(message, Inbound::Idle(_))),
        "{:#?}",
        stream.received
    );

    let failed = running.logged("the agent still takes no job").await;
    assert_eq!(failed["level"], "ERROR");
    assert!(failed.to_string().contains("refused"), "{failed}");
    fs::remove_file(&stuck).expect("the flag goes");
    stream.idle().await;
    assert!(!home.dir().join("podman/dirty").exists());
    running.stop().await;
    stream.closed().await;
}
