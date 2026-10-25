use std::fs;
use std::os::unix::fs::PermissionsExt;

use bosun_proto::agent::v1::{Capability, PROTOCOL};

use crate::server::{CREDENTIAL, FakeServer, TOKEN};
use crate::{Home, NAME, output};

#[tokio::test(flavor = "multi_thread")]
async fn register_writes_a_configuration_only_the_agent_user_can_read_and_run_connects_with_it() {
    let mut server = FakeServer::start().await;
    let home = Home::empty();
    let finished = output(home.register_command(&server, TOKEN, &["Deploy", "gpu"])).await;
    assert!(finished.status.success(), "{finished:#?}");
    let stdout = String::from_utf8_lossy(&finished.stdout);
    assert!(
        stdout.contains(&server.agent_id().to_string()),
        "the registration did not name the agent: {stdout}"
    );
    let registration = server.registration().await;
    assert_eq!(registration.token, TOKEN);
    assert_eq!(registration.name, NAME);
    assert_eq!(registration.labels, vec!["Deploy", "gpu"]);

    let mode = fs::metadata(home.config())
        .expect("the configuration exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let config: toml::Table = fs::read_to_string(home.config())
        .expect("the configuration is readable")
        .parse()
        .expect("the configuration is TOML");
    assert_eq!(config["url"].as_str(), Some(server.url().as_str()));
    assert_eq!(
        config["dir"].as_str(),
        Some(home.dir().to_string_lossy().as_ref())
    );
    assert_eq!(
        config["agent_id"].as_str(),
        Some(server.agent_id().to_string().as_str())
    );
    assert_eq!(config["credential"].as_str(), Some(CREDENTIAL));

    let again = output(home.register_command(&server, TOKEN, &[])).await;
    assert!(
        !again.status.success(),
        "a second registration overwrote the first"
    );
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("is registered already"),
        "{again:#?}"
    );

    let running = home.start();
    let mut stream = server.accept().await;
    assert_eq!(stream.hello.protocol, PROTOCOL);
    assert_eq!(stream.hello.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        stream.hello.capabilities,
        vec![i32::from(Capability::Shell)]
    );
    assert_eq!(stream.hello.labels, vec!["Deploy", "gpu"]);
    assert!(["Linux", "macOS"].contains(&stream.hello.os.as_str()));
    assert!(["X64", "ARM64"].contains(&stream.hello.arch.as_str()));
    assert!(!stream.hello.hostname.is_empty());
    assert!(stream.hello.executions.is_empty());
    stream.welcome(server.agent_id());
    stream.idle().await;
    let logs = running.stop().await;
    assert!(
        logs.iter().any(|line| line["message"] == "connected"),
        "{logs:#?}"
    );
    stream.closed().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_registration_names_the_reason_of_the_server_and_writes_nothing() {
    let server = FakeServer::start().await;
    let home = Home::empty();
    let finished = output(home.register_command(&server, "brt_expired", &[])).await;
    assert!(!finished.status.success(), "{finished:#?}");
    let stderr = String::from_utf8_lossy(&finished.stderr);
    assert!(
        stderr.contains("401 Unauthorized: the registration token expired"),
        "{stderr}"
    );
    assert!(!home.config().exists());
}
